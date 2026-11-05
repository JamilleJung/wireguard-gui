//! Property tests for the container import parsers (`src/import_parse.rs`):
//! backup ZIP preflight and restore parsing, and QR config selection and the
//! bounded image decode. The modules are compiled straight from the source
//! tree, like the other `prop_*` tests.

#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../src/import_parse.rs"]
mod import_parse;
mod prop_support;
#[allow(dead_code)]
#[path = "../src/secrets.rs"]
mod secrets;
#[allow(dead_code)]
#[path = "../src/validation.rs"]
mod validation;

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use import_parse::{
    MAX_ARCHIVE_ENTRIES, MAX_BACKUP_BYTES, MAX_BACKUP_CONFIGS, MAX_CONFIG_BYTES, MAX_QR_DIMENSION,
    MAX_TOTAL_CONFIG_BYTES, backup_configs, backup_entry_count, decode_qr_bytes, decode_qr_image,
    open_backup_archive, preflight_zip_directory, select_qr_config,
};
use prop_support::{Rng, check, check_scaled, wg_key};

// ---------------------------------------------------------------- input builders

fn valid_config(rng: &mut Rng) -> String {
    let mut text = format!("[Interface]\nPrivateKey = {}\n", wg_key(rng));
    if rng.chance(1, 2) {
        text.push_str(&format!("Address = 10.{}.0.1/24\n", rng.below(256)));
    }
    if rng.chance(1, 3) {
        text.push_str(&format!("ListenPort = {}\n", 1 + rng.below(65535)));
    }
    for _ in 0..rng.below(3) {
        text.push_str(&format!(
            "\n[Peer]\nPublicKey = {}\nAllowedIPs = 10.{}.{}.0/24\n",
            wg_key(rng),
            rng.below(256),
            rng.below(256)
        ));
        if rng.chance(1, 2) {
            text.push_str(&format!(
                "Endpoint = vpn.example.com:{}\n",
                1 + rng.below(65535)
            ));
        }
    }
    if rng.chance(1, 4) {
        text.push_str("# restored from a backup\n");
    }
    assert!(
        config::validate_basic_wireguard_config(&text).is_ok(),
        "builder made an invalid config: {text:?}"
    );
    text
}

fn tunnel_name(rng: &mut Rng) -> String {
    const FIRST: &[u8] = b"abcwxyzABCWXYZ0189";
    const REST: &[u8] = b"abcxyzABCXYZ0189_-.";
    loop {
        let mut name = String::from(*rng.pick(FIRST) as char);
        for _ in 0..rng.below(15) {
            name.push(*rng.pick(REST) as char);
        }
        if validation::validate_tunnel_name(&name).is_ok() {
            return name;
        }
    }
}

/// Distinct tunnel names. Sometimes includes a case variant of another
/// name: tunnel names are case-sensitive, like the config files.
fn tunnel_names(rng: &mut Rng, count: usize) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    while names.len() < count {
        let name = match names.last() {
            Some(previous) if rng.chance(1, 4) => rng.recase(previous),
            _ => tunnel_name(rng),
        };
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

#[derive(Clone)]
enum Entry {
    File(String, Vec<u8>),
    Dir(String),
}

fn options(rng: &mut Rng) -> zip::write::SimpleFileOptions {
    let method = if rng.chance(1, 2) {
        zip::CompressionMethod::Stored
    } else {
        zip::CompressionMethod::Deflated
    };
    zip::write::SimpleFileOptions::default().compression_method(method)
}

fn write_zip(rng: &mut Rng, entries: &[Entry]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        match entry {
            Entry::File(name, content) => {
                zip.start_file(name.as_str(), options(rng)).unwrap();
                zip.write_all(content).unwrap();
            }
            Entry::Dir(name) => zip.add_directory(name.as_str(), options(rng)).unwrap(),
        }
    }
    zip.finish().unwrap().into_inner()
}

fn conf(name: &str, content: &str) -> Entry {
    Entry::File(format!("{name}.conf"), content.as_bytes().to_vec())
}

/// Entries restore must skip: other files and directories.
fn extra_entry(rng: &mut Rng, index: usize) -> Entry {
    match rng.below(5) {
        0 => Entry::File(format!("README-{index}.txt"), b"not a config".to_vec()),
        1 => Entry::File(format!("wg{index}.conf.bak"), b"[Interface]".to_vec()),
        2 => Entry::File(format!("upper{index}.CONF"), b"ignored".to_vec()),
        3 => Entry::Dir(format!("folder{index}.conf/")),
        _ => Entry::Dir(format!("notes{index}/")),
    }
}

/// A valid backup: its entries in archive order and the configs restore
/// should return, in the same order.
fn valid_backup(rng: &mut Rng) -> (Vec<Entry>, Vec<(String, String)>) {
    let count = 1 + rng.below(6);
    let configs: Vec<(String, String)> = tunnel_names(rng, count)
        .into_iter()
        .map(|name| {
            let content = valid_config(rng);
            (name, content)
        })
        .collect();
    let mut entries: Vec<Entry> = configs.iter().map(|(n, c)| conf(n, c)).collect();
    for index in 0..rng.below(4) {
        let at = rng.below(entries.len() + 1);
        let extra = extra_entry(rng, index);
        entries.insert(at, extra);
    }
    (entries, configs)
}

fn padded_config(rng: &mut Rng, len: usize) -> String {
    let mut text = format!("[Interface]\nPrivateKey = {}\n#", wg_key(rng));
    assert!(len > text.len());
    let pad = len - text.len() - 1;
    text.extend(std::iter::repeat_n('x', pad));
    text.push('\n');
    assert_eq!(text.len(), len);
    text
}

fn last_end_record(archive: &[u8]) -> usize {
    (0..=archive.len() - 22)
        .rev()
        .find(|&i| archive[i..].starts_with(b"PK\x05\x06"))
        .expect("archive has an end record")
}

/// A reader of `len` zero bytes that stores none of them.
struct Zeros {
    len: u64,
    pos: u64,
}

impl Read for Zeros {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = (self.len.saturating_sub(self.pos)).min(buf.len() as u64) as usize;
        buf[..n].fill(0);
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Zeros {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.pos = match pos {
            SeekFrom::Start(n) => n,
            SeekFrom::End(n) => self.len.saturating_add_signed(n),
            SeekFrom::Current(n) => self.pos.saturating_add_signed(n),
        };
        Ok(self.pos)
    }
}

// ---------------------------------------------------------------- backup archives

#[test]
fn generated_backups_round_trip_through_preflight() {
    check("generated_backups_round_trip_through_preflight", |rng| {
        let (entries, configs) = valid_backup(rng);
        let mut archive = write_zip(rng, &entries);
        if rng.chance(1, 4) {
            // Trailing data after the end record is tolerated (zip does too).
            let junk: Vec<u8> = (0..rng.below(64))
                .map(|_| b'a' + rng.below(26) as u8)
                .collect();
            archive.extend_from_slice(&junk);
        }
        preflight_zip_directory(&mut Cursor::new(&archive), archive.len() as u64).unwrap();
        assert_eq!(backup_entry_count(Cursor::new(&archive)), entries.len());
        assert_eq!(
            open_backup_archive(Cursor::new(&archive)).unwrap().len(),
            entries.len()
        );
        assert_eq!(backup_configs(Cursor::new(&archive)).unwrap(), configs);
    });
}

/// File names that must never become a tunnel, whatever else the archive holds.
fn unsafe_entry_name(rng: &mut Rng) -> String {
    let name = tunnel_name(rng);
    let short = &name[..name.len().min(6)];
    match rng.below(16) {
        0 => format!("../{name}.conf"),
        1 => format!("/{name}.conf"),
        2 => format!("/etc/wireguard/{name}.conf"),
        3 => format!("nested/{name}.conf"),
        4 => format!("{name}/../{name}.conf"),
        5 => format!("..\\{name}.conf"),
        6 => format!("C:\\{name}.conf"),
        7 => format!(" {name}.conf"),
        8 => format!("{name} .conf"),
        9 => format!("{name}.conf.conf"),
        10 => ".conf".to_string(),
        11 => format!("-{name}.conf"),
        12 => format!("{name}{}.conf", "x".repeat(16 - name.len().min(15))),
        13 => format!("{short}..{short}.conf"),
        14 => format!("{name}\t.conf"),
        _ => format!("é{name}.conf"),
    }
}

#[test]
fn unsafe_names_reject_the_whole_archive() {
    check("unsafe_names_reject_the_whole_archive", |rng| {
        let (mut entries, _) = valid_backup(rng);
        let bad = unsafe_entry_name(rng);
        let at = rng.below(entries.len() + 1);
        entries.insert(
            at,
            conf(bad.strip_suffix(".conf").unwrap(), &valid_config(rng)),
        );
        let archive = write_zip(rng, &entries);
        let error = backup_configs(Cursor::new(&archive)).unwrap_err();
        assert!(
            error.to_ascii_lowercase().contains("tunnel name"),
            "{bad:?}: {error}"
        );
    });
}

#[test]
fn duplicate_names_reject_the_whole_archive() {
    check("duplicate_names_reject_the_whole_archive", |rng| {
        let (mut entries, _) = valid_backup(rng);
        // ZipWriter refuses duplicates, so write a same-length placeholder and
        // patch it in the local header and the central directory.
        let files: Vec<String> = entries
            .iter()
            .filter_map(|e| match e {
                Entry::File(name, _) => Some(name.clone()),
                Entry::Dir(_) => None,
            })
            .collect();
        let target = rng.pick(&files).clone();
        let placeholder = "~".repeat(target.len());
        let at = rng.below(entries.len() + 1);
        let content = valid_config(rng).into_bytes();
        entries.insert(at, Entry::File(placeholder.clone(), content));
        let mut archive = write_zip(rng, &entries);
        let positions: Vec<usize> = (0..=archive.len() - placeholder.len())
            .filter(|&i| archive[i..].starts_with(placeholder.as_bytes()))
            .collect();
        assert_eq!(positions.len(), 2, "placeholder must appear exactly twice");
        for i in positions {
            archive[i..i + target.len()].copy_from_slice(target.as_bytes());
        }
        let error = backup_configs(Cursor::new(&archive)).unwrap_err();
        assert!(error.contains("duplicate"), "{target}: {error}");
        assert_eq!(backup_entry_count(Cursor::new(&archive)), 0);
    });
}

#[test]
fn archive_entry_limit_is_enforced() {
    check_scaled("archive_entry_limit_is_enforced", 200, |rng| {
        let total = MAX_ARCHIVE_ENTRIES as usize - 3 + rng.below(7);
        let mut entries: Vec<Entry> = (1..total)
            .map(|i| Entry::File(format!("e{i}"), Vec::new()))
            .collect();
        let config = valid_config(rng);
        entries.insert(rng.below(entries.len() + 1), conf("wg0", &config));
        let archive = write_zip(rng, &entries);
        let result = backup_configs(Cursor::new(&archive));
        if total as u64 <= MAX_ARCHIVE_ENTRIES {
            assert_eq!(result.unwrap(), vec![("wg0".to_string(), config)]);
            assert_eq!(backup_entry_count(Cursor::new(&archive)), total);
        } else {
            assert!(result.unwrap_err().contains("too many"), "{total}");
            assert_eq!(backup_entry_count(Cursor::new(&archive)), 0);
        }
    });
}

#[test]
fn forged_entry_counts_are_refused_before_parsing() {
    check("forged_entry_counts_are_refused_before_parsing", |rng| {
        let (entries, _) = valid_backup(rng);
        let mut archive = write_zip(rng, &entries);
        let end = last_end_record(&archive);
        let declared = match rng.below(3) {
            0 => u16::MAX,
            1 => MAX_ARCHIVE_ENTRIES as u16 + 1 + rng.below(1000) as u16,
            _ => rng.below(usize::from(u16::MAX)) as u16,
        };
        let fields: &[usize] = rng.pick::<&[usize]>(&[&[8], &[10], &[8, 10]]);
        for &field in fields {
            archive[end + field..end + field + 2].copy_from_slice(&declared.to_le_bytes());
        }
        let preflight = preflight_zip_directory(&mut Cursor::new(&archive), archive.len() as u64);
        let result = backup_configs(Cursor::new(&archive));
        if declared == u16::MAX {
            assert_eq!(preflight.unwrap_err(), "ZIP64 end locator is missing.");
            assert!(result.is_err());
        } else if u64::from(declared) > MAX_ARCHIVE_ENTRIES {
            assert!(preflight.unwrap_err().contains("too many"));
            assert!(result.unwrap_err().contains("too many"));
        }
        // Any other count may or may not match the directory; the parse only
        // has to return without panicking.
    });
}

#[test]
fn tunnel_config_count_limit_is_enforced() {
    check_scaled("tunnel_config_count_limit_is_enforced", 400, |rng| {
        let count = MAX_BACKUP_CONFIGS - 1 + rng.below(3);
        let config = valid_config(rng);
        let entries: Vec<Entry> = (0..count)
            .map(|i| conf(&format!("t{i}"), &config))
            .collect();
        let archive = write_zip(rng, &entries);
        let result = backup_configs(Cursor::new(&archive));
        if count <= MAX_BACKUP_CONFIGS {
            assert_eq!(result.unwrap().len(), count);
        } else {
            assert_eq!(
                result.unwrap_err(),
                "Backup exceeds the tunnel config size limit."
            );
        }
    });
}

#[test]
fn single_config_size_limit_is_enforced() {
    check_scaled("single_config_size_limit_is_enforced", 200, |rng| {
        let len = MAX_CONFIG_BYTES as usize - 2 + rng.below(5);
        let big = padded_config(rng, len);
        let (mut entries, _) = valid_backup(rng);
        entries.insert(rng.below(entries.len() + 1), conf("big", &big));
        let archive = write_zip(rng, &entries);
        let result = backup_configs(Cursor::new(&archive));
        if len as u64 <= MAX_CONFIG_BYTES {
            assert!(result.unwrap().iter().any(|(n, c)| n == "big" && *c == big));
        } else {
            assert_eq!(
                result.unwrap_err(),
                "Backup exceeds the tunnel config size limit."
            );
        }
    });
}

#[test]
fn total_config_size_limit_is_enforced() {
    check_scaled("total_config_size_limit_is_enforced", 500, |rng| {
        let count = 16 + rng.below(3);
        let total = MAX_TOTAL_CONFIG_BYTES as usize - count + rng.below(2 * count + 1);
        let entries: Vec<Entry> = (0..count)
            .map(|i| {
                let len = total / count + usize::from(i < total % count);
                let content = padded_config(rng, len);
                Entry::File(format!("t{i}.conf"), content.into_bytes())
            })
            .collect();
        let largest = total.div_ceil(count);
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for entry in &entries {
            if let Entry::File(name, content) = entry {
                // Stored keeps this quick; compression is covered elsewhere.
                zip.start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                    .unwrap();
                zip.write_all(content).unwrap();
            }
        }
        let archive = zip.finish().unwrap().into_inner();
        let result = backup_configs(Cursor::new(&archive));
        if total as u64 <= MAX_TOTAL_CONFIG_BYTES && largest as u64 <= MAX_CONFIG_BYTES {
            assert_eq!(result.unwrap().len(), count);
        } else {
            assert_eq!(
                result.unwrap_err(),
                "Backup exceeds the tunnel config size limit."
            );
        }
    });
}

#[test]
fn archive_size_limit_is_checked_before_reading() {
    check_scaled("archive_size_limit_is_checked_before_reading", 20, |rng| {
        let len = match rng.below(3) {
            0 => MAX_BACKUP_BYTES + 1 + rng.next_u64() % (1 << 40),
            1 => rng.below(22) as u64,
            _ => 22 + rng.below(256 * 1024) as u64,
        };
        let error = backup_configs(Zeros { len, pos: 0 }).unwrap_err();
        assert_eq!(backup_entry_count(Zeros { len, pos: 0 }), 0);
        let expected = if len > MAX_BACKUP_BYTES {
            "Backup exceeds 64 MiB."
        } else if len < 22 {
            "Invalid or oversized ZIP file."
        } else {
            "ZIP end record is missing or inconsistent."
        };
        assert_eq!(error, expected, "{len}");
    });
}

#[test]
fn corrupted_archives_never_panic_or_admit_bad_configs() {
    const MARKERS: &[&[u8]] = &[
        b"PK\x03\x04",
        b"PK\x01\x02",
        b"PK\x05\x06",
        b"PK\x06\x06",
        b"PK\x06\x07",
    ];
    check(
        "corrupted_archives_never_panic_or_admit_bad_configs",
        |rng| {
            let mut data = if rng.chance(1, 4) {
                let len = rng.below(512);
                rng.bytes(len)
            } else {
                let (entries, _) = valid_backup(rng);
                write_zip(rng, &entries)
            };
            for _ in 0..1 + rng.below(8) {
                if data.is_empty() {
                    break;
                }
                let at = rng.below(data.len());
                match rng.below(4) {
                    0 => {
                        let marker = *rng.pick(MARKERS);
                        let end = (at + marker.len()).min(data.len());
                        data[at..end].copy_from_slice(&marker[..end - at]);
                    }
                    1 => data.truncate(at),
                    2 => data[at] = 0xFF,
                    _ => data[at] ^= 1 << rng.below(8),
                }
            }
            let _ = preflight_zip_directory(&mut Cursor::new(&data), data.len() as u64);
            let count = backup_entry_count(Cursor::new(&data));
            assert!(count as u64 <= MAX_ARCHIVE_ENTRIES);
            if let Ok(configs) = backup_configs(Cursor::new(&data)) {
                assert!(count >= configs.len());
                for (name, content) in &configs {
                    assert!(validation::validate_tunnel_name(name).is_ok());
                    assert!(config::validate_basic_wireguard_config(content).is_ok());
                    assert!(!secrets::config_runs_scripts(content));
                }
            }
        },
    );
}

#[test]
fn script_hooks_and_invalid_configs_reject_the_whole_archive() {
    check(
        "script_hooks_and_invalid_configs_reject_the_whole_archive",
        |rng| {
            let (mut entries, _) = valid_backup(rng);
            let (bad, expected) = if rng.chance(1, 2) {
                let hook = rng.pick::<&str>(&["PreUp", "PostUp", "PreDown", "PostDown"]);
                let hook = format!("[Interface]\n{} = id\n", rng.recase(hook));
                (
                    valid_config(rng).replacen("[Interface]\n", &hook, 1),
                    "Script hooks are not allowed in backup tunnel hooked.",
                )
            } else {
                (format!("{}Bogus = 1\n", valid_config(rng)), "")
            };
            entries.insert(rng.below(entries.len() + 1), conf("hooked", &bad));
            let archive = write_zip(rng, &entries);
            let error = backup_configs(Cursor::new(&archive)).unwrap_err();
            if !expected.is_empty() {
                assert_eq!(error, expected);
            }
        },
    );
}

// ---------------------------------------------------------------- QR selection

/// Grid results that never hold a usable config: failed decodes, other QR
/// payloads, invalid configs and configs with root hooks.
fn noise_grid(rng: &mut Rng) -> Result<String, String> {
    match rng.below(4) {
        0 => Err(rng.messy_text(40)),
        1 => Ok(rng.messy_text(80)),
        2 => Ok(format!("{}Bogus = 1\n", valid_config(rng))),
        _ => Ok(format!("{}PostUp = id\n", valid_config(rng))),
    }
}

fn shuffle<T>(rng: &mut Rng, items: &mut [T]) {
    for i in (1..items.len()).rev() {
        items.swap(i, rng.below(i + 1));
    }
}

fn usable(content: &str) -> bool {
    config::validate_basic_wireguard_config(content).is_ok()
        && !secrets::config_runs_scripts(content)
}

#[test]
fn qr_selection_accepts_repeats_of_one_config() {
    check("qr_selection_accepts_repeats_of_one_config", |rng| {
        let config = valid_config(rng);
        let mut grids: Vec<Result<String, String>> =
            (0..1 + rng.below(4)).map(|_| Ok(config.clone())).collect();
        for _ in 0..rng.below(5) {
            let noise = noise_grid(rng);
            grids.push(noise);
        }
        shuffle(rng, &mut grids);
        assert_eq!(select_qr_config(grids).unwrap(), config);
    });
}

#[test]
fn qr_selection_refuses_distinct_configs() {
    check("qr_selection_refuses_distinct_configs", |rng| {
        let first = valid_config(rng);
        let second = valid_config(rng);
        assert_ne!(first, second);
        let mut grids = vec![Ok(first), Ok(second)];
        for _ in 0..rng.below(5) {
            let extra = if rng.chance(1, 2) {
                grids[rng.below(2)].clone()
            } else {
                noise_grid(rng)
            };
            grids.push(extra);
        }
        shuffle(rng, &mut grids);
        assert_eq!(
            select_qr_config(grids).unwrap_err(),
            "Image contains multiple different WireGuard configs; use an image with one tunnel."
        );
    });
}

#[test]
fn qr_selection_matches_the_one_distinct_config_rule() {
    check("qr_selection_matches_the_one_distinct_config_rule", |rng| {
        let pool = [valid_config(rng), valid_config(rng)];
        let grids: Vec<Result<String, String>> = (0..rng.below(6))
            .map(|_| {
                if rng.chance(1, 3) {
                    Ok(rng.pick(&pool).clone())
                } else {
                    noise_grid(rng)
                }
            })
            .collect();
        let mut distinct: Vec<&String> = Vec::new();
        for content in grids.iter().flatten() {
            if usable(content) && !distinct.contains(&content) {
                distinct.push(content);
            }
        }
        let result = select_qr_config(grids.clone());
        match distinct.len() {
            0 => assert_eq!(
                result.unwrap_err(),
                "No valid WireGuard config QR code found in the image."
            ),
            1 => assert_eq!(&result.unwrap(), distinct[0]),
            _ => assert!(result.unwrap_err().contains("multiple different")),
        }
    });
}

fn png(image: &image::GrayImage) -> Vec<u8> {
    let mut bytes = Vec::new();
    image
        .write_to(&mut Cursor::new(&mut bytes), image::ImageFormat::Png)
        .unwrap();
    bytes
}

#[test]
fn qr_image_dimensions_are_bounded() {
    check_scaled("qr_image_dimensions_are_bounded", 20, |rng| {
        let long = if rng.chance(1, 2) {
            MAX_QR_DIMENSION + 1 + rng.below(8) as u32
        } else {
            MAX_QR_DIMENSION - rng.below(8) as u32
        };
        let short = 1 + rng.below(3) as u32;
        let (width, height) = if rng.chance(1, 2) {
            (long, short)
        } else {
            (short, long)
        };
        let bytes = png(&image::GrayImage::new(width, height));
        match decode_qr_image(&bytes) {
            Ok(image) => {
                assert!(long <= MAX_QR_DIMENSION, "{width}x{height} accepted");
                assert_eq!(image.dimensions(), (width, height));
                assert_eq!(
                    decode_qr_bytes(&bytes).unwrap_err(),
                    "No QR code found in the image."
                );
            }
            Err(error) => {
                assert!(long > MAX_QR_DIMENSION, "{width}x{height}: {error}");
                assert!(error.contains("bounded QR image"), "{error}");
            }
        }
    });
}

#[test]
fn qr_decode_never_panics_on_corrupt_images() {
    check_scaled("qr_decode_never_panics_on_corrupt_images", 4, |rng| {
        let mut bytes = if rng.chance(1, 3) {
            let len = rng.below(256);
            rng.bytes(len)
        } else {
            let side = 1 + rng.below(48) as u32;
            let pixels = rng.bytes((side * side) as usize);
            let image = image::GrayImage::from_raw(side, side, pixels).unwrap();
            png(&image)
        };
        for _ in 0..rng.below(6) {
            if bytes.is_empty() {
                break;
            }
            let at = rng.below(bytes.len());
            bytes[at] = rng.next_u64() as u8;
        }
        if let Ok(content) = decode_qr_bytes(&bytes) {
            assert!(usable(&content));
        }
    });
}

/// Render QR codes for `payloads` side by side, each with a quiet zone.
fn qr_image(payloads: &[&str]) -> image::GrayImage {
    const SCALE: u32 = 3;
    const BORDER: u32 = 4;
    let codes: Vec<(u32, Vec<qrcode::Color>)> = payloads
        .iter()
        .map(|p| {
            let code = qrcode::QrCode::new(p.as_bytes()).unwrap();
            (code.width() as u32, code.to_colors())
        })
        .collect();
    let cell = (codes.iter().map(|(w, _)| *w).max().unwrap() + 2 * BORDER) * SCALE;
    image::GrayImage::from_fn(cell * codes.len() as u32, cell, |x, y| {
        let (width, colors) = &codes[(x / cell) as usize];
        let (mx, my) = ((x % cell) / SCALE, y / SCALE);
        let dark = (BORDER..BORDER + width).contains(&mx)
            && (BORDER..BORDER + width).contains(&my)
            && colors[((my - BORDER) * width + mx - BORDER) as usize] == qrcode::Color::Dark;
        image::Luma([if dark { 0 } else { 255 }])
    })
}

fn short_config(rng: &mut Rng) -> String {
    format!(
        "[Interface]\nPrivateKey = {}\nAddress = 10.{}.0.2/32\n",
        wg_key(rng),
        rng.below(256)
    )
}

#[test]
fn qr_images_yield_their_one_config_or_refuse_ambiguity() {
    check_scaled(
        "qr_images_yield_their_one_config_or_refuse_ambiguity",
        100,
        |rng| {
            let first = short_config(rng);
            let second = short_config(rng);
            let (payloads, expected): (Vec<&str>, Result<&str, &str>) = match rng.below(4) {
                0 => (vec![&first], Ok(&first)),
                1 => (vec![&first, &first], Ok(&first)),
                2 => (vec![&first, "https://example.com/not-a-config"], Ok(&first)),
                _ => (vec![&first, &second], Err("multiple different")),
            };
            let result = decode_qr_bytes(&png(&qr_image(&payloads)));
            match expected {
                Ok(config) => assert_eq!(result.unwrap(), config),
                Err(fragment) => assert!(result.unwrap_err().contains(fragment)),
            }
        },
    );
}
