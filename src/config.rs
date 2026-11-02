#[derive(Default)]
pub struct ParsedPeer {
    pub public_key: String,
    pub preshared_key: String,
    pub allowed_ips: String,
    pub endpoint: String,
    pub keepalive: String,
}

#[derive(Default)]
pub struct ParsedConfig {
    pub private_key: Option<String>,
    pub address: Option<String>,
    pub dns: Option<String>,
    pub listen_port: Option<String>,
    pub peers: Vec<ParsedPeer>,
}

/// Combine repeated multi-valued keys (Address/DNS/AllowedIPs) so two separate
/// lines — e.g. an IPv4 and an IPv6 `Address` — are both kept (matching how
/// `wg-quick` treats repeated directives) instead of last-write-wins.
fn join_values(existing: Option<String>, value: String) -> String {
    match existing {
        Some(prev) if !prev.is_empty() => format!("{prev}, {value}"),
        _ => value,
    }
}

pub fn parse_config(text: &str) -> ParsedConfig {
    let mut cfg = ParsedConfig::default();
    let mut section = "";
    for raw in text.lines() {
        let line = raw
            .split_once('#')
            .map_or(raw, |(line, _)| line)
            .trim_ascii();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let s = line
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or("");
            if s.eq_ignore_ascii_case("Peer") {
                cfg.peers.push(ParsedPeer::default());
                section = "peer";
            } else if s.eq_ignore_ascii_case("Interface") {
                section = "interface";
            } else {
                section = "";
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim_ascii();
        let value = value.trim_ascii().to_string();
        match section {
            "interface" => match key.to_ascii_lowercase().as_str() {
                "privatekey" => cfg.private_key = Some(value),
                "address" => cfg.address = Some(join_values(cfg.address.take(), value)),
                "dns" => cfg.dns = Some(join_values(cfg.dns.take(), value)),
                "listenport" => cfg.listen_port = Some(value),
                _ => {}
            },
            "peer" => {
                if let Some(p) = cfg.peers.last_mut() {
                    match key.to_ascii_lowercase().as_str() {
                        "publickey" => p.public_key = value,
                        "presharedkey" => p.preshared_key = value,
                        "allowedips" => {
                            let prev = std::mem::take(&mut p.allowed_ips);
                            p.allowed_ips = join_values((!prev.is_empty()).then_some(prev), value);
                        }
                        "endpoint" => p.endpoint = value,
                        "persistentkeepalive" => p.keepalive = value,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    cfg
}

pub fn private_key(text: &str) -> Option<String> {
    parse_config(text).private_key
}

/// A WireGuard key is base64 of 32 bytes: 43 chars + one '=' padding.
pub fn is_wg_key(s: &str) -> bool {
    let s = s.trim_ascii();
    let bytes = s.as_bytes();
    bytes.len() == 44
        && bytes[43] == b'='
        && bytes[..43].iter().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/'))
        // 32 bytes leave two padding bits in the final base64 character. wg
        // rejects nonzero bits even when the text has the right length/alphabet.
        && b"AEIMQUYcgkosw048".contains(&bytes[42])
}

/// A syntactically valid DNS hostname (RFC 1123 labels).
fn is_hostname(host: &str) -> bool {
    let host = host.strip_suffix('.').unwrap_or(host); // tolerate one trailing dot (FQDN)
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    // A host that is only digits and dots is a malformed IPv4 literal, not a
    // hostname (a valid one would have parsed as Ipv4Addr already).
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        return false;
    }
    host.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// host:port, including bracketed IPv6 `[::1]:51820`.
pub fn is_endpoint(s: &str) -> bool {
    let s = s.trim_ascii();
    // Reject a leading '+'/'-'/whitespace that `str::parse` would otherwise
    // accept; a port must be plain ASCII digits in 1..=65535.
    let port_ok = |port: &str| {
        !port.is_empty()
            && port.bytes().all(|b| b.is_ascii_digit())
            && matches!(port.parse::<u32>(), Ok(p) if (1..=65535).contains(&p))
    };

    if let Some(rest) = s.strip_prefix('[') {
        let Some((inner, after)) = rest.split_once(']') else {
            return false;
        };
        let Some(port) = after.strip_prefix(':') else {
            return false;
        };
        let (address, zone) = inner
            .split_once('%')
            .map_or((inner, None), |(address, zone)| (address, Some(zone)));
        let zone_ok = zone.is_none_or(|zone| {
            !zone.is_empty()
                && zone.len() <= 15
                && !zone.chars().any(|c| {
                    c.is_whitespace() || c.is_control() || matches!(c, '%' | '[' | ']' | ':' | '/')
                })
        });
        return address.parse::<std::net::Ipv6Addr>().is_ok() && zone_ok && port_ok(port);
    }

    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    if host.contains(':') {
        return false;
    }
    let host_ok = host.parse::<std::net::Ipv4Addr>().is_ok() || is_hostname(host);
    host_ok && port_ok(port)
}

/// A CIDR / address check via real parsing.
pub fn looks_like_inet(s: &str) -> bool {
    let s = s.trim_ascii();
    let (addr, prefix) = match s.split_once('/') {
        Some((a, p)) => (a, Some(p)),
        None => (s, None),
    };
    let ip: std::net::IpAddr = match addr.parse() {
        Ok(ip) => ip,
        Err(_) => return false,
    };
    match prefix {
        None => true,
        Some(p) if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) => false,
        Some(p) => match p.parse::<u8>() {
            Ok(n) => match ip {
                std::net::IpAddr::V4(_) => n <= 32,
                std::net::IpAddr::V6(_) => n <= 128,
            },
            Err(_) => false,
        },
    }
}

fn decimal(value: &str, max: u32) -> Option<u32> {
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u32>().ok().filter(|n| *n <= max)
}

/// Validate obvious WireGuard config errors before handing text to `wg-quick`.
///
/// This deliberately stays permissive: Address and Peer sections are allowed to
/// be absent for interface-only setups, but if fields are present they must be
/// shaped like WireGuard expects.
pub fn validate_basic_wireguard_config(text: &str) -> Result<(), String> {
    if text.len() > 1024 * 1024
        || text
            .bytes()
            .any(|b| b == 0 || (b.is_ascii_control() && !matches!(b, b'\t' | b'\r' | b'\n')))
    {
        return Err("Config is too large or contains invalid control characters.".into());
    }
    // Validate each occurrence before scalar values are folded by parse_config.
    // This keeps whole-archive preflight consistent with helper validation.
    let mut section = "";
    let mut have_interface = false;
    for (index, raw) in text.lines().enumerate() {
        let line = raw
            .split_once('#')
            .map_or(raw, |(line, _)| line)
            .trim_ascii();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            let header = line
                .strip_prefix('[')
                .and_then(|s| s.strip_suffix(']'))
                .unwrap_or("");
            if header.eq_ignore_ascii_case("Interface") {
                have_interface = true;
                section = "interface";
            } else if header.eq_ignore_ascii_case("Peer") {
                section = "peer";
            } else {
                return Err(format!("Invalid config section on line {}.", index + 1));
            }
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("Expected a config directive on line {}.", index + 1))?;
        let key = key.trim_ascii().to_ascii_lowercase();
        let value = value.trim_ascii();
        let valid = match (section, key.as_str()) {
            ("interface", "privatekey") | ("peer", "publickey") => is_wg_key(value),
            ("peer", "presharedkey") => is_wg_key(value),
            ("interface", "address") => {
                let addresses: Vec<_> = value
                    .split(|c: char| c == ',' || c.is_ascii_whitespace())
                    .filter(|v| !v.is_empty())
                    .collect();
                !addresses.is_empty() && addresses.into_iter().all(looks_like_inet)
            }
            ("peer", "allowedips") => !value.is_empty() && value.split(',').all(looks_like_inet),
            ("interface", "listenport") => decimal(value, 65535).is_some(),
            ("peer", "persistentkeepalive") => {
                value.eq_ignore_ascii_case("off") || decimal(value, 65535).is_some()
            }
            ("peer", "endpoint") => is_endpoint(value),
            ("interface", "mtu") => {
                value.is_empty() || decimal(value, 65535).is_some_and(|n| n > 0)
            }
            ("interface", "fwmark") => {
                value.eq_ignore_ascii_case("off")
                    || decimal(value, u32::MAX).is_some()
                    || value.strip_prefix("0x").is_some_and(|hex| {
                        !hex.is_empty()
                            && hex.bytes().all(|b| b.is_ascii_hexdigit())
                            && u32::from_str_radix(hex, 16).is_ok()
                    })
            }
            ("interface", "saveconfig") => {
                value.eq_ignore_ascii_case("true") || value.eq_ignore_ascii_case("false")
            }
            ("interface", "table") => value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')),
            // wg-quick splits DNS into servers/search domains on commas and
            // whitespace; an empty directive is a valid no-op.
            ("interface", "dns") => true,
            // Frontends may display/admin-edit hooks, while the privileged
            // helper rejects them independently before any managed write.
            ("interface", "preup" | "postup" | "predown" | "postdown") => true,
            _ => {
                return Err(format!(
                    "Unknown or misplaced config directive on line {}.",
                    index + 1
                ));
            }
        };
        if !valid {
            return Err(format!(
                "Invalid {key} in [{section}] on line {}.",
                index + 1
            ));
        }
    }
    if !have_interface {
        return Err("Missing an [Interface] section.".into());
    }

    let cfg = parse_config(text);

    if cfg.private_key.is_none() {
        return Err("[Interface] is missing PrivateKey.".into());
    }
    for (index, peer) in cfg.peers.iter().enumerate() {
        if peer.public_key.is_empty() {
            return Err(format!("Peer {} is missing PublicKey.", index + 1));
        }
        if peer.allowed_ips.is_empty() {
            return Err(format!("Peer {} is missing AllowedIPs.", index + 1));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    #[test]
    fn unicode_whitespace_cannot_be_validated_then_passed_to_ascii_tools() {
        for line in [
            format!("\u{2003}PrivateKey = {KEY}"),
            format!("PrivateKey = {KEY}\u{a0}"),
            format!("PrivateKey\u{a0} = {KEY}"),
        ] {
            assert!(validate_basic_wireguard_config(&format!("[Interface]\n{line}\n")).is_err());
        }
        let commented =
            format!("[Interface] # บ้าน\nPrivateKey = {KEY} # comment\nAddress = 10.0.0.1/24\n");
        assert!(validate_basic_wireguard_config(&commented).is_ok());
        assert!(!looks_like_inet("10.0.0.1/24\u{a0}"));
    }

    #[test]
    fn quick_interface_headers_must_not_hide_metadata_scope() {
        for header in ["[ Interface ]", "[Interface ]", "[ Interface]", "[ Peer ]"] {
            let text =
                format!("[Interface]\nPrivateKey = {KEY}\n{header}\nAddress = 10.0.0.1/24\n");
            assert!(validate_basic_wireguard_config(&text).is_err(), "{header}");
        }
        assert!(
            validate_basic_wireguard_config(&format!(
                "[interface]\nprivatekey = {KEY}\naddress = 10.0.0.1/24\n"
            ))
            .is_ok()
        );
    }

    #[test]
    fn supported_wg_quick_options_and_scoped_ipv6_remain_valid() {
        for table in ["auto", "off", "123", "main"] {
            for mark in ["off", "0", "123", "0xca6c"] {
                let text = format!(
                    "[Interface]\nPrivateKey = {KEY}\nListenPort = 0\nMTU = 1420\nTable = {table}\nFwMark = {mark}\nSaveConfig = true\nDNS = 1.1.1.1, search.example\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = [fe80::1%eth0]:51820\nPersistentKeepalive = 65535\n"
                );
                assert!(
                    validate_basic_wireguard_config(&text).is_ok(),
                    "table={table} mark={mark}"
                );
            }
        }
        let optional = format!(
            "[Interface]\nPrivateKey = {KEY}\nMTU =\nTable =\nDNS = 1.1.1.1 8.8.8.8 search.example\nAddress = 10.0.0.1/24 fd00::1/64\n"
        );
        assert!(validate_basic_wireguard_config(&optional).is_ok());
        let invalid_psk = format!(
            "[Interface]\nPrivateKey = {KEY}\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = ::/0\nPresharedKey =\n"
        );
        assert!(validate_basic_wireguard_config(&invalid_psk).is_err());
        assert!(is_endpoint("[fe80::1%3]:51820"));
        assert!(!is_endpoint("[fe80::1%]:51820"));
        assert!(!is_endpoint("[fe80::1%eth0%evil]:51820"));
    }

    #[test]
    fn key_padding_bits_must_match_wireguard_decoder() {
        assert!(is_wg_key(KEY));
        assert!(!is_wg_key("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq="));
        assert!(!is_wg_key("éABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmno="));
    }

    #[test]
    fn every_repeated_scalar_and_section_is_validated() {
        let valid = format!(
            "[Interface]\nPrivateKey = {KEY}\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        for invalid in [
            "Endpoint = bad\nEndpoint = host:42",
            "PersistentKeepalive = 65536\nPersistentKeepalive = 25",
            "AllowedIPs = 10.0.0.1/+24",
        ] {
            assert!(
                validate_basic_wireguard_config(&format!("{valid}{invalid}\n")).is_err(),
                "{invalid}"
            );
        }
        for invalid in [
            "[[Interface]]",
            "[Interface] trailing",
            "[Unknown]",
            "some junk",
            "Unknown = value",
            "Address = 10.0.0.1/+24",
            "ListenPort =",
            "MTU = 0",
            "SaveConfig = perhaps",
        ] {
            assert!(
                validate_basic_wireguard_config(&format!(
                    "[Interface]\nPrivateKey = {KEY}\n{invalid}\n"
                ))
                .is_err(),
                "{invalid}"
            );
        }
        assert!(
            validate_basic_wireguard_config(&format!(
                "PrivateKey = {KEY}\n[Interface]\nPrivateKey = {KEY}\n"
            ))
            .is_err()
        );
        assert!(
            validate_basic_wireguard_config(&format!(
                "[Peer]\nPublicKey = {KEY}\nAllowedIPs = ::/0\n[Interface]\nPrivateKey = {KEY}\n"
            ))
            .is_ok()
        );
        assert!(validate_basic_wireguard_config(&format!("{valid}[Interface]\nListenPort = 0\nFwMark = 0xca6c\nTable = off\nSaveConfig = false\n")).is_ok());
    }

    #[test]
    fn wg_key_shape() {
        assert!(is_wg_key(KEY));
        assert!(!is_wg_key("tooshort="));
        assert!(!is_wg_key(""));
        assert!(!is_wg_key(&KEY.replace('=', "x")));
    }

    #[test]
    fn endpoint_validation() {
        assert!(is_endpoint("vpn.example.com:51820"));
        assert!(is_endpoint("10.0.0.1:51820"));
        assert!(is_endpoint("[2402:6880:2000:590::2]:51820"));
        assert!(!is_endpoint("2402:6880:2000:590::2:51820"));
        assert!(!is_endpoint("[notanaddr]:51820"));
        assert!(!is_endpoint("host:0"));
        assert!(!is_endpoint("host:99999"));
        assert!(!is_endpoint("host"));
        assert!(!is_endpoint("@#$:51820"));
        // Tightened cases: leading '-' label, malformed dotted-quad, and a port
        // with a leading '+' that str::parse would otherwise accept.
        assert!(is_endpoint("a-b.example.com:51820"));
        assert!(!is_endpoint("-evil:51820"));
        assert!(!is_endpoint("999.999.999.999:51820"));
        assert!(!is_endpoint("host:+5"));
        assert!(is_endpoint("host.example.com.:51820")); // one trailing dot (FQDN) tolerated
    }

    #[test]
    fn keepalive_off_is_valid() {
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\n\
             AllowedIPs = 0.0.0.0/0\nPersistentKeepalive = off\n"
        );
        assert!(validate_basic_wireguard_config(&cfg).is_ok());
        let bad = format!(
            "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\n\
             AllowedIPs = 0.0.0.0/0\nPersistentKeepalive = soon\n"
        );
        assert!(validate_basic_wireguard_config(&bad).is_err());
    }

    #[test]
    fn repeated_address_and_allowedips_lines_are_joined() {
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\nAddress = fd00::2/64\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nAllowedIPs = ::/0\n"
        );
        let p = parse_config(&cfg);
        assert_eq!(p.address.as_deref(), Some("10.0.0.2/24, fd00::2/64"));
        assert_eq!(p.peers[0].allowed_ips, "0.0.0.0/0, ::/0");
        assert!(validate_basic_wireguard_config(&cfg).is_ok());
    }

    #[test]
    fn earlier_invalid_keys_cannot_be_hidden_by_repeated_values() {
        let valid = format!(
            "[Interface]\nPrivateKey = {KEY}\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        for key in ["PrivateKey", "PublicKey"] {
            let line = format!("{key} = {KEY}");
            let bad = valid.replace(&line, &format!("{key} = bad\n{line}"));
            assert!(validate_basic_wireguard_config(&bad).is_err(), "{key}");
        }
        let bad = format!("{valid}PresharedKey = bad\nPresharedKey = {KEY}\n");
        assert!(validate_basic_wireguard_config(&bad).is_err());
    }

    #[test]
    fn earlier_empty_address_lists_cannot_be_hidden_by_repeated_values() {
        let valid = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        for (key, value) in [("Address", "10.0.0.2/24"), ("AllowedIPs", "0.0.0.0/0")] {
            let line = format!("{key} = {value}");
            let bad = valid.replace(&line, &format!("{key} =\n{line}"));
            assert!(validate_basic_wireguard_config(&bad).is_err(), "{key}");
        }
    }

    #[test]
    fn inline_comments_match_wg_quick_parsing() {
        let text = format!(
            "[Interface] # home\nPrivateKey = {KEY} # secret\n\
             Address = 10.0.0.2/24 # client\n[Peer] # gateway\n\
             PublicKey = {KEY} # server\nAllowedIPs = 0.0.0.0/0 # all IPv4\n\
             Endpoint = vpn.example.com:51820 # remote\nPersistentKeepalive = 25 # seconds\n"
        );
        assert!(validate_basic_wireguard_config(&text).is_ok());
        let parsed = parse_config(&text);
        assert_eq!(parsed.private_key.as_deref(), Some(KEY));
        assert_eq!(parsed.peers[0].endpoint, "vpn.example.com:51820");
    }

    #[test]
    fn inet_validation() {
        assert!(looks_like_inet("10.0.0.2/24"));
        assert!(looks_like_inet("10.0.0.2"));
        assert!(looks_like_inet("::1/128"));
        assert!(looks_like_inet("fd00:7::2/64"));
        assert!(looks_like_inet("0.0.0.0/0"));
        assert!(!looks_like_inet("10.0.0.2/33"));
        assert!(!looks_like_inet("::1/129"));
        assert!(!looks_like_inet("not-an-ip"));
        assert!(!looks_like_inet("999.1.1.1"));
    }

    #[test]
    fn validate_full_config_ok() {
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24, fd00::2/64\n\
             DNS = 1.1.1.1\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0, ::/0\n\
             Endpoint = vpn.example.com:51820\nPersistentKeepalive = 25\n"
        );
        assert!(validate_basic_wireguard_config(&cfg).is_ok());
        let p = parse_config(&cfg);
        assert_eq!(p.address.as_deref(), Some("10.0.0.2/24, fd00::2/64"));
        assert_eq!(p.peers.len(), 1);
    }

    #[test]
    fn validates_interface_only_config() {
        let cfg = format!("[Interface]\nPrivateKey = {KEY}\n");
        assert!(validate_basic_wireguard_config(&cfg).is_ok());
    }

    #[test]
    fn validate_rejects_missing_private_key_and_bad_peer() {
        assert!(validate_basic_wireguard_config("not a config").is_err());
        assert!(validate_basic_wireguard_config("[Interface]\nAddress = 10.0.0.2/24\n").is_err());
        assert!(
            validate_basic_wireguard_config(&format!(
                "[Interface]\nPrivateKey = {KEY}\n[Peer]\nAllowedIPs = 0.0.0.0/0\n"
            ))
            .is_err()
        );
    }
}
