//! Property tests for secret redaction and root-hook detection
//! (`src/secrets.rs`): no PrivateKey/PresharedKey value may survive
//! `redact_config`, whatever the casing, spacing, separator or log prefix.

mod prop_support;
#[allow(dead_code)]
#[path = "../src/secrets.rs"]
mod secrets;

use prop_support::{Rng, check, wg_key};
use secrets::{config_runs_scripts, redact_config};

/// A line that carries `secret` the way configs, `wg showconf` output, error
/// messages and journal entries spell it.
fn secret_line(rng: &mut Rng, secret: &str) -> String {
    let name = rng_pick_name(rng);
    let name = rng.recase(name);
    let (a, b) = (rng.spaces(), rng.spaces());
    match rng.below(8) {
        0..=2 => format!("{a}{name}{b}={}{secret}", rng.spaces()),
        3 => format!("{name}: {secret}"),
        4 => format!("journal: {name} {secret}"),
        5 => format!("error={name}: {secret}"),
        6 => format!(
            "wg-quick[{}]: {name} = {secret} # from file",
            rng.below(99_999)
        ),
        _ => format!("{name}={secret}=trailing"),
    }
}

fn rng_pick_name(rng: &mut Rng) -> &'static str {
    *rng.pick(&["PrivateKey", "PresharedKey"])
}

fn public_line(rng: &mut Rng, key: &str) -> String {
    let directive = *rng.pick(&["PublicKey", "Endpoint", "AllowedIPs", "Address", "DNS"]);
    format!("{} = {key}", rng.recase(directive))
}

/// `redact_config` works line by line and re-joins with "\n", so CRLF pairs and
/// trailing blank lines are normalized by design. Remove them up front so the
/// properties below compare content, not line-break spelling. Lone "\r"
/// characters inside a line are kept: they must not hide a secret.
fn normalize_breaks(text: &str) -> String {
    let mut out = text.to_string();
    while out.contains("\r\n") {
        out = out.replace("\r\n", "\n");
    }
    out.trim_end_matches('\n').to_string()
}

const ERROR_LINES: &[&str] = &[
    "Key is not the correct length or format: ",
    "key is not correct length: ",
    "Invalid private key: ",
    "Invalid preshared key ",
];

#[test]
fn no_secret_value_survives_redaction() {
    check("redaction", |rng| {
        let mut lines = Vec::new();
        let mut secrets_in = Vec::new();
        let mut publics = Vec::new();
        for _ in 0..1 + rng.below(12) {
            match rng.below(4) {
                0 | 1 => {
                    let secret = wg_key(rng);
                    lines.push(secret_line(rng, &secret));
                    secrets_in.push(secret);
                }
                2 => {
                    let secret = wg_key(rng);
                    lines.push(format!("{}{secret}", rng.pick(ERROR_LINES)));
                    secrets_in.push(secret);
                }
                _ => {
                    let key = wg_key(rng);
                    lines.push(public_line(rng, &key));
                    publics.push((lines.len() - 1, key));
                }
            }
            if rng.chance(1, 5) {
                lines.push(
                    rng.pick(&["", "[Peer]", "[Interface]", "# comment"])
                        .to_string(),
                );
            }
        }
        let sep = *rng.pick(&["\n", "\r\n"]);
        let input = lines.join(sep);
        let output = redact_config(&input);

        for secret in &secrets_in {
            assert!(
                !output.contains(secret.as_str()) && !output.contains(&secret[..43]),
                "secret survived redaction\ninput:\n{input}\noutput:\n{output}"
            );
        }
        // Redaction must not swallow the surrounding non-secret lines.
        let out_lines: Vec<&str> = output.lines().collect();
        assert_eq!(out_lines.len(), input.lines().count(), "line count changed");
        for (index, key) in publics {
            assert!(
                out_lines[index].contains(key.as_str()),
                "over-redacted {:?}",
                lines[index]
            );
        }
    });
}

#[test]
fn secret_bearing_lines_are_always_replaced_for_arbitrary_text() {
    check("redaction_arbitrary", |rng| {
        let mut text = rng.messy_text(200);
        if rng.chance(1, 2) {
            let at = rng.below(text.chars().count() + 1);
            let byte = text.char_indices().nth(at).map_or(text.len(), |(i, _)| i);
            let secret = wg_key(rng);
            let line = secret_line(rng, &secret);
            text.insert_str(byte, &line);
        }
        let text = normalize_breaks(&text);
        let output = redact_config(&text);
        let out_lines: Vec<&str> = output.lines().collect();
        let in_lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            out_lines.len(),
            in_lines.len(),
            "line count changed for {text:?}"
        );
        for (input, out) in in_lines.iter().zip(&out_lines) {
            let lower = input.to_ascii_lowercase();
            if lower.contains("privatekey") || lower.contains("presharedkey") {
                assert!(
                    *out == "<redacted secret line>"
                        || *out == "<redacted key error>"
                        || out.ends_with(" = <redacted>"),
                    "secret line not replaced: {input:?} -> {out:?}"
                );
            } else if !lower.contains("key is not")
                && !lower.contains("invalid private key")
                && !lower.contains("invalid preshared key")
            {
                // `lines()` folds a trailing "\r" into the line break once the
                // output is re-joined with "\n"; that is not a content change.
                assert_eq!(
                    input.trim_end_matches('\r'),
                    out.trim_end_matches('\r'),
                    "non-secret line changed"
                );
            }
        }
    });
}

#[test]
fn redaction_is_idempotent() {
    check("redaction_idempotent", |rng| {
        let mut text = rng.messy_text(120);
        text.push('\n');
        let secret = wg_key(rng);
        text.push_str(&secret_line(rng, &secret));
        let text = normalize_breaks(&text);
        let once = redact_config(&text);
        assert_eq!(redact_config(&once), once);
    });
}

#[test]
fn hook_directives_are_detected_in_any_case_and_comments_are_not() {
    check("hooks", |rng| {
        let hook = *rng.pick(&["PreUp", "PostUp", "PreDown", "PostDown"]);
        let hook = rng.recase(hook);
        let base = format!(
            "[Interface]\nPrivateKey = {}\nAddress = 10.0.0.2/32\n",
            wg_key(rng)
        );
        let command = rng.pick(&["true", "iptables -F", "sh -c 'id'", ""]);
        let active = format!(
            "{base}{}{hook}{}={}{command}\n",
            rng.spaces(),
            rng.spaces(),
            rng.spaces()
        );
        assert!(config_runs_scripts(&active), "{active}");
        let commented = format!("{base}{}# {hook} = {command}\n", rng.spaces());
        assert!(!config_runs_scripts(&commented), "{commented}");
        assert!(!config_runs_scripts(&base));
    });
}
