//! The isolated QR decoder protocol of the real application binary: image
//! bytes on stdin, the decoded config on stdout, errors on stderr, and a
//! decoder crash confined to the child process.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_wireguard-gui");
const CHILD_ARG: &str = "--internal-decode-qr";
const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

fn run_child(bytes: &[u8]) -> std::process::Output {
    let mut child = Command::new(BIN)
        .arg(CHILD_ARG)
        .env("DISPLAY", "")
        .env("WAYLAND_DISPLAY", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn application binary");
    child.stdin.take().unwrap().write_all(bytes).unwrap();
    child.wait_with_output().unwrap()
}

fn qr_png(text: &str) -> Vec<u8> {
    let code = qrcode::QrCode::new(text.as_bytes()).unwrap();
    let width = code.width() as u32;
    let colors = code.to_colors();
    let (scale, border) = (8u32, 4u32);
    let size = (width + 2 * border) * scale;
    let img = image::GrayImage::from_fn(size, size, |x, y| {
        let (mx, my) = (
            (x / scale) as i64 - border as i64,
            (y / scale) as i64 - border as i64,
        );
        let dark = mx >= 0
            && my >= 0
            && (mx as u32) < width
            && (my as u32) < width
            && colors[(my as u32 * width + mx as u32) as usize] == qrcode::Color::Dark;
        image::Luma([if dark { 0 } else { 255 }])
    });
    let mut png = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

#[test]
fn child_decodes_a_tunnel_qr_code() {
    let config = format!("[Interface]\nPrivateKey = {KEY}\n");
    let out = run_child(&qr_png(&config));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8(out.stdout).unwrap(), config);
}

#[test]
fn child_reports_unreadable_images_as_errors() {
    let out = run_child(b"not an image");
    assert_eq!(out.status.code(), Some(1));
    assert!(!out.stderr.is_empty());
    assert!(out.stdout.is_empty());
}

/// A crafted image that trips an assertion inside the QR library. It may
/// abort the child; it must never produce a config, and only the child dies.
#[test]
fn decoder_assertion_is_confined_to_the_child() {
    let image = include_bytes!("data/qr-decoder-assert.png");
    let out = run_child(image);
    assert!(!out.status.success());
    assert!(out.stdout.is_empty());
}
