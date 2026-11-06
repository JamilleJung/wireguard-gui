/// Redact WireGuard secrets from config-like text before it reaches logs or
/// error messages.
///
/// Line terminators are kept as they are, so redacting twice gives the same
/// text. A `Key = value` line keeps its key and loses only its value, so a
/// redacted preview still shows which directives a config contains (a root
/// `PostUp` hook stays visible even when its command mentions a key).
pub fn redact_config(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for chunk in text.split_inclusive('\n') {
        let (line, ending) = match chunk.strip_suffix('\n') {
            Some(line) => match line.strip_suffix('\r') {
                Some(line) => (line, "\r\n"),
                None => (line, "\n"),
            },
            None => (chunk, ""),
        };
        out.push_str(&redact_line(line));
        out.push_str(ending);
    }
    out
}

/// A config directive name: letters and digits only (`PrivateKey`, `PostUp`).
fn directive_key(line: &str) -> Option<&str> {
    let (key, _) = line.split_once('=')?;
    let key = key.trim();
    (!key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric())).then_some(key)
}

fn redact_line(line: &str) -> String {
    // Substring-match the lowercased line: a secret-bearing line in another
    // shape (e.g. a journal `PrivateKey: <key>` form) is redacted whole rather
    // than let through. Keeps the frontend and helper redactors identical.
    let lower = line.to_ascii_lowercase();
    let key_error = lower.contains("key is not the correct length")
        || lower.contains("key is not correct length")
        || lower.contains("invalid private key")
        || lower.contains("invalid preshared key");
    let secret = lower.contains("privatekey") || lower.contains("presharedkey");
    if !key_error && !secret {
        return line.to_string();
    }
    match directive_key(line) {
        Some(key) => format!("{key} = <redacted>"),
        None if key_error => "<redacted key error>".to_string(),
        None => "<redacted secret line>".to_string(),
    }
}

/// True if the config contains directives that `wg-quick` runs as root on
/// activation.
pub fn config_runs_scripts(text: &str) -> bool {
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let k = key.trim().to_ascii_lowercase();
            if matches!(k.as_str(), "postup" | "preup" | "postdown" | "predown") {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{config_runs_scripts, redact_config};

    #[test]
    fn secret_before_an_equals_in_a_log_prefix_is_redacted() {
        for line in [
            "PrivateKey: SECRET = metadata",
            "journal: PresharedKey SECRET=value",
            "error=PrivateKey: SECRET",
            "Key is not the correct length or format: SECRET",
            "Invalid private key: SECRET",
        ] {
            assert!(!redact_config(line).contains("SECRET"), "{line}");
        }
    }

    #[test]
    fn redacts_private_and_preshared_keys() {
        let input = "PrivateKey = abc\nPresharedKey = xyz\nPublicKey = pub";
        let output = redact_config(input);
        assert!(output.contains("PrivateKey = <redacted>"));
        assert!(output.contains("PresharedKey = <redacted>"));
        assert!(output.contains("PublicKey = pub"));
        assert!(!output.contains("abc"));
        assert!(!output.contains("xyz"));
    }

    #[test]
    fn redaction_keeps_line_endings_and_is_idempotent() {
        for input in [
            "",
            "\n\n",
            "a\n\n",
            "PrivateKey = abc\r\nok\n",
            "PrivateKey: SECRET\n\n\n",
            "no newline at end",
        ] {
            let once = redact_config(input);
            assert_eq!(redact_config(&once), once, "{input:?}");
            assert_eq!(once.matches('\n').count(), input.matches('\n').count());
        }
    }

    #[test]
    fn redaction_never_hides_a_root_hook() {
        // Found by fuzzing: a hook whose command mentions a key error used to
        // become "<redacted key error>", hiding the PostUp line in previews.
        let config = "[Interface]\nPrivateKey = abc\nPostUp = echo invalid private key; id\n\
                      PreDown = cat /tmp/PrivateKey\n";
        let redacted = redact_config(config);
        assert!(config_runs_scripts(&redacted), "{redacted}");
        assert!(redacted.contains("PostUp = <redacted>"));
        assert!(redacted.contains("PreDown = <redacted>"));
        assert!(!redacted.contains("invalid private key"));
        assert!(!redacted.contains("/tmp/PrivateKey"));
    }

    #[test]
    fn detects_root_script_hooks() {
        assert!(config_runs_scripts("PostUp = iptables -A OUTPUT -j DROP"));
        assert!(config_runs_scripts("preDown = true"));
        assert!(!config_runs_scripts("# PostUp = ignored"));
        assert!(!config_runs_scripts("PublicKey = abc"));
    }
}
