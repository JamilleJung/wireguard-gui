//! Reading a tunnel config from a QR-code image. The bounded image decode and
//! grid selection live in [`crate::import_parse`].

#[cfg(test)]
use crate::import_parse::select_qr_config;
use crate::safe_file;

const MAX_QR_FILE: u64 = 32 * safe_file::MIB;

/// Decode a QR-code image file into its text (a WireGuard `.conf`). The file
/// is opened through [`safe_file`], so a FIFO, socket, device or symlink is
/// refused without blocking, and only a bounded regular file is decoded.
/// Decoding runs in a child process ([`crate::qr_isolate`]) so a decoder crash
/// cannot abort the app.
pub fn decode_qr(path: &std::path::Path) -> Result<String, String> {
    let bytes = safe_file::read_file(path, "QR image", MAX_QR_FILE)?;
    crate::qr_isolate::decode(&bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    #[test]
    fn qr_selection_skips_bad_grids_and_refuses_ambiguous_tunnels() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        assert_eq!(
            select_qr_config([
                Err("decode failed".into()),
                Ok("unrelated QR".into()),
                Ok(valid.clone())
            ])
            .unwrap(),
            valid
        );
        assert!(
            select_qr_config([
                Ok(valid.clone()),
                Ok(format!("{valid}Address = 10.0.0.1/24\n"))
            ])
            .is_err()
        );
        assert_eq!(
            select_qr_config([Ok(valid.clone()), Ok(valid.clone())]).unwrap(),
            valid
        );
    }

    #[test]
    fn qr_decoder_rejects_oversized_dimensions() {
        let dir = crate::backend::tests::scratch_dir();
        let path = dir.join("wide.png");
        image::GrayImage::new(4097, 1).save(&path).unwrap();
        assert!(decode_qr(&path).unwrap_err().contains("bounded QR image"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn qr_import_never_hangs_on_special_files() {
        use std::os::unix::ffi::OsStrExt as _;
        let dir = crate::backend::tests::scratch_dir();
        let fifo = dir.join("camera.png");
        let c = std::ffi::CString::new(fifo.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        let error = decode_qr(&fifo).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(2));
        assert!(error.contains("regular file"), "{error}");
        assert!(
            decode_qr(std::path::Path::new("/dev/zero"))
                .unwrap_err()
                .contains("regular file")
        );
        assert!(decode_qr(&dir).unwrap_err().contains("regular file"));
        let big = dir.join("big.png");
        std::fs::File::create(&big)
            .unwrap()
            .set_len(MAX_QR_FILE + 1)
            .unwrap();
        assert!(decode_qr(&big).unwrap_err().contains("32 MiB"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
