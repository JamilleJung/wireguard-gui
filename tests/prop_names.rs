//! Property tests for tunnel-name validation and import-name sanitizing
//! (`src/validation.rs`), including agreement between the frontend validator
//! and the privileged helper's own name check (via the built `wg-helper`).

mod prop_support;
#[allow(dead_code)]
#[path = "../src/validation.rs"]
mod validation;

use prop_support::{Rng, check};
use std::process::{Command, Stdio};
use validation::{sanitize_import_name, validate_tunnel_name};

const NAME_CHARS: &[char] = &[
    'a', 'z', 'A', 'Z', '0', '9', '.', '_', '-', '/', '\\', ' ', '\t', '%', ':', 'é', '\u{0}',
    '\u{a0}', '*', '$',
];

fn name_like(rng: &mut Rng) -> String {
    let len = rng.below(20);
    let mut s: String = (0..len).map(|_| *rng.pick(NAME_CHARS)).collect();
    if rng.chance(1, 5) {
        s.push_str(rng.pick(&[".conf", ".CONF", "..", ".conf.", "/x"]));
    }
    s
}

fn safe_name(rng: &mut Rng) -> String {
    const FIRST: &[char] = &['a', 'q', 'Z', '0', '7'];
    const REST: &[char] = &['a', 'Z', '5', '_', '-', '.'];
    let mut s = String::new();
    s.push(*rng.pick(FIRST));
    for _ in 0..rng.below(15) {
        let c = *rng.pick(REST);
        if c == '.' && s.ends_with('.') {
            continue;
        }
        s.push(c);
    }
    while s.to_ascii_lowercase().ends_with(".conf") {
        s.truncate(s.len() - 5);
    }
    s
}

#[test]
fn sanitized_import_names_are_always_valid_and_idempotent() {
    check("sanitize", |rng| {
        let input = if rng.chance(1, 2) {
            rng.messy_text(40)
        } else {
            name_like(rng)
        };
        let out = sanitize_import_name(&input);
        if let Err(e) = validate_tunnel_name(&out) {
            panic!("{input:?} -> {out:?} is invalid: {e}");
        }
        assert_eq!(
            sanitize_import_name(&out),
            out,
            "not idempotent for {input:?}"
        );
    });
}

#[test]
fn accepted_names_cannot_escape_the_config_directory() {
    check("validate_contract", |rng| {
        let name = if rng.chance(1, 2) {
            name_like(rng)
        } else {
            rng.messy_text(20)
        };
        if validate_tunnel_name(&name).is_ok() {
            assert!(!name.is_empty() && name.len() <= 15, "{name:?}");
            assert!(name.is_ascii(), "{name:?}");
            assert!(name.as_bytes()[0].is_ascii_alphanumeric(), "{name:?}");
            assert!(!name.contains('/') && !name.contains('\\') && !name.contains(".."));
            assert!(!name.to_ascii_lowercase().ends_with(".conf"), "{name:?}");
            let path = std::path::Path::new("/etc/wireguard").join(format!("{name}.conf"));
            assert_eq!(path.parent(), Some(std::path::Path::new("/etc/wireguard")));
        }
    });
}

#[test]
fn safe_names_are_accepted_and_sanitize_to_a_prefix() {
    check("safe_names", |rng| {
        let name = safe_name(rng);
        if let Err(e) = validate_tunnel_name(&name) {
            panic!("{name:?} rejected: {e}");
        }
        let cleaned = sanitize_import_name(&name);
        assert!(name.starts_with(&cleaned), "{name:?} -> {cleaned:?}");
    });
}

/// The frontend validator and the helper's independent `name_ok` must agree;
/// a name the UI accepts but the helper refuses (or the reverse) is a bug.
/// `read` is read-only and validates before touching the filesystem.
#[test]
fn helper_and_frontend_agree_on_tunnel_names() {
    let helper = env!("CARGO_BIN_EXE_wg-helper");
    let cases = std::env::var("PROP_CASES")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map_or(150, |n| n.min(500));
    let mut rng = Rng::new(0x6e61_6d65);
    for case in 0..cases {
        let name = match case % 3 {
            0 => safe_name(&mut rng),
            1 => name_like(&mut rng),
            _ => rng.messy_text(18),
        };
        if name.contains('\0') {
            continue; // cannot be passed as an argument at all
        }
        let out = Command::new(helper)
            .args(["read", &name])
            .env_clear()
            .env("PATH", "/usr/sbin:/usr/bin:/sbin:/bin")
            .stdin(Stdio::null())
            .output()
            .expect("run wg-helper");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let helper_rejects = stderr.contains("invalid tunnel name");
        let frontend_rejects = validate_tunnel_name(&name).is_err();
        assert_eq!(
            helper_rejects, frontend_rejects,
            "disagreement on {name:?}: helper stderr {stderr:?}"
        );
        if helper_rejects {
            assert_eq!(out.status.code(), Some(2), "invalid name must exit 2");
            assert!(out.stdout.is_empty(), "no output on rejection");
        }
    }
}
