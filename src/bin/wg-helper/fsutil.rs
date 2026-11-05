//! Filesystem primitives for root-owned state: private directories, the shared
//! mutation lock, link-safe config reads, backups and atomic writes.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{BACKUP_DIR, WG_DIR, conf_path, logging};

pub const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

fn euid() -> u32 {
    unsafe { libc::geteuid() }
}

pub fn ensure_wg_dir() -> Result<(), String> {
    ensure_private_dir(Path::new(WG_DIR))
}

pub fn lock_helper_state(dir: &Path, timeout: Duration) -> Result<File, String> {
    use std::os::fd::AsRawFd;
    ensure_private_dir(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(".helper.lock"))
        .map_err(|e| format!("open helper lock: {e}"))?;
    let metadata = lock.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != euid() {
        return Err("untrusted helper lock file".into());
    }
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let start = std::time::Instant::now();
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(format!("lock helper state: {error}"));
        }
        if start.elapsed() >= timeout {
            return Err("another helper operation is busy; retry when it completes".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

pub fn ensure_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.uid() != euid() => {
            return Err(format!("untrusted directory: {}", path.display()));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
        }
        Err(e) => return Err(format!("inspect {}: {e}", path.display())),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

pub fn check_private_dir(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.is_dir() || metadata.uid() != euid() || metadata.mode() & 0o022 != 0 {
        return Err(format!("untrusted directory: {}", path.display()));
    }
    Ok(())
}

/// Open an owned, single-link regular file without following links and
/// without blocking on a FIFO or device that was put in its place.
pub fn open_config(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != euid()
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "config must be an owned regular file with one link and no group/other write permission: {}",
            path.display()
        ));
    }
    Ok(file)
}

pub fn backup_existing(name: &str) -> Result<(), String> {
    let src = conf_path(name);
    if let Err(error) = fs::symlink_metadata(&src) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(format!("inspect {}: {error}", src.display()));
    }
    let input = open_config(&src)?;
    ensure_private_dir(Path::new(BACKUP_DIR))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dst = Path::new(BACKUP_DIR).join(format!("{name}.conf.{stamp}.{}", std::process::id()));
    backup_file(input, &dst)?;
    logging::event(
        "backup",
        Some(name),
        &[("path", &dst.display().to_string())],
    );
    Ok(())
}

pub fn backup_file(mut input: File, dst: &Path) -> Result<(), String> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)
        .map_err(|e| format!("create backup: {e}"))?;
    let result = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|e| format!("backup: {e}"));
    if result.is_err() {
        let _ = fs::remove_file(dst);
    }
    result
}

pub fn sync_dir(path: &Path) {
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
}

pub fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    atomic_write_mode(path, content, true)
}

/// Stage `content` next to `path`, fsync it, then publish it with one rename
/// (or a no-clobber link when `replace` is false). A failure at any step —
/// including a full disk — leaves the previous file untouched and removes the
/// staging file.
pub fn atomic_write_mode(path: &Path, content: &str, replace: bool) -> Result<(), String> {
    let dir = path.parent().ok_or("target path has no parent")?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("target path has no file name")?;
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .map_err(|e| format!("create temp {}: {e}", tmp.display()))?;
    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|e| format!("write temp {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync temp {}: {e}", tmp.display()))?;
        if replace {
            fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        } else {
            // An atomic link fails if another writer has created the target;
            // never overwrite an existing tunnel during rename.
            fs::hard_link(&tmp, path).map_err(|e| format!("create {}: {e}", path.display()))?;
            fs::remove_file(&tmp).map_err(|e| format!("remove staging link: {e}"))?;
        }
        sync_dir(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn read_config_text(path: &Path) -> Result<String, String> {
    read_config_input(open_config(path)?)
}

pub fn read_config_input(reader: impl Read) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("read config: {e}"))?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err("config exceeds the 1 MiB size limit".into());
    }
    Ok(text)
}

pub fn read_stdin() -> Result<String, String> {
    read_config_input(std::io::stdin())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn config_reads_reject_symlinks_hardlinks_directories_and_fifos() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let source = dir.0.join("secret");
        fs::write(&source, "private contents").unwrap();
        let link = dir.0.join("wg0.conf");
        symlink(&source, &link).unwrap();
        assert!(open_config(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&source, &link).unwrap();
        assert!(open_config(&link).is_err());
        fs::remove_file(&link).unwrap();
        assert!(open_config(&source).is_ok());
        for mode in [0o620, 0o602, 0o666] {
            fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                open_config(&source).is_err(),
                "untrusted config mode {mode:o}"
            );
        }
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(open_config(&source).is_ok());
        assert!(open_config(&dir.0).is_err());
        let fifo = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(open_config(&link).is_err());
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "FIFO must not block"
        );
    }

    #[test]
    fn private_directory_checks_do_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let target = dir.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.0.join("link");
        symlink(&target, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert!(check_private_dir(&link).is_err());
        assert_eq!(fs::metadata(target).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn shared_mutation_lock_has_a_bounded_wait_and_rejects_links() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let first = lock_helper_state(&dir.0, Duration::from_millis(20)).unwrap();
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_err());
        drop(first);
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_ok());
        let path = dir.0.join(".helper.lock");
        fs::remove_file(&path).unwrap();
        let other = dir.0.join("other");
        fs::write(&other, "unchanged").unwrap();
        symlink(&other, &path).unwrap();
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_err());
        assert_eq!(fs::read_to_string(other).unwrap(), "unchanged");
    }

    #[test]
    fn backups_are_exclusive_private_and_do_not_clobber_existing_files() {
        let dir = TempDir::new();
        let source = dir.0.join("source");
        fs::write(&source, "original").unwrap();
        let backup = dir.0.join("backup");
        backup_file(File::open(&source).unwrap(), &backup).unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
        fs::write(&source, "new value").unwrap();
        assert!(backup_file(File::open(source).unwrap(), &backup).is_err());
        assert_eq!(fs::read_to_string(backup).unwrap(), "original");
    }

    #[test]
    fn failed_atomic_write_cleans_staging_and_no_replace_preserves_target() {
        let dir = TempDir::new();
        let path = dir.0.join("wg0.conf");
        fs::create_dir(&path).unwrap();
        assert!(atomic_write(&path, "new").is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        fs::remove_dir(&path).unwrap();
        atomic_write(&path, "original").unwrap();
        assert!(atomic_write_mode(&path, "replacement", false).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn atomic_write_refuses_a_planted_staging_symlink() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let path = dir.0.join("wg0.conf");
        let victim = dir.0.join("victim");
        fs::write(&victim, "keep").unwrap();
        let staging = dir.0.join(format!(".wg0.conf.tmp.{}", std::process::id()));
        symlink(&victim, &staging).unwrap();
        assert!(atomic_write(&path, "new").is_err());
        assert_eq!(fs::read_to_string(victim).unwrap(), "keep");
        assert!(!path.exists());
    }

    #[test]
    fn oversized_config_input_is_bounded() {
        assert!(read_config_input(std::io::repeat(b'x').take(MAX_CONFIG_BYTES + 2)).is_err());
        assert!(read_config_input(std::io::repeat(b'x').take(MAX_CONFIG_BYTES)).is_ok());
    }
}
