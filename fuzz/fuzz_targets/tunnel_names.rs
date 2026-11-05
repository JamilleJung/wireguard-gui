//! Tunnel-name rules (`src/validation.rs`): sanitized import names are always
//! valid, and accepted names can never leave /etc/wireguard.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/validation.rs"]
mod validation;

use libfuzzer_sys::fuzz_target;
use validation::{sanitize_import_name, validate_tunnel_name};

fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    let sanitized = sanitize_import_name(&input);
    assert!(
        validate_tunnel_name(&sanitized).is_ok(),
        "sanitize produced an invalid name"
    );
    assert_eq!(
        sanitize_import_name(&sanitized),
        sanitized,
        "sanitize is not idempotent"
    );
    if validate_tunnel_name(&input).is_ok() {
        assert!(input.len() <= 15 && input.is_ascii());
        assert!(!input.contains('/') && !input.contains('\\') && !input.contains(".."));
        let path = std::path::Path::new("/etc/wireguard").join(format!("{input}.conf"));
        assert_eq!(path.parent(), Some(std::path::Path::new("/etc/wireguard")));
    }
});
