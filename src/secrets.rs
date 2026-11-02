/// Redact WireGuard secrets from config-like text before it reaches logs or
/// error messages.
pub fn redact_config(text: &str) -> String {
    text.lines()
        .map(|line| {
            // Match the privileged helper's redact(): substring-match the
            // lowercased line and, when a secret-bearing line has no '='
            // separator (e.g. a journal `PrivateKey: <key>` form), redact the
            // WHOLE line rather than letting it through. Keeps the two redactors
            // from diverging into a weaker one on the log path.
            let lower = line.to_ascii_lowercase();
            if lower.contains("key is not the correct length")
                || lower.contains("key is not correct length")
                || lower.contains("invalid private key")
                || lower.contains("invalid preshared key")
            {
                return "<redacted key error>".to_string();
            }
            if lower.contains("privatekey") || lower.contains("presharedkey") {
                match line.split_once('=') {
                    Some((key, _))
                        if matches!(
                            key.trim().to_ascii_lowercase().as_str(),
                            "privatekey" | "presharedkey"
                        ) =>
                    {
                        format!("{} = <redacted>", key.trim())
                    }
                    Some(_) => "<redacted secret line>".to_string(),
                    None => "<redacted secret line>".to_string(),
                }
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
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
    fn detects_root_script_hooks() {
        assert!(config_runs_scripts("PostUp = iptables -A OUTPUT -j DROP"));
        assert!(config_runs_scripts("preDown = true"));
        assert!(!config_runs_scripts("# PostUp = ignored"));
        assert!(!config_runs_scripts("PublicKey = abc"));
    }
}
