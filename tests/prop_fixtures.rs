//! Contract tests over the scripted fixtures in `tests/fixtures/configs/`
//! (see `tests/fixtures/README.md`).

#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../src/secrets.rs"]
mod secrets;

use config::{parse_config, validate_basic_wireguard_config};
use std::path::{Path, PathBuf};

fn fixtures(kind: &str) -> Vec<(PathBuf, String)> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/configs")
        .join(kind);
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "conf"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no fixtures in {}", dir.display());
    files
        .into_iter()
        .map(|p| {
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(
                text.starts_with("# TEST FIXTURE ONLY"),
                "{} lacks the test-only header; regenerate with scripts/gen-fixtures.sh",
                p.display()
            );
            (p, text)
        })
        .collect()
}

#[test]
fn valid_fixtures_are_accepted_and_run_no_hooks() {
    for (path, text) in fixtures("valid") {
        if let Err(e) = validate_basic_wireguard_config(&text) {
            panic!("{} rejected: {e}", path.display());
        }
        assert!(!secrets::config_runs_scripts(&text), "{}", path.display());
        assert!(
            parse_config(&text).private_key.is_some(),
            "{}",
            path.display()
        );
    }
}

#[test]
fn hook_fixtures_parse_but_are_flagged() {
    for (path, text) in fixtures("hooks") {
        assert!(
            validate_basic_wireguard_config(&text).is_ok(),
            "{}",
            path.display()
        );
        assert!(secrets::config_runs_scripts(&text), "{}", path.display());
    }
}

#[test]
fn invalid_fixtures_are_rejected() {
    for (path, text) in fixtures("invalid") {
        assert!(
            validate_basic_wireguard_config(&text).is_err(),
            "{} was accepted",
            path.display()
        );
    }
}

#[test]
fn redaction_removes_every_fixture_secret() {
    for kind in ["valid", "hooks", "invalid"] {
        for (path, text) in fixtures(kind) {
            let parsed = parse_config(&text);
            let mut secrets_in: Vec<String> = parsed.private_key.into_iter().collect();
            secrets_in.extend(
                parsed
                    .peers
                    .into_iter()
                    .map(|p| p.preshared_key)
                    .filter(|k| k.len() >= 16),
            );
            let redacted = secrets::redact_config(&text);
            for secret in secrets_in.iter().filter(|s| s.len() >= 16) {
                assert!(!redacted.contains(secret.as_str()), "{}", path.display());
            }
        }
    }
}
