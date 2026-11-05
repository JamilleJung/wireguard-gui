//! Structured audit records sent to syslog/journald through `logger`.
//!
//! Every record is one line of `key=value` pairs. Values are quoted when they
//! contain anything outside a conservative character set and every record is
//! passed through the config redactor, so a key that reaches an error message
//! can never be written to the journal.

use std::time::Duration;

use crate::process::command_output;
use crate::secrets;

fn caller() -> String {
    std::env::var("SUDO_USER")
        .ok()
        .or_else(|| {
            std::env::var("PKEXEC_UID")
                .ok()
                .map(|uid| format!("uid:{uid}"))
        })
        .unwrap_or_else(|| "unknown".to_string())
}

fn plain(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-:/@+,".contains(&b))
}

fn quote(value: &str) -> String {
    let value = &secrets::redact_config(value);
    if plain(value) {
        return value.to_string();
    }
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Render one record; every value is redacted before it is quoted, and the
/// quoting escapes newlines so a record is always one journal line. Kept separate from [`event`] so it can be tested.
pub fn record(event: &str, tunnel: Option<&str>, fields: &[(&str, &str)], user: &str) -> String {
    let mut line = format!("event={} user={}", quote(event), quote(user));
    if let Some(tunnel) = tunnel {
        line.push_str(&format!(" tunnel={}", quote(tunnel)));
    }
    for (key, value) in fields {
        line.push_str(&format!(" {key}={}", quote(value)));
    }
    line
}

pub fn event(event: &str, tunnel: Option<&str>, fields: &[(&str, &str)]) {
    let msg = record(event, tunnel, fields, &caller());
    let _ = command_output(
        "logger",
        &["-t", env!("CARGO_PKG_NAME"), "--", &msg],
        None,
        Duration::from_secs(2),
    );
}

/// Record the outcome of a mutating verb.
pub fn outcome(verb: &str, names: &[String], result: &Result<(), String>) {
    let tunnel = names.first().map(String::as_str);
    let target = names.get(1).map(String::as_str);
    let mut fields = Vec::new();
    if let Some(target) = target {
        fields.push(("target", target));
    }
    match result {
        Ok(()) => fields.push(("result", "ok")),
        Err(error) => {
            fields.push(("result", "error"));
            fields.push(("error", error.as_str()));
        }
    }
    event(verb, tunnel, &fields);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_single_line_key_value_pairs() {
        let line = record(
            "save",
            Some("wg0"),
            &[("result", "error"), ("error", "bad\nthing \"quoted\"")],
            "alice",
        );
        assert_eq!(
            line,
            "event=save user=alice tunnel=wg0 result=error error=\"bad\\nthing \\\"quoted\\\"\""
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn records_never_contain_private_or_preshared_keys() {
        let key = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";
        let leaked = format!("[Interface]\nPrivateKey = {key}\n[Peer]\nPresharedKey = {key}");
        let line = record("save", Some("wg0"), &[("error", &leaked)], "bob");
        assert!(!line.contains(key), "{line}");
    }
}
