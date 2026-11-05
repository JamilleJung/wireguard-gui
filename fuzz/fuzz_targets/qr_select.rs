//! QR import (`src/import_parse.rs`): selecting one tunnel config among
//! decoded grids, and the bounded image decode path.
//!
//! The first byte picks the mode:
//! * `0x89` or `0xFF` (PNG / JPEG magic): the whole input is an image file
//!   for the bounded decoder; images of at most 512x512 pixels then go
//!   through grid detection too, larger ones only through the decoder;
//! * `0x01`: a raw grayscale image (width, height, pixels) for grid detection;
//! * anything else: NUL-separated decoded grid contents (a segment starting
//!   with `0x01` is a grid that failed to decode), checked against a model of
//!   the selection rule.
//!
//! Grid detection (`rqrr`) is only fuzzed with `--features detect`: rqrr 0.11
//! asserts on some crafted grids (`Perspective::map`), which is why the apps
//! decode QR images in a child process (`src/qr_isolate.rs`). Without the
//! feature the target covers everything the apps run in-process.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;
#[path = "common/heap.rs"]
mod heap;
#[allow(dead_code)]
#[path = "../../src/import_parse.rs"]
mod import_parse;
#[allow(dead_code)]
#[path = "../../src/secrets.rs"]
mod secrets;
#[allow(dead_code)]
#[path = "../../src/validation.rs"]
mod validation;

use import_parse::{MAX_QR_DECODE_ALLOC, MAX_QR_DIMENSION};
use libfuzzer_sys::fuzz_target;

const DETECT_PIXELS: u32 = 512 * 512;

fn acceptable(content: &str) -> bool {
    config::validate_basic_wireguard_config(content).is_ok()
        && !secrets::config_runs_scripts(content)
}

fn check_selected(result: Result<String, String>) {
    if let Ok(content) = result {
        assert!(acceptable(&content), "selected an unusable config");
    }
}

fn grids(data: &[u8]) {
    let grids: Vec<Result<String, String>> = data
        .split(|&b| b == 0)
        .map(|segment| match segment.split_first() {
            Some((1, rest)) => Err(String::from_utf8_lossy(rest).into_owned()),
            _ => Ok(String::from_utf8_lossy(segment).into_owned()),
        })
        .collect();
    let mut distinct: Vec<&String> = Vec::new();
    for content in grids.iter().flatten() {
        if acceptable(content) && !distinct.contains(&content) {
            distinct.push(content);
        }
    }
    match (
        import_parse::select_qr_config(grids.clone()),
        distinct.len(),
    ) {
        (Ok(selected), 1) => assert_eq!(&selected, distinct[0]),
        (Err(error), 0) => assert!(error.contains("No valid"), "{error}"),
        (Err(error), _) if distinct.len() > 1 => {
            assert!(error.contains("multiple different"), "{error}");
        }
        (result, n) => panic!("selection {result:?} with {n} distinct configs"),
    }
}

fn image_file(data: &[u8]) {
    let (decoded, peak) = heap::measure(|| import_parse::decode_qr_image(data));
    let side = MAX_QR_DIMENSION as usize;
    // Decoder buffers are capped by its allocation limit; the grayscale copy
    // adds one byte per pixel.
    let budget = MAX_QR_DECODE_ALLOC as usize + 2 * side * side + 8 * 1024 * 1024;
    assert!(peak <= budget, "bounded decode held {peak} bytes");
    let Ok(image) = decoded else {
        return;
    };
    assert!(image.width() <= MAX_QR_DIMENSION && image.height() <= MAX_QR_DIMENSION);
    if cfg!(feature = "detect") && image.width() * image.height() <= DETECT_PIXELS {
        check_selected(import_parse::qr_config_from_image(image));
    }
}

fn raw_image(data: &[u8]) {
    if !cfg!(feature = "detect") {
        return;
    }
    let [width, height, pixels @ ..] = data else {
        return;
    };
    let (width, height) = (u32::from(*width) + 1, u32::from(*height) + 1);
    let image = image::GrayImage::from_fn(width, height, |x, y| {
        let index = (y * width + x) as usize;
        image::Luma([pixels.get(index).copied().unwrap_or(255)])
    });
    check_selected(import_parse::qr_config_from_image(image));
}

fuzz_target!(|data: &[u8]| {
    match data.first() {
        Some(0x89 | 0xFF) => image_file(data),
        Some(0x01) => raw_image(&data[1..]),
        _ => grids(data),
    }
});
