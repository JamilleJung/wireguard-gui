//! Config parsing and validation (`src/config.rs`) must never panic, and any
//! config the validator accepts must satisfy what its consumers rely on.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;

use config::{is_endpoint, is_wg_key, looks_like_inet, parse_config};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let parsed = parse_config(&text);
    for line in text.lines() {
        let _ = is_endpoint(line);
        let _ = looks_like_inet(line);
        let _ = is_wg_key(line);
    }
    if config::validate_basic_wireguard_config(&text).is_ok() {
        let key = parsed
            .private_key
            .as_deref()
            .expect("accepted without PrivateKey");
        assert!(is_wg_key(key), "accepted malformed PrivateKey");
        for peer in &parsed.peers {
            assert!(is_wg_key(&peer.public_key), "accepted malformed PublicKey");
            assert!(
                !peer.allowed_ips.is_empty(),
                "accepted peer without AllowedIPs"
            );
            assert!(peer.allowed_ips.split(',').all(looks_like_inet));
            assert!(peer.endpoint.is_empty() || is_endpoint(&peer.endpoint));
            assert!(peer.preshared_key.is_empty() || is_wg_key(&peer.preshared_key));
        }
        assert!(
            !text
                .bytes()
                .any(|b| b == 0 || (b.is_ascii_control() && !matches!(b, b'\t' | b'\r' | b'\n'))),
            "accepted control characters"
        );
    }
});
