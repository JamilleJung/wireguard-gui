//! One routine for opening user-chosen or app-owned files without hanging or
//! being redirected: config import, QR import and backup restore all use it.
//!
//! The file is opened with `O_NONBLOCK | O_NOFOLLOW | O_CLOEXEC` so a named
//! pipe, socket or device can never block the worker, and a symbolic link is
//! refused instead of followed. The opened descriptor is then checked with
//! `fstat` (never the path again), so the checks apply to exactly what is read.

use std::ffi::OsStr;
use std::fmt;
use std::fs::File;
use std::io::Read as _;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt as _;
use std::os::unix::fs::{FileTypeExt as _, OpenOptionsExt as _};
use std::path::Path;

pub const MIB: u64 = 1024 * 1024;

const OPEN_FLAGS: libc::c_int = libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC;

/// Why a file could not be opened or read. `what` names the file for the
/// user ("Config", "QR image", "Backup").
#[derive(Debug)]
pub enum SafeFileError {
    Open {
        what: &'static str,
        source: std::io::Error,
    },
    SymbolicLink {
        what: &'static str,
    },
    NotRegular {
        what: &'static str,
        kind: &'static str,
    },
    TooLarge {
        what: &'static str,
        limit: u64,
    },
    Read {
        what: &'static str,
        source: std::io::Error,
    },
    NotUtf8 {
        what: &'static str,
    },
}

impl SafeFileError {
    /// The OS error behind an open/read failure, if any (for tests and logs).
    pub fn raw_os_error(&self) -> Option<i32> {
        match self {
            Self::Open { source, .. } | Self::Read { source, .. } => source.raw_os_error(),
            _ => None,
        }
    }
}

fn limit_label(limit: u64) -> String {
    if limit >= MIB && limit.is_multiple_of(MIB) {
        format!("{} MiB", limit / MIB)
    } else {
        format!("{limit} bytes")
    }
}

impl fmt::Display for SafeFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Open { what, source } => match source.raw_os_error() {
                Some(libc::EACCES) | Some(libc::EPERM) => {
                    write!(
                        f,
                        "Couldn't open {}: permission denied",
                        what.to_lowercase()
                    )
                }
                Some(libc::ENXIO) => write!(
                    f,
                    "{what} must be a regular file, not a socket or unconnected device"
                ),
                _ => write!(f, "Couldn't open {}: {source}", what.to_lowercase()),
            },
            Self::SymbolicLink { what } => write!(
                f,
                "{what} is a symbolic link; choose the file it points to instead"
            ),
            Self::NotRegular { what, kind } => {
                write!(f, "{what} must be a regular file, not {kind}")
            }
            Self::TooLarge { what, limit } => {
                write!(f, "{what} exceeds the {} limit", limit_label(*limit))
            }
            Self::Read { what, source } => {
                write!(f, "Couldn't read {}: {source}", what.to_lowercase())
            }
            Self::NotUtf8 { what } => write!(f, "{what} is not valid UTF-8 text"),
        }
    }
}

impl std::error::Error for SafeFileError {}

impl From<SafeFileError> for String {
    fn from(error: SafeFileError) -> Self {
        error.to_string()
    }
}

fn kind_of(meta: &std::fs::Metadata) -> &'static str {
    let t = meta.file_type();
    if t.is_dir() {
        "a directory"
    } else if t.is_fifo() {
        "a named pipe"
    } else if t.is_socket() {
        "a socket"
    } else if t.is_char_device() || t.is_block_device() {
        "a device"
    } else if t.is_symlink() {
        "a symbolic link"
    } else {
        "a special file"
    }
}

fn open_error(what: &'static str, source: std::io::Error) -> SafeFileError {
    if source.raw_os_error() == Some(libc::ELOOP) {
        SafeFileError::SymbolicLink { what }
    } else {
        SafeFileError::Open { what, source }
    }
}

/// Validate an already-open descriptor: regular file within `limit`. The
/// nonblocking flag is cleared afterwards so ordinary reads behave normally.
fn check_opened(file: File, what: &'static str, limit: u64) -> Result<File, SafeFileError> {
    let meta = file
        .metadata()
        .map_err(|source| SafeFileError::Read { what, source })?;
    if !meta.is_file() {
        return Err(SafeFileError::NotRegular {
            what,
            kind: kind_of(&meta),
        });
    }
    if meta.len() > limit {
        return Err(SafeFileError::TooLarge { what, limit });
    }
    let fd = file.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags >= 0 {
        unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    }
    Ok(file)
}

/// Open `path` read-only without following a final symlink or blocking on a
/// special file, and require a regular file of at most `limit` bytes.
pub fn open_regular(path: &Path, what: &'static str, limit: u64) -> Result<File, SafeFileError> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(OPEN_FLAGS)
        .open(path)
        .map_err(|source| open_error(what, source))?;
    check_opened(file, what, limit)
}

/// Like [`open_regular`], but relative to an already-open directory so a
/// swapped path component cannot redirect the open.
pub fn open_regular_at(
    dir: &File,
    name: &OsStr,
    what: &'static str,
    limit: u64,
) -> Result<File, SafeFileError> {
    let cname = std::ffi::CString::new(name.as_bytes()).map_err(|_| SafeFileError::Open {
        what,
        source: std::io::Error::from(std::io::ErrorKind::InvalidInput),
    })?;
    let fd = unsafe { libc::openat(dir.as_raw_fd(), cname.as_ptr(), libc::O_RDONLY | OPEN_FLAGS) };
    if fd < 0 {
        return Err(open_error(what, std::io::Error::last_os_error()));
    }
    check_opened(unsafe { File::from_raw_fd(fd) }, what, limit)
}

/// Read an opened file to the end, refusing anything that grows past `limit`
/// while it is being read.
pub fn read_opened(file: File, what: &'static str, limit: u64) -> Result<Vec<u8>, SafeFileError> {
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| SafeFileError::Read { what, source })?;
    if bytes.len() as u64 > limit {
        return Err(SafeFileError::TooLarge { what, limit });
    }
    Ok(bytes)
}

/// Open safely and read the whole file.
pub fn read_file(path: &Path, what: &'static str, limit: u64) -> Result<Vec<u8>, SafeFileError> {
    read_opened(open_regular(path, what, limit)?, what, limit)
}

/// Open safely and read the whole file as UTF-8 text.
pub fn read_text(path: &Path, what: &'static str, limit: u64) -> Result<String, SafeFileError> {
    String::from_utf8(read_file(path, what, limit)?).map_err(|_| SafeFileError::NotUtf8 { what })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    pub(crate) fn scratch(tag: &str) -> std::path::PathBuf {
        use std::os::unix::fs::DirBuilderExt as _;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("wg-safe-{tag}-{}-{stamp}", std::process::id()));
        std::fs::DirBuilder::new().mode(0o700).create(&dir).unwrap();
        dir
    }

    fn mkfifo(path: &Path) {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
    }

    #[test]
    fn reads_regular_files_within_the_limit() {
        let dir = scratch("regular");
        let path = dir.join("a.conf");
        std::fs::write(&path, "hello").unwrap();
        assert_eq!(read_text(&path, "Config", MIB).unwrap(), "hello");
        let big = dir.join("big.conf");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MIB + 1)
            .unwrap();
        let error = read_text(&big, "Config", MIB).unwrap_err();
        assert!(error.to_string().contains("1 MiB"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn named_pipe_is_refused_without_waiting_for_a_writer() {
        let dir = scratch("fifo");
        let path = dir.join("pipe.png");
        mkfifo(&path);
        let start = Instant::now();
        let error = read_file(&path, "QR image", 32 * MIB).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(matches!(
            error,
            SafeFileError::NotRegular {
                kind: "a named pipe",
                ..
            }
        ));
        assert!(error.to_string().contains("regular file"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn directories_devices_and_sockets_are_not_regular_files() {
        let dir = scratch("special");
        // EISDIR-style input: a directory chosen as a file.
        let error = read_file(&dir, "Config", MIB).unwrap_err();
        assert!(matches!(
            error,
            SafeFileError::NotRegular {
                kind: "a directory",
                ..
            }
        ));
        let error = read_file(Path::new("/dev/null"), "Config", MIB).unwrap_err();
        assert!(matches!(
            error,
            SafeFileError::NotRegular {
                kind: "a device",
                ..
            }
        ));
        let socket = dir.join("sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let start = Instant::now();
        let error = read_file(&socket, "Config", MIB).unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(error.to_string().contains("regular file"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn symbolic_links_are_refused_not_followed() {
        let dir = scratch("link");
        let real = dir.join("real.conf");
        std::fs::write(&real, "secret").unwrap();
        let link = dir.join("link.conf");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let error = read_text(&link, "Config", MIB).unwrap_err();
        assert!(
            matches!(error, SafeFileError::SymbolicLink { .. }),
            "{error}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn permission_denied_is_reported_as_such() {
        use std::os::unix::fs::PermissionsExt as _;
        if unsafe { libc::geteuid() } == 0 {
            // root bypasses file permission bits; the helper-path test covers
            // EACCES for root via a non-executable helper instead.
            return;
        }
        let dir = scratch("perm");
        let path = dir.join("locked.conf");
        std::fs::write(&path, "x").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        let error = read_text(&path, "Config", MIB).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(libc::EACCES));
        assert!(error.to_string().contains("permission denied"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn relative_open_uses_the_directory_descriptor() {
        let dir = scratch("at");
        std::fs::write(dir.join("x.zip"), "zip").unwrap();
        let handle = std::fs::File::open(&dir).unwrap();
        let file = open_regular_at(&handle, OsStr::new("x.zip"), "Backup", MIB).unwrap();
        assert_eq!(read_opened(file, "Backup", MIB).unwrap(), b"zip");
        assert!(open_regular_at(&handle, OsStr::new("missing.zip"), "Backup", MIB).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_utf8_text_is_an_error() {
        let dir = scratch("utf8");
        let path = dir.join("bad.conf");
        std::fs::write(&path, [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_text(&path, "Config", MIB).unwrap_err(),
            SafeFileError::NotUtf8 { .. }
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
