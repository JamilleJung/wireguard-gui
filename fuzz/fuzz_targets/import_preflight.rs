//! The checks every imported config passes through before it is offered for
//! saving (file import, backup restore preflight and QR selection in
//! `src/backend.rs` all call these): validation, root-hook detection, name
//! sanitizing and redaction of any error text.
//!
//! The ZIP/QR container parsing itself lives in private functions of
//! `src/backend.rs` and is not reachable without a library crate; see
//! CONTRIBUTING.md ("Fuzzing").
#![no_main]

#[allow(dead_code)]
#[path = "../../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../../src/secrets.rs"]
mod secrets;
#[allow(dead_code)]
#[path = "../../src/validation.rs"]
mod validation;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // First line: proposed file stem; the rest: config text.
    let raw = String::from_utf8_lossy(data);
    let (stem, body) = raw.split_once('\n').unwrap_or(("", &raw));
    let name = validation::sanitize_import_name(stem);
    assert!(validation::validate_tunnel_name(&name).is_ok());
    match config::validate_basic_wireguard_config(body) {
        Ok(()) => {
            let runs_scripts = secrets::config_runs_scripts(body);
            let parsed = config::parse_config(body);
            assert!(parsed.private_key.is_some());
            if runs_scripts {
                // A hook-bearing config must be visible as such after redaction
                // too, because the UI shows the redacted text in its warning.
                assert!(secrets::config_runs_scripts(&secrets::redact_config(body)));
            }
        }
        Err(message) => {
            // Error text may be shown and logged; it must never echo a secret.
            let redacted = secrets::redact_config(&message);
            assert!(!redacted.to_ascii_lowercase().contains("privatekey ="));
        }
    }
});
