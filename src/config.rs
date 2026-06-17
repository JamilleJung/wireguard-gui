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

pub fn parse_config(text: &str) -> ParsedConfig {
    let mut cfg = ParsedConfig::default();
    let mut section = "";
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let s = line.trim_matches(|c| c == '[' || c == ']').trim();
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
        let key = key.trim();
        let value = value.trim().to_string();
        match section {
            "interface" => match key.to_ascii_lowercase().as_str() {
                "privatekey" => cfg.private_key = Some(value),
                "address" => cfg.address = Some(value),
                "dns" => cfg.dns = Some(value),
                "listenport" => cfg.listen_port = Some(value),
                _ => {}
            },
            "peer" => {
                if let Some(p) = cfg.peers.last_mut() {
                    match key.to_ascii_lowercase().as_str() {
                        "publickey" => p.public_key = value,
                        "presharedkey" => p.preshared_key = value,
                        "allowedips" => p.allowed_ips = value,
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
    let s = s.trim();
    s.len() == 44
        && s.ends_with('=')
        && s[..43]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// host:port, including bracketed IPv6 `[::1]:51820`.
pub fn is_endpoint(s: &str) -> bool {
    let s = s.trim();
    let port_ok = |port: &str| matches!(port.parse::<u32>(), Ok(p) if (1..=65535).contains(&p));

    if let Some(rest) = s.strip_prefix('[') {
        let Some((inner, after)) = rest.split_once(']') else {
            return false;
        };
        let Some(port) = after.strip_prefix(':') else {
            return false;
        };
        return inner.parse::<std::net::Ipv6Addr>().is_ok() && port_ok(port);
    }

    let Some((host, port)) = s.rsplit_once(':') else {
        return false;
    };
    if host.is_empty() || host.contains(':') {
        return false;
    }
    let host_ok = host.parse::<std::net::Ipv4Addr>().is_ok()
        || host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    host_ok && port_ok(port)
}

/// A CIDR / address check via real parsing.
pub fn looks_like_inet(s: &str) -> bool {
    let s = s.trim();
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
        Some(p) => match p.parse::<u8>() {
            Ok(n) => match ip {
                std::net::IpAddr::V4(_) => n <= 32,
                std::net::IpAddr::V6(_) => n <= 128,
            },
            Err(_) => false,
        },
    }
}

/// Validate obvious WireGuard config errors before handing text to `wg-quick`.
///
/// This deliberately stays permissive: Address and Peer sections are allowed to
/// be absent for interface-only setups, but if fields are present they must be
/// shaped like WireGuard expects.
pub fn validate_basic_wireguard_config(text: &str) -> Result<(), String> {
    let has_iface = text
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case("[Interface]"));
    if !has_iface {
        return Err("Missing an [Interface] section.".into());
    }

    let cfg = parse_config(text);

    match cfg.private_key.as_deref() {
        None | Some("") => return Err("[Interface] is missing PrivateKey.".into()),
        Some(k) if !is_wg_key(k) => {
            return Err(
                "PrivateKey is not a valid WireGuard key (expected 44-char base64).".into(),
            );
        }
        _ => {}
    }

    if let Some(addrs) = cfg.address.as_deref() {
        if addrs.trim().is_empty() {
            return Err("Address is empty.".into());
        }
        for a in addrs.split(',') {
            if !looks_like_inet(a) {
                return Err(format!("Address '{}' is not a valid IP/CIDR.", a.trim()));
            }
        }
    }

    if let Some(port) = cfg.listen_port.as_deref()
        && !port.is_empty()
        && port.parse::<u32>().map(|p| p > 65535).unwrap_or(true)
    {
        return Err(format!("ListenPort '{port}' is not a valid port."));
    }

    for (i, p) in cfg.peers.iter().enumerate() {
        let n = i + 1;
        if p.public_key.is_empty() {
            return Err(format!("Peer {n} is missing PublicKey."));
        }
        if !is_wg_key(&p.public_key) {
            return Err(format!("Peer {n} has an invalid PublicKey."));
        }
        if !p.preshared_key.is_empty() && !is_wg_key(&p.preshared_key) {
            return Err(format!("Peer {n} has an invalid PresharedKey."));
        }
        if p.allowed_ips.trim().is_empty() {
            return Err(format!("Peer {n} is missing AllowedIPs."));
        }
        for a in p.allowed_ips.split(',') {
            if !looks_like_inet(a) {
                return Err(format!("Peer {n}: AllowedIPs '{}' is not valid.", a.trim()));
            }
        }
        if !p.endpoint.is_empty() && !is_endpoint(&p.endpoint) {
            return Err(format!(
                "Peer {n}: Endpoint '{}' must be host:port.",
                p.endpoint
            ));
        }
        if !p.keepalive.is_empty() && p.keepalive.parse::<u32>().is_err() {
            return Err(format!(
                "Peer {n}: PersistentKeepalive '{}' must be a number.",
                p.keepalive
            ));
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=";

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
