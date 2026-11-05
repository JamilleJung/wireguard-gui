//! Container parsing for the two bulk import paths: backup ZIP archives
//! (restore and the backup list) and QR-code images.
//!
//! Nothing here touches the filesystem or the helper. Callers open the file
//! (with the safe-open rules) and hand over a reader or the bytes. The only
//! dependencies are `config`, `validation` and `secrets`, so the fuzz targets
//! in `fuzz/` and `tests/prop_import.rs` compile this file directly with
//! `#[path]`, like the other pure modules.
//!
//! Limits, each enforced before the work or allocation it bounds:
//!
//! * a backup archive is at most [`MAX_BACKUP_BYTES`];
//! * its end records may declare at most [`MAX_ARCHIVE_ENTRIES`] entries, and
//!   the parsed central directory may hold no more than that, without
//!   duplicate names;
//! * restore reads at most [`MAX_BACKUP_CONFIGS`] tunnel configs, each at most
//!   [`MAX_CONFIG_BYTES`] and together at most [`MAX_TOTAL_CONFIG_BYTES`] once
//!   decompressed, so the retained configs never exceed that total;
//! * a QR image is at most [`MAX_QR_DIMENSION`] pixels per side and the image
//!   decoder may allocate at most [`MAX_QR_DECODE_ALLOC`] bytes.

use std::io::{Read, Seek, SeekFrom};

use crate::{config, secrets, validation};

/// Largest backup archive accepted for restore or listing.
pub const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;
/// Most archive entries a backup may declare or contain.
pub const MAX_ARCHIVE_ENTRIES: u64 = 4096;
/// Most tunnel configs one restore reads.
pub const MAX_BACKUP_CONFIGS: usize = 1024;
/// Largest single decompressed tunnel config.
pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
/// Largest total of decompressed tunnel configs in one restore.
pub const MAX_TOTAL_CONFIG_BYTES: u64 = 16 * MAX_CONFIG_BYTES;
/// Largest QR image width and height in pixels.
pub const MAX_QR_DIMENSION: u32 = 4096;
/// Most bytes the image decoder may allocate for one QR image.
pub const MAX_QR_DECODE_ALLOC: u64 = 128 * 1024 * 1024;

// ---------------------------------------------------------------------------
// Backup archives
// ---------------------------------------------------------------------------

// ZIP metadata is eagerly allocated by ZipArchive::new. Read only the bounded
// end records first so a huge declared/actual entry count cannot allocate that
// metadata before the archive's entry limit is enforced.
pub fn preflight_zip_directory(reader: &mut (impl Read + Seek), length: u64) -> Result<(), String> {
    if !(22..=MAX_BACKUP_BYTES).contains(&length) {
        return Err("Invalid or oversized ZIP file.".into());
    }
    // zip may fall back among EOCD64 records anywhere before a locator. Scan
    // every structurally possible record with a bounded overlapping buffer,
    // rather than trusting just the locator's optimistic offset.
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut consumed = 0u64;
    let mut carry = Vec::new();
    while consumed < length {
        let count = (length - consumed).min(64 * 1024) as usize;
        let base = consumed.saturating_sub(carry.len() as u64);
        let mut bytes = carry;
        let old_len = bytes.len();
        bytes.resize(old_len + count, 0);
        reader
            .read_exact(&mut bytes[old_len..])
            .map_err(|e| e.to_string())?;
        consumed += count as u64;
        for index in 0..bytes.len().saturating_sub(55) {
            if !bytes[index..].starts_with(b"PK\x06\x06") {
                continue;
            }
            let size = u64::from_le_bytes(bytes[index + 4..index + 12].try_into().unwrap());
            if size < 40
                || base
                    .checked_add(index as u64)
                    .and_then(|offset| offset.checked_add(12))
                    .and_then(|offset| offset.checked_add(size))
                    .is_none_or(|end| end > length)
            {
                continue;
            }
            for offset in [24, 32] {
                if u64::from_le_bytes(
                    bytes[index + offset..index + offset + 8]
                        .try_into()
                        .unwrap(),
                ) > MAX_ARCHIVE_ENTRIES
                {
                    return Err("Backup contains too many archive entries.".into());
                }
            }
        }
        carry = bytes[bytes.len().saturating_sub(55)..].to_vec();
    }
    // zip9 searches a bounded tail for *all* EOCD candidates and accepts
    // trailing garbage (comment end <= EOF). Use a wider bound and inspect
    // every candidate: a small fake footer must not mask a huge earlier one.
    let tail_len = length.min(128 * 1024) as usize;
    reader
        .seek(SeekFrom::Start(length - tail_len as u64))
        .map_err(|e| e.to_string())?;
    let mut tail = vec![0; tail_len];
    reader.read_exact(&mut tail).map_err(|e| e.to_string())?;
    let mut found = false;
    for index in 0..=tail.len() - 22 {
        if !tail[index..].starts_with(b"PK\x05\x06")
            || index + 22 + u16::from_le_bytes([tail[index + 20], tail[index + 21]]) as usize
                > tail.len()
        {
            continue;
        }
        found = true;
        let disk_entries = u16::from_le_bytes([tail[index + 8], tail[index + 9]]);
        let entries = u16::from_le_bytes([tail[index + 10], tail[index + 11]]);
        let zip64_locator = index >= 20 && tail[index - 20..].starts_with(b"PK\x06\x07");
        if entries == u16::MAX || disk_entries == u16::MAX {
            if !zip64_locator {
                return Err("ZIP64 end locator is missing.".into());
            }
        } else if u64::from(entries) > MAX_ARCHIVE_ENTRIES
            || u64::from(disk_entries) > MAX_ARCHIVE_ENTRIES
        {
            return Err("Backup contains too many archive entries.".into());
        }
    }
    if !found {
        return Err("ZIP end record is missing or inconsistent.".into());
    }
    Ok(())
}

/// Open a backup archive: size limit, end-record preflight, then the central
/// directory, refusing duplicate entry names and too many entries.
pub fn open_backup_archive<R: Read + Seek>(mut reader: R) -> Result<zip::ZipArchive<R>, String> {
    let length = reader.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    if length > MAX_BACKUP_BYTES {
        return Err("Backup exceeds 64 MiB.".into());
    }
    preflight_zip_directory(&mut reader, length)?;
    reader.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    if zip.has_duplicate_names() {
        return Err("Backup contains duplicate archive filenames.".into());
    }
    if zip.len() as u64 > MAX_ARCHIVE_ENTRIES {
        return Err("Backup contains too many archive entries.".into());
    }
    Ok(zip)
}

/// Number of archive entries shown in the backup list: 0 for an archive that
/// [`open_backup_archive`] refuses.
pub fn backup_entry_count<R: Read + Seek>(reader: R) -> usize {
    open_backup_archive(reader).map_or(0, |zip| zip.len())
}

// Validate the whole archive before the first privileged write. Bound both
// declared and actual uncompressed sizes so a corrupt backup cannot exhaust
// memory or overwrite earlier tunnels before a later invalid entry is found.
/// Every `NAME.conf` file entry of a backup as `(NAME, contents)`, in archive
/// order. Other entries are ignored. The archive is refused as a whole if any
/// config has an invalid or repeated name, is too large, fails validation or
/// runs script hooks, or if none is present.
pub fn backup_configs<R: Read + Seek>(reader: R) -> Result<Vec<(String, String)>, String> {
    let mut zip = open_backup_archive(reader)?;
    let mut configs = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut total = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let raw = entry.name().map_err(|e| e.to_string())?.into_owned();
        let Some(name) = raw.strip_suffix(".conf") else {
            continue;
        };
        validation::validate_tunnel_name(name)?;
        if name.trim() != name || !names.insert(name.to_string()) {
            return Err(format!("Invalid or duplicate tunnel name in backup: {raw}"));
        }
        if entry.size() > MAX_CONFIG_BYTES || configs.len() >= MAX_BACKUP_CONFIGS {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        let mut content = String::new();
        entry
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        total += content.len() as u64;
        if content.len() as u64 > MAX_CONFIG_BYTES || total > MAX_TOTAL_CONFIG_BYTES {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        config::validate_basic_wireguard_config(&content)?;
        if secrets::config_runs_scripts(&content) {
            return Err(format!(
                "Script hooks are not allowed in backup tunnel {name}."
            ));
        }
        configs.push((name.to_string(), content));
    }
    if configs.is_empty() {
        return Err("The backup contained no tunnel configs.".into());
    }
    Ok(configs)
}

// ---------------------------------------------------------------------------
// QR images
// ---------------------------------------------------------------------------

/// Decode image file bytes (PNG or JPEG) to grayscale within the QR bounds.
pub fn decode_qr_image(bytes: &[u8]) -> Result<image::GrayImage, String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    // Bounds cover a full-size screenshot while limiting both decoder buffers
    // and rqrr's additional per-pixel working memory.
    limits.max_image_width = Some(MAX_QR_DIMENSION);
    limits.max_image_height = Some(MAX_QR_DIMENSION);
    limits.max_alloc = Some(MAX_QR_DECODE_ALLOC);
    reader.limits(limits);
    Ok(reader
        .decode()
        .map_err(|e| format!("Couldn't decode bounded QR image: {e}"))?
        .to_luma8())
}

/// Find the QR codes in a grayscale image and select the tunnel config they
/// carry (see [`select_qr_config`]).
pub fn qr_config_from_image(image: image::GrayImage) -> Result<String, String> {
    let mut prepared = rqrr::PreparedImage::prepare(image);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("No QR code found in the image.".into());
    }
    select_qr_config(grids.iter().map(|grid| {
        grid.decode()
            .map(|(_, content)| content)
            .map_err(|e| e.to_string())
    }))
}

/// Decode a QR-code image file's bytes into the tunnel config it carries.
pub fn decode_qr_bytes(bytes: &[u8]) -> Result<String, String> {
    qr_config_from_image(decode_qr_image(bytes)?)
}

/// Pick the one tunnel config among decoded QR grids. Grids that failed to
/// decode, hold no valid config, or hold a config with script hooks are
/// skipped; repeats of the same config count once; two different configs
/// are refused as ambiguous.
pub fn select_qr_config(
    results: impl IntoIterator<Item = Result<String, String>>,
) -> Result<String, String> {
    let mut configs = Vec::new();
    for result in results {
        let Ok(content) = result else {
            continue;
        };
        if config::validate_basic_wireguard_config(&content).is_ok()
            && !secrets::config_runs_scripts(&content)
            && !configs.contains(&content)
        {
            configs.push(content);
            if configs.len() > 1 {
                return Err("Image contains multiple different WireGuard configs; use an image with one tunnel.".into());
            }
        }
    }
    configs
        .pop()
        .ok_or("No valid WireGuard config QR code found in the image.".into())
}
