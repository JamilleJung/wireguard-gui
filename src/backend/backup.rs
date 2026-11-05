//! Backups and exports: timestamped archives of every tunnel config, kept under
//! the user's data dir (NOT /etc/wireguard, so no helper needed for storage),
//! plus atomic private file writes used by every export.

use std::io::Write;
use std::path::{Path, PathBuf};

use super::error::HelperError;
use crate::safe_file;

/// All tunnel configs as (filename, contents), for export.
pub fn read_all_configs() -> Result<Vec<(String, String)>, String> {
    collect_configs(super::saved_tunnel_names()?, |name| {
        super::read_config(name).map_err(String::from)
    })
}

pub(crate) fn collect_configs(
    names: impl IntoIterator<Item = String>,
    mut read: impl FnMut(&str) -> Result<String, String>,
) -> Result<Vec<(String, String)>, String> {
    names
        .into_iter()
        .map(|name| {
            super::validate_tunnel_name(&name)?;
            let content = read(&name)?;
            Ok((format!("{name}.conf"), content))
        })
        .collect()
}

/// Export is all-or-error: unreadable configs never silently disappear from a
/// successful backup, and failed writes preserve an existing destination.
pub fn export_zip(dest: &Path) -> Result<usize, String> {
    let files = read_all_configs()?;
    write_zip_files(dest, &files, true)
}

fn write_zip_files(
    dest: &Path,
    files: &[(String, String)],
    replace: bool,
) -> Result<usize, String> {
    if files.is_empty() {
        return Err("No tunnels to export.".into());
    }
    atomic_private_write(dest, replace, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        for (name, content) in files {
            zip.start_file(name, opts).map_err(|e| e.to_string())?;
            zip.write_all(content.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(files.len())
    })
}

/// Publish a completed 0600 sibling file. Replacing a hardlink changes only
/// the requested directory entry; a partial write never truncates another file.
pub(crate) fn atomic_private_write<T>(
    dest: &Path,
    replace: bool,
    write: impl FnOnce(&mut std::fs::File) -> Result<T, String>,
) -> Result<T, String> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if std::fs::symlink_metadata(dest).is_ok_and(|meta| !meta.is_file()) {
        return Err("Destination must be a regular file, not a directory or symlink.".into());
    }
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let template = dir.join(".wireguard-export-XXXXXX");
    let mut name = std::ffi::CString::new(template.as_os_str().as_bytes())
        .map_err(|e| e.to_string())?
        .into_bytes_with_nul();
    let fd = unsafe { libc::mkstemp(name.as_mut_ptr().cast()) };
    if fd < 0 {
        return Err(format!(
            "create private file: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    name.pop();
    let tmp = PathBuf::from(std::ffi::OsString::from_vec(name));
    let result = (|| {
        let result = write(&mut file)?;
        file.sync_all().map_err(|e| e.to_string())?;
        if replace {
            std::fs::rename(&tmp, dest).map_err(|e| format!("publish {}: {e}", dest.display()))?;
        } else {
            // link is an atomic no-clobber publication; remove the temporary
            // link afterward, leaving the completed archive with one link.
            std::fs::hard_link(&tmp, dest)
                .map_err(|e| format!("publish {}: {e}", dest.display()))?;
        }
        if let Ok(parent) = std::fs::File::open(dir) {
            let _ = parent.sync_all();
        }
        Ok(result)
    })();
    let _ = std::fs::remove_file(tmp);
    result
}

/// Metadata about one backup archive.
#[derive(Clone)]
pub struct BackupInfo {
    #[allow(dead_code)]
    pub path: PathBuf,
    pub name: String,
    pub when_secs: i64,
    pub size: u64,
    pub count: usize,
}

/// `$XDG_DATA_HOME` (or `~/.local/share`) `/wireguard/backups`, created 0700 —
/// the archives bundle private keys, so keep the whole tree private. The dir is
/// shared with wireguard-tui (same /etc/wireguard configs).
pub fn backup_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .filter(|p| p.is_absolute())
        .ok_or("Can't locate an absolute home directory for backups.")?;
    ensure_backup_dir(&base)
}

pub(crate) fn ensure_backup_dir(base: &Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let dir = base.join("wireguard").join("backups");
    for path in [base.join("wireguard"), dir.clone()] {
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("Backup directory is not owned by the current user.".into());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    dir.canonicalize().map_err(|e| e.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn tm_of(secs: i64) -> Option<libc::tm> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        None
    } else {
        Some(tm)
    }
}

/// Local "YYYY-MM-DD HH:MM:SS" for display.
pub fn fmt_time(secs: i64) -> String {
    match tm_of(secs) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => "unknown".into(),
    }
}

fn stamp_compact(secs: i64) -> String {
    match tm_of(secs) {
        Some(tm) => format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => format!("{secs}"),
    }
}

/// Human-readable size, e.g. "12.3 KiB".
pub fn fmt_size(bytes: u64) -> String {
    super::fmt_bytes(bytes)
}

/// Create a timestamped backup of every tunnel config. Returns its metadata.
pub fn create_backup() -> Result<BackupInfo, String> {
    let dir = backup_dir()?;
    let secs = now_secs();
    static NEXT_BACKUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_BACKUP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!(
        "wg-backup-{}-{}-{sequence}.zip",
        stamp_compact(secs),
        std::process::id()
    );
    let path = dir.join(&name);
    let count = write_zip_files(&path, &read_all_configs()?, false)?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(BackupInfo {
        path,
        name,
        when_secs: secs,
        size,
        count,
    })
}

fn count_zip_entries(path: &Path) -> usize {
    use std::io::{Seek, SeekFrom};
    let Ok(mut file) = open_backup(path) else {
        return 0;
    };
    let Ok(meta) = file.metadata() else {
        return 0;
    };
    if preflight_zip_directory(&mut file, meta.len()).is_err()
        || file.seek(SeekFrom::Start(0)).is_err()
    {
        return 0;
    }
    zip::ZipArchive::new(file)
        .ok()
        .filter(|zip| !zip.has_duplicate_names() && zip.len() <= 4096)
        .map(|zip| zip.len())
        .unwrap_or(0)
}

/// List existing backups, newest first.
pub fn list_backups() -> Vec<BackupInfo> {
    let dir = match backup_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        let Ok(file) = open_backup(&path) else {
            continue;
        };
        let Ok(md) = file.metadata() else { continue };
        let when_secs = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(BackupInfo {
            name: ent.file_name().to_string_lossy().into_owned(),
            when_secs,
            size: md.len(),
            count: count_zip_entries(&path),
            path,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.when_secs));
    out
}

pub(crate) const MAX_BACKUP_BYTES: u64 = 64 * safe_file::MIB;

/// Open relative to the already-open backup directory. Do not canonicalize a
/// file then reopen its spelling: a symlink swap could redirect that second IO.
fn open_backup(path: &Path) -> Result<std::fs::File, String> {
    let dir = backup_dir()?;
    open_backup_in(&dir, path)
}

pub(crate) fn open_backup_in(dir: &Path, path: &Path) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if path
        .parent()
        .ok_or("Backup path has no directory.")?
        .canonicalize()
        .map_err(|e| e.to_string())?
        != dir
    {
        return Err("Refusing a path outside the backup directory.".into());
    }
    let name = path.file_name().ok_or("Backup path has no filename.")?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| e.to_string())?;
    let file = safe_file::open_regular_at(&directory, name, "Backup", MAX_BACKUP_BYTES)
        .map_err(|e| format!("open backup: {e}"))?;
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if meta.nlink() != 1 || meta.uid() != unsafe { libc::geteuid() } {
        return Err(
            "Backup must be an owned regular file with one link and at most 64 MiB.".into(),
        );
    }
    Ok(file)
}

/// Where restored configs are written. The helper is the real store; tests
/// substitute an in-memory one that fails on demand.
pub(crate) trait ConfigStore {
    fn names(&mut self) -> Result<Vec<String>, HelperError>;
    fn read(&mut self, name: &str) -> Result<String, HelperError>;
    fn save(&mut self, name: &str, content: &str) -> Result<(), HelperError>;
    fn delete(&mut self, name: &str) -> Result<(), HelperError>;
}

struct HelperStore;

impl ConfigStore for HelperStore {
    fn names(&mut self) -> Result<Vec<String>, HelperError> {
        super::saved_tunnel_names()
    }
    fn read(&mut self, name: &str) -> Result<String, HelperError> {
        super::read_config(name)
    }
    fn save(&mut self, name: &str, content: &str) -> Result<(), HelperError> {
        super::save_config(name, content)
    }
    fn delete(&mut self, name: &str) -> Result<(), HelperError> {
        super::delete(name)
    }
}

/// What a restore did, including any rollback after a failure.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// Existing tunnels whose file was replaced by the backup's version.
    pub overwritten: Vec<String>,
    /// Tunnels that did not exist before and were created.
    pub created: Vec<String>,
    pub failure: Option<RestoreFailure>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct RestoreFailure {
    pub tunnel: String,
    pub error: String,
    /// Tunnels whose previous contents were written back.
    pub reverted: Vec<String>,
    /// Tunnels this restore had created and then deleted again.
    pub removed: Vec<String>,
    /// Tunnels that could not be put back, with the reason.
    pub rollback_failed: Vec<(String, String)>,
}

impl RestoreReport {
    pub fn succeeded(&self) -> bool {
        self.failure.is_none()
    }

    pub fn message(&self) -> String {
        let Some(f) = &self.failure else {
            return format!(
                "Restored {} tunnel(s) from backup ({} new, {} overwritten)",
                self.created.len() + self.overwritten.len(),
                self.created.len(),
                self.overwritten.len()
            );
        };
        let mut parts = vec![format!("Restore failed at {}: {}.", f.tunnel, f.error)];
        if !f.reverted.is_empty() {
            parts.push(format!(
                "Restored previous contents of {}.",
                f.reverted.join(", ")
            ));
        }
        if !f.removed.is_empty() {
            parts.push(format!(
                "Removed tunnels the restore had created: {}.",
                f.removed.join(", ")
            ));
        }
        if f.rollback_failed.is_empty() {
            parts.push("No tunnel was changed by this restore.".into());
        } else {
            let failed: Vec<String> = f
                .rollback_failed
                .iter()
                .map(|(name, error)| format!("{name} ({error})"))
                .collect();
            parts.push(format!(
                "Rollback FAILED for {} — check these tunnels manually.",
                failed.join("; ")
            ));
        }
        parts.join(" ")
    }
}

/// Write every config, rolling back the ones already written if a later one
/// fails. Returns `Err` only when nothing was written (preflight failure).
pub(crate) fn restore_into(
    store: &mut impl ConfigStore,
    configs: &[(String, String)],
) -> Result<RestoreReport, String> {
    let existing = store
        .names()
        .map_err(|e| format!("Couldn't list current tunnels: {e}"))?;
    // Read every file the restore will overwrite before writing anything, so
    // a rollback never depends on a read that could fail halfway through.
    let mut previous = Vec::with_capacity(configs.len());
    for (name, _) in configs {
        if existing.iter().any(|e| e == name) {
            let text = store.read(name).map_err(|e| {
                format!(
                    "Couldn't read the current {name} before restoring (nothing was changed): {e}"
                )
            })?;
            previous.push(Some(text));
        } else {
            previous.push(None);
        }
    }
    let mut report = RestoreReport::default();
    let mut written: Vec<(&str, Option<&str>)> = Vec::new();
    for ((name, content), prev) in configs.iter().zip(&previous) {
        match store.save(name, content) {
            Ok(()) => {
                written.push((name, prev.as_deref()));
                if prev.is_some() {
                    report.overwritten.push(name.clone());
                } else {
                    report.created.push(name.clone());
                }
            }
            Err(error) => {
                // A timed-out save may have landed: undo it like the others.
                if error.outcome_unknown() {
                    written.push((name, prev.as_deref()));
                }
                let mut failure = RestoreFailure {
                    tunnel: name.clone(),
                    error: error.to_string(),
                    ..Default::default()
                };
                for (done, prev) in written.into_iter().rev() {
                    let undo = match prev {
                        Some(text) => store.save(done, text),
                        None => store.delete(done),
                    };
                    match (undo, prev) {
                        (Ok(()), Some(_)) => failure.reverted.push(done.to_string()),
                        (Ok(()), None) => failure.removed.push(done.to_string()),
                        (Err(e), _) => failure
                            .rollback_failed
                            .push((done.to_string(), e.to_string())),
                    }
                }
                failure.reverted.reverse();
                failure.removed.reverse();
                report.failure = Some(failure);
                return Ok(report);
            }
        }
    }
    Ok(report)
}

/// Restore every `.conf` inside a backup archive (overwriting tunnels of the
/// same name), rolling back on a partial failure.
pub fn restore_backup(path: &Path) -> Result<RestoreReport, String> {
    let configs = backup_configs(open_backup(path)?)?;
    restore_into(&mut HelperStore, &configs)
}

// ZIP metadata is eagerly allocated by ZipArchive::new. Read only the bounded
// end records first so a huge declared/actual entry count cannot allocate that
// metadata before the archive's entry limit is enforced.
pub(crate) fn preflight_zip_directory(
    reader: &mut (impl std::io::Read + std::io::Seek),
    length: u64,
) -> Result<(), String> {
    use std::io::SeekFrom;
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
                ) > 4096
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
        } else if entries > 4096 || disk_entries > 4096 {
            return Err("Backup contains too many archive entries.".into());
        }
    }
    if !found {
        return Err("ZIP end record is missing or inconsistent.".into());
    }
    Ok(())
}

// Validate the whole archive before the first privileged write. Bound both
// declared and actual uncompressed sizes so a corrupt backup cannot exhaust
// memory or overwrite earlier tunnels before a later invalid entry is found.
pub(crate) fn backup_configs<R: std::io::Read + std::io::Seek>(
    mut reader: R,
) -> Result<Vec<(String, String)>, String> {
    use std::io::Read;
    const MAX_CONFIG_BYTES: u64 = safe_file::MIB;
    const MAX_TOTAL_BYTES: u64 = 16 * MAX_CONFIG_BYTES;
    let length = reader
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| e.to_string())?;
    if length > MAX_BACKUP_BYTES {
        return Err("Backup exceeds 64 MiB.".into());
    }
    preflight_zip_directory(&mut reader, length)?;
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    if zip.has_duplicate_names() {
        return Err("Backup contains duplicate archive filenames.".into());
    }
    if zip.len() > 4096 {
        return Err("Backup contains too many archive entries.".into());
    }
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
        super::validate_tunnel_name(name)?;
        if name.trim() != name || !names.insert(name.to_string()) {
            return Err(format!("Invalid or duplicate tunnel name in backup: {raw}"));
        }
        if entry.size() > MAX_CONFIG_BYTES || configs.len() >= 1024 {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        let mut content = String::new();
        entry
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        total += content.len() as u64;
        if content.len() as u64 > MAX_CONFIG_BYTES || total > MAX_TOTAL_BYTES {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        crate::config::validate_basic_wireguard_config(&content)?;
        if super::config_runs_scripts(&content) {
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

/// Delete a backup archive (only within the backup directory).
pub fn delete_backup(path: &Path) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let _file = open_backup(path)?;
    let dir = backup_dir()?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| e.to_string())?;
    let name = std::ffi::CString::new(
        path.file_name()
            .ok_or("Missing backup filename.")?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

pub(crate) fn write_private(dest: &Path, data: &[u8]) -> Result<(), String> {
    atomic_private_write(dest, true, |file| {
        file.write_all(data).map_err(|e| e.to_string())
    })
}

pub fn export_backup_to(src: &Path, dest: &Path) -> Result<(), String> {
    let data = safe_file::read_opened(open_backup(src)?, "Backup", MAX_BACKUP_BYTES)
        .map_err(|e| e.to_string())?;
    write_private(dest, &data)
}

pub fn save_text_to(dest: &Path, text: &str) -> Result<(), String> {
    write_private(dest, text.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::tests::{KEY, scratch_dir};
    use std::collections::BTreeMap;

    #[test]
    fn atomic_exports_preserve_old_data_and_other_hardlinks_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        let dest = dir.join("export.zip");
        let other = dir.join("other-file");
        std::fs::write(&dest, b"old archive").unwrap();
        std::fs::hard_link(&dest, &other).unwrap();
        let failed: Result<(), String> = atomic_private_write(&dest, true, |file| {
            file.write_all(b"partial secret").unwrap();
            Err("simulated full disk".into())
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"old archive");
        write_private(&dest, b"new secret").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new secret");
        assert_eq!(std::fs::read(&other).unwrap(), b"old archive");
        assert_eq!(dest.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert!(
            atomic_private_write(&dest, false, |f| f
                .write_all(b"overwrite")
                .map_err(|e| e.to_string()))
            .is_err()
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"new secret");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn disk_full_while_writing_keeps_the_previous_file_and_no_temp() {
        let dir = scratch_dir();
        let dest = dir.join("log.txt");
        std::fs::write(&dest, b"previous log").unwrap();
        // /dev/full fails every write with ENOSPC, like a full filesystem.
        let error = atomic_private_write(&dest, true, |_| {
            let mut full = std::fs::OpenOptions::new()
                .write(true)
                .open("/dev/full")
                .unwrap();
            full.write_all(b"new log").map_err(|e| e.to_string())
        })
        .unwrap_err();
        assert!(error.contains("No space left"), "{error}");
        assert_eq!(std::fs::read(&dest).unwrap(), b"previous log");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_refuses_symlinks_and_never_silently_omits_failed_reads() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir();
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::write(&real, b"do not touch").unwrap();
        symlink(&real, &link).unwrap();
        assert!(write_private(&link, b"secret").is_err());
        assert_eq!(std::fs::read(&real).unwrap(), b"do not touch");
        let result = collect_configs(["wg0".to_string(), "wg1".to_string()], |name| {
            if name == "wg0" {
                Ok("valid content".into())
            } else {
                Err("permission denied".into())
            }
        });
        assert_eq!(result.unwrap_err(), "permission denied");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_storage_rejects_symlink_directories_and_linked_archives() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = scratch_dir();
        let outside = scratch_dir();
        symlink(&outside, base.join("wireguard")).unwrap();
        assert!(ensure_backup_dir(&base).is_err());
        assert!(!outside.join("backups").exists());
        std::fs::remove_file(base.join("wireguard")).unwrap();
        let dir = ensure_backup_dir(&base).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_backup_dir(&base).unwrap();
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        let real = dir.join("real.zip");
        let linked = dir.join("linked.zip");
        std::fs::write(&real, b"archive").unwrap();
        assert!(open_backup_in(&dir, &real).is_ok());
        symlink(&real, &linked).unwrap();
        assert!(open_backup_in(&dir, &linked).is_err());
        std::fs::remove_file(&linked).unwrap();
        std::fs::hard_link(&real, &linked).unwrap();
        assert!(open_backup_in(&dir, &real).is_err());
        std::fs::remove_file(&linked).unwrap();
        // A FIFO in the backup directory is refused without blocking.
        use std::os::unix::ffi::OsStrExt as _;
        let fifo = dir.join("pipe.zip");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let error = open_backup_in(&dir, &fifo).unwrap_err();
        assert!(error.contains("regular file"), "{error}");
        std::fs::remove_dir_all(base).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn zip_entry_budget_is_checked_before_eager_metadata_allocation() {
        let mut end = vec![0; 22];
        end[..4].copy_from_slice(b"PK\x05\x06");
        end[8..10].copy_from_slice(&4097u16.to_le_bytes());
        end[10..12].copy_from_slice(&4097u16.to_le_bytes());
        assert!(
            backup_configs(std::io::Cursor::new(end))
                .unwrap_err()
                .contains("too many")
        );
        let mut zip64 = vec![0; 56 + 20 + 22];
        zip64[..4].copy_from_slice(b"PK\x06\x06");
        zip64[4..12].copy_from_slice(&44u64.to_le_bytes());
        zip64[24..32].copy_from_slice(&4097u64.to_le_bytes());
        zip64[32..40].copy_from_slice(&4097u64.to_le_bytes());
        zip64[56..60].copy_from_slice(b"PK\x06\x07");
        zip64[76..80].copy_from_slice(b"PK\x05\x06");
        zip64[84..86].copy_from_slice(&u16::MAX.to_le_bytes());
        zip64[86..88].copy_from_slice(&u16::MAX.to_le_bytes());
        assert!(
            backup_configs(std::io::Cursor::new(zip64))
                .unwrap_err()
                .contains("too many")
        );
    }

    #[test]
    fn small_fake_zip_footer_cannot_mask_an_oversized_fallback_directory() {
        let mut entries = vec![0; 44];
        entries[..4].copy_from_slice(b"PK\x05\x06");
        entries[8..10].copy_from_slice(&4097u16.to_le_bytes());
        entries[10..12].copy_from_slice(&4097u16.to_le_bytes());
        entries[20..22].copy_from_slice(&22u16.to_le_bytes());
        entries[22..26].copy_from_slice(b"PK\x05\x06");
        assert!(
            backup_configs(std::io::Cursor::new(entries))
                .unwrap_err()
                .contains("too many")
        );
        // A ZIP64 candidate can cross the scanner's chunk boundary and the
        // parser also accepts a 40-byte declared body before its later checks.
        let mut zip64 = vec![0; 64 * 1024 + 80];
        let index = 64 * 1024 - 20;
        zip64[index..index + 4].copy_from_slice(b"PK\x06\x06");
        zip64[index + 4..index + 12].copy_from_slice(&40u64.to_le_bytes());
        zip64[index + 24..index + 32].copy_from_slice(&4097u64.to_le_bytes());
        assert!(
            preflight_zip_directory(&mut std::io::Cursor::new(&zip64), zip64.len() as u64)
                .unwrap_err()
                .contains("too many")
        );
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        let mut trailing = backup_archive(&[("wg0.conf", &valid)]).into_inner();
        trailing.extend_from_slice(b"harmless trailing data");
        assert_eq!(
            backup_configs(std::io::Cursor::new(trailing))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn duplicate_central_directory_names_are_not_hidden_by_zip_indexing() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        let mut archive =
            backup_archive(&[("wg0.conf", &valid), ("wg1.conf", &valid)]).into_inner();
        for index in 0..archive.len().saturating_sub(8) {
            if &archive[index..index + 8] == b"wg1.conf" {
                archive[index + 2] = b'0';
            }
        }
        assert!(
            backup_configs(std::io::Cursor::new(archive))
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn oversized_zip_is_rejected_before_parsing_metadata() {
        let dir = scratch_dir();
        let path = dir.join("large.zip");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BACKUP_BYTES + 1).unwrap();
        drop(file);
        assert!(
            backup_configs(std::fs::File::open(path).unwrap())
                .unwrap_err()
                .contains("64 MiB")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn backup_archive(files: &[(&str, &str)]) -> std::io::Cursor<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap()
    }

    #[test]
    fn backup_restore_only_reads_tunnel_configs() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        let configs = backup_configs(backup_archive(&[
            ("README.txt", "not a config"),
            ("wg0.conf", &valid),
        ]))
        .unwrap();
        assert_eq!(configs, vec![("wg0".to_string(), valid)]);
        assert!(backup_configs(backup_archive(&[("notes.txt", "text")])).is_err());
    }

    #[test]
    fn backup_restore_preflights_names_and_all_content() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        for name in ["../wg0.conf", "nested/wg0.conf", "wg 0.conf", " wg0.conf"] {
            assert!(
                backup_configs(backup_archive(&[(name, &valid)])).is_err(),
                "{name}"
            );
        }
        assert!(
            backup_configs(backup_archive(&[
                ("wg0.conf", &valid),
                ("wg1.conf", "invalid config"),
            ]))
            .is_err()
        );
        let scripted = format!("{valid}PostUp = id\n");
        assert!(backup_configs(backup_archive(&[("wg0.conf", &scripted)])).is_err());
    }

    #[test]
    fn backup_restore_rejects_oversized_configs() {
        let oversized = "x".repeat(1024 * 1024 + 1);
        assert!(backup_configs(backup_archive(&[("wg0.conf", &oversized)])).is_err());
    }

    /// In-memory store that fails the Nth save with the given error.
    #[derive(Default)]
    struct FakeStore {
        files: BTreeMap<String, String>,
        saves: usize,
        fail_save_at: Option<(usize, HelperError)>,
        /// Whether a failing save still lands (a timeout that completed).
        failed_save_lands: bool,
        fail_delete: bool,
        fail_read: bool,
    }

    impl ConfigStore for FakeStore {
        fn names(&mut self) -> Result<Vec<String>, HelperError> {
            Ok(self.files.keys().cloned().collect())
        }
        fn read(&mut self, name: &str) -> Result<String, HelperError> {
            if self.fail_read {
                return Err(HelperError::from_failure(
                    "read".into(),
                    "Permission denied".into(),
                ));
            }
            self.files
                .get(name)
                .cloned()
                .ok_or(HelperError::InvalidOutput("missing".into()))
        }
        fn save(&mut self, name: &str, content: &str) -> Result<(), HelperError> {
            let index = self.saves;
            self.saves += 1;
            if let Some((at, error)) = &self.fail_save_at
                && *at == index
            {
                if self.failed_save_lands {
                    self.files.insert(name.into(), content.into());
                }
                return Err(error.clone());
            }
            self.files.insert(name.into(), content.into());
            Ok(())
        }
        fn delete(&mut self, name: &str) -> Result<(), HelperError> {
            if self.fail_delete {
                return Err(HelperError::from_failure("delete".into(), "busy".into()));
            }
            self.files.remove(name);
            Ok(())
        }
    }

    fn configs() -> Vec<(String, String)> {
        ["wg0", "wg1", "wg2"]
            .into_iter()
            .map(|n| (n.to_string(), format!("backup {n}")))
            .collect()
    }

    fn store_with_wg0() -> FakeStore {
        let mut store = FakeStore::default();
        store.files.insert("wg0".into(), "current wg0".into());
        store
    }

    #[test]
    fn restore_reports_created_and_overwritten_tunnels() {
        let mut store = store_with_wg0();
        let report = restore_into(&mut store, &configs()).unwrap();
        assert!(report.succeeded());
        assert_eq!(report.overwritten, vec!["wg0"]);
        assert_eq!(report.created, vec!["wg1", "wg2"]);
        assert!(report.message().contains("Restored 3 tunnel(s)"));
        assert_eq!(store.files["wg0"], "backup wg0");
    }

    #[test]
    fn failed_restore_rolls_back_overwritten_and_created_tunnels() {
        let mut store = store_with_wg0();
        store.fail_save_at = Some((
            2,
            HelperError::from_failure("save wg2".into(), "Permission denied".into()),
        ));
        let report = restore_into(&mut store, &configs()).unwrap();
        let failure = report.failure.as_ref().unwrap();
        assert_eq!(failure.tunnel, "wg2");
        assert_eq!(failure.reverted, vec!["wg0"]);
        assert_eq!(failure.removed, vec!["wg1"]);
        assert!(failure.rollback_failed.is_empty());
        assert_eq!(store.files.len(), 1);
        assert_eq!(store.files["wg0"], "current wg0");
        let message = report.message();
        assert!(message.contains("Restore failed at wg2"), "{message}");
        assert!(message.contains("No tunnel was changed"), "{message}");
    }

    #[test]
    fn timed_out_save_is_rolled_back_too() {
        let mut store = store_with_wg0();
        store.fail_save_at = Some((1, HelperError::TimedOut));
        store.failed_save_lands = true;
        let report = restore_into(&mut store, &configs()).unwrap();
        let failure = report.failure.unwrap();
        assert_eq!(failure.removed, vec!["wg1"]);
        assert_eq!(failure.reverted, vec!["wg0"]);
        assert_eq!(store.files.keys().collect::<Vec<_>>(), vec!["wg0"]);
        assert_eq!(store.files["wg0"], "current wg0");
    }

    #[test]
    fn rollback_failures_are_reported_exactly() {
        let mut store = store_with_wg0();
        store.fail_save_at = Some((2, HelperError::TimedOut));
        store.fail_delete = true;
        let report = restore_into(&mut store, &configs()).unwrap();
        let failure = report.failure.as_ref().unwrap();
        assert_eq!(failure.reverted, vec!["wg0"]);
        let names: Vec<&str> = failure
            .rollback_failed
            .iter()
            .map(|(n, _)| n.as_str())
            .collect();
        assert_eq!(names, vec!["wg2", "wg1"]);
        assert!(
            report.message().contains("Rollback FAILED for wg2"),
            "{}",
            report.message()
        );
    }

    #[test]
    fn unreadable_current_config_aborts_before_any_write() {
        let mut store = store_with_wg0();
        store.fail_read = true;
        let error = restore_into(&mut store, &configs()).unwrap_err();
        assert!(error.contains("nothing was changed"), "{error}");
        assert_eq!(store.saves, 0);
    }
}
