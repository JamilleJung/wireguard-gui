/// Redact WireGuard secrets from config-like text before it reaches logs or
/// error messages.
pub fn redact_config(text: &str) -> String {
    text.lines()
        .map(|line| {
            let Some((key, _)) = line.split_once('=') else {
                return line.to_string();
            };
            let trimmed = key.trim();
            if matches!(
                trimmed.to_ascii_lowercase().as_str(),
                "privatekey" | "presharedkey"
            ) {
                format!("{trimmed} = <redacted>")
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
