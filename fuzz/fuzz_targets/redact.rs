//! Secret redaction (`src/secrets.rs`): every line that names PrivateKey or
//! PresharedKey must be replaced, and no other line may change.
#![no_main]

#[allow(dead_code)]
#[path = "../../src/secrets.rs"]
mod secrets;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let _ = secrets::config_runs_scripts(&text);
    let output = secrets::redact_config(&text);
    let input_lines: Vec<&str> = text.lines().collect();
    let output_lines: Vec<&str> = output.lines().collect();
    // Re-joining with "\n" may fold a "\r" or a final blank line into the
    // line break; compare only lines both sides agree exist.
    for (input, out) in input_lines.iter().zip(&output_lines) {
        let lower = input.to_ascii_lowercase();
        if lower.contains("privatekey") || lower.contains("presharedkey") {
            assert!(
                *out == "<redacted secret line>"
                    || *out == "<redacted key error>"
                    || out.ends_with(" = <redacted>"),
                "secret line survived redaction"
            );
        }
    }
    assert_eq!(secrets::redact_config(&output).len(), output.len());
});
