const MAX_TUNNEL_NAME_LEN: usize = 15;

/// Validate the user-facing tunnel/interface name accepted by the helper.
///
/// Linux interface names are limited to 15 bytes. This project intentionally
/// keeps names simple so every privileged path resolves to
/// `/etc/wireguard/<name>.conf` without ambiguity.
pub fn validate_tunnel_name(name: &str) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Tunnel name is required.".into());
    }
    if name.len() > MAX_TUNNEL_NAME_LEN {
        return Err("Tunnel name must be 15 characters or fewer.".into());
    }
    if name.eq(".") || name.eq("..") || name.contains("..") {
        return Err("Tunnel name must not contain path traversal.".into());
    }
    if name.contains('/') || name.contains('\\') || name.starts_with('/') {
        return Err("Tunnel name must not contain path separators.".into());
    }
    if name.to_ascii_lowercase().ends_with(".conf") {
        return Err("Use the tunnel name without the .conf suffix.".into());
    }
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return Err("Tunnel name is required.".into());
    };
    if !first.is_ascii_alphanumeric() {
        return Err("Tunnel name must start with a letter or number.".into());
    }
    if !chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.') {
        return Err("Tunnel name may contain only letters, numbers, '.', '_', and '-'.".into());
    }
    Ok(())
}

/// Make a safe tunnel/interface name from an imported file stem.
pub fn sanitize_import_name(file_stem: &str) -> String {
    let mut stem = file_stem.trim();
    while stem.to_ascii_lowercase().ends_with(".conf") {
        stem = &stem[..stem.len() - 5];
        stem = stem.trim_end_matches('.');
    }
    let cleaned: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let truncated: String = cleaned.chars().take(MAX_TUNNEL_NAME_LEN).collect();
    let trimmed = truncated
        .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
        .trim_end_matches('.');
    if trimmed.is_empty() {
        "tunnel".to_string()
    } else if trimmed.to_ascii_lowercase().ends_with(".conf") {
        trimmed[..trimmed.len() - 5]
            .trim_end_matches('.')
            .to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{sanitize_import_name, validate_tunnel_name};

    #[test]
    fn validates_safe_tunnel_names() {
        for name in ["wg0", "work-vpn", "home.vpn", "office_1"] {
            assert!(validate_tunnel_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn rejects_unsafe_tunnel_names() {
        for name in [
            "",
            ".",
            "..",
            "../x",
            "/tmp/x",
            "wg/evil",
            "wg\\evil",
            "bad..name",
            "-bad",
            "name.conf",
            "abcdefghijklmnop",
        ] {
            assert!(validate_tunnel_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn sanitizes_import_names_without_conf_conf_confusion() {
        assert_eq!(sanitize_import_name("home"), "home");
        assert_eq!(sanitize_import_name("home server"), "home_server");
        assert_eq!(sanitize_import_name("name.conf"), "name");
        assert_eq!(sanitize_import_name("name.conf.conf"), "name");
        assert_eq!(sanitize_import_name("@#$"), "tunnel");
        assert_eq!(sanitize_import_name("___abc"), "abc");
        assert_eq!(sanitize_import_name("a.b.c."), "a.b.c");
        let long = sanitize_import_name("averylongtunnelname1234567");
        assert!(long.chars().count() <= 15);
    }
}
