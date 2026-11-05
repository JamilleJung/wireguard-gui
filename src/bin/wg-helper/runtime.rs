//! Queries and changes to the running WireGuard interface (`wg show`/`wg set`),
//! and the runtime settings a live `wg syncconf` must not reset.

use std::time::Duration;

use crate::process::command_output;

pub fn interface_active(name: &str) -> Result<bool, String> {
    command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5))
        .map(|out| out.split_whitespace().any(|iface| iface == name))
}

/// Parse a non-zero firewall mark as printed by `wg`, `nft` or `iptables`.
pub fn parse_fwmark(value: &str) -> Result<u32, String> {
    let value = value.trim();
    let mark = if let Some(hex) = value.strip_prefix("0x") {
        (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
    } else {
        (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten()
    };
    mark.filter(|n| *n != 0)
        .ok_or_else(|| "invalid or missing WireGuard fwmark".into())
}

/// `off`/`0` → `None`; anything else must be a valid mark.
fn parse_optional_fwmark(value: &str) -> Result<Option<u32>, String> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("off") || value == "0" || value == "0x0" {
        return Ok(None);
    }
    parse_fwmark(value).map(Some)
}

/// The interface's current fwmark, `None` when it is off.
pub fn runtime_fwmark(name: &str) -> Result<Option<u32>, String> {
    let out = command_output(
        "wg",
        &["show", name, "fwmark"],
        None,
        Duration::from_secs(5),
    )?;
    parse_optional_fwmark(&out)
}

/// The interface's current UDP listen port, `None` when it has none.
pub fn runtime_listen_port(name: &str) -> Result<Option<u16>, String> {
    let out = command_output(
        "wg",
        &["show", name, "listen-port"],
        None,
        Duration::from_secs(5),
    )?;
    let value = out.trim();
    if value.is_empty() || value == "0" || value == "(none)" {
        return Ok(None);
    }
    value
        .parse::<u16>()
        .map(Some)
        .map_err(|_| format!("invalid listen port reported for {name}: {value}"))
}

pub fn unused_fwmark(all: &str) -> Result<u32, String> {
    let mut used = std::collections::HashSet::new();
    for line in all.lines().filter(|l| !l.trim().is_empty()) {
        let value = line
            .split_whitespace()
            .nth(1)
            .ok_or("invalid WireGuard fwmark listing")?;
        if value != "off" {
            used.insert(parse_fwmark(value)?);
        }
    }
    let mut mark = 51820u32;
    while used.contains(&mark) {
        mark = mark.checked_add(1).ok_or("no unused WireGuard fwmark")?;
    }
    Ok(mark)
}

/// Give an active tunnel a fwmark when it has none and return the mark in use.
pub fn ensure_tunnel_has_fwmark(name: &str) -> Result<u32, String> {
    if !interface_active(name)? {
        return Err(format!("kill switch needs an active tunnel: {name}"));
    }
    if let Some(mark) = runtime_fwmark(name)? {
        return Ok(mark);
    }
    let all = command_output(
        "wg",
        &["show", "all", "fwmark"],
        None,
        Duration::from_secs(5),
    )?;
    let mark = unused_fwmark(&all)?;
    command_output(
        "wg",
        &["set", name, "fwmark", &mark.to_string()],
        None,
        Duration::from_secs(5),
    )?;
    match runtime_fwmark(name)? {
        Some(actual) if actual == mark => Ok(mark),
        other => Err(format!(
            "fwmark for {name} did not take effect (expected {mark}, found {})",
            other.map_or("off".into(), |m| m.to_string())
        )),
    }
}

/// The `[Interface]` settings of a stripped config that `wg syncconf` applies.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct InterfaceSettings {
    /// `None`: not specified. `Some(None)`: explicitly off.
    pub fwmark: Option<Option<u32>>,
    pub listen_port: Option<u16>,
}

fn section(line: &str) -> Option<String> {
    let line = line.split('#').next().unwrap_or("").trim();
    line.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map(|name| name.trim().to_ascii_lowercase())
}

fn key_value(line: &str) -> Option<(String, &str)> {
    let line = line.split('#').next().unwrap_or("");
    let (key, value) = line.split_once('=')?;
    Some((key.trim().to_ascii_lowercase(), value.trim()))
}

pub fn interface_settings(conf: &str) -> Result<InterfaceSettings, String> {
    let mut settings = InterfaceSettings::default();
    let mut in_interface = false;
    let mut seen_interface = false;
    for line in conf.lines() {
        if let Some(name) = section(line) {
            in_interface = name == "interface";
            seen_interface |= in_interface;
            continue;
        }
        if !in_interface {
            continue;
        }
        match key_value(line) {
            Some((key, value)) if key == "fwmark" => {
                settings.fwmark = Some(parse_optional_fwmark(value)?);
            }
            Some((key, value)) if key == "listenport" => {
                settings.listen_port = Some(
                    value
                        .parse::<u16>()
                        .map_err(|_| format!("invalid ListenPort in config: {value}"))?,
                );
            }
            _ => {}
        }
    }
    if !seen_interface {
        return Err("config has no [Interface] section".into());
    }
    Ok(settings)
}

/// `wg syncconf` is a full `setconf` of the interface fields: a FwMark or
/// ListenPort missing from the file is reset (fwmark to off, the port to a new
/// random one). wg-quick sets the fwmark at runtime for its policy routing and
/// the kill switch depends on it, so carry the live values into the file
/// instead of letting a config edit silently drop them.
///
/// A config that *changes* the fwmark of a running tunnel is refused: the
/// policy-routing rules and the kill switch were built for the old mark and
/// only a reconnect can rebuild them consistently.
pub fn preserve_runtime_settings(
    stripped: &str,
    runtime_mark: Option<u32>,
    runtime_port: Option<u16>,
) -> Result<String, String> {
    let settings = interface_settings(stripped)?;
    let mut extra = Vec::new();
    match (settings.fwmark, runtime_mark) {
        (None, Some(mark)) => extra.push(format!("FwMark = {mark}")),
        (None, None) => {}
        (Some(wanted), current) if wanted == current => {}
        // Setting a mark on a tunnel that has none touches no routing state.
        (Some(_), None) => {}
        (Some(wanted), current) => {
            let show = |m: Option<u32>| m.map_or("off".to_string(), |m| m.to_string());
            return Err(format!(
                "the saved config changes FwMark from {} to {}; reconnect the tunnel to apply it",
                show(current),
                show(wanted)
            ));
        }
    }
    if settings.listen_port.is_none()
        && let Some(port) = runtime_port
    {
        extra.push(format!("ListenPort = {port}"));
    }
    if extra.is_empty() {
        return Ok(stripped.to_string());
    }
    let mut out = String::with_capacity(stripped.len() + 64);
    let mut inserted = false;
    for line in stripped.lines() {
        out.push_str(line);
        out.push('\n');
        if !inserted && section(line).as_deref() == Some("interface") {
            for setting in &extra {
                out.push_str(setting);
                out.push('\n');
            }
            inserted = true;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    #[test]
    fn runtime_marks_do_not_collide() {
        assert_eq!(
            unused_fwmark("wg0\t0xca6c\nwg1\t51821\nwg2\toff\n").unwrap(),
            51822
        );
        for bad in ["off", "0", "+1", "1; accept", "0x", "4294967296"] {
            assert!(parse_fwmark(bad).is_err(), "{bad}");
        }
        assert_eq!(parse_fwmark("0x0000ca6c").unwrap(), 51820);
        assert_eq!(parse_optional_fwmark("off").unwrap(), None);
        assert_eq!(parse_optional_fwmark("0x0").unwrap(), None);
    }

    #[test]
    fn sync_keeps_the_runtime_fwmark_and_listen_port() {
        let stripped = format!(
            "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        let out = preserve_runtime_settings(&stripped, Some(51820), Some(41000)).unwrap();
        assert_eq!(
            interface_settings(&out).unwrap(),
            InterfaceSettings {
                fwmark: Some(Some(51820)),
                listen_port: Some(41000)
            }
        );
        // The settings belong to [Interface], never to a peer.
        let iface_end = out.find("[Peer]").unwrap();
        assert!(out[..iface_end].contains("FwMark = 51820"));
        assert!(out[..iface_end].contains("ListenPort = 41000"));
        assert!(out.contains(&format!("PublicKey = {KEY}")));
    }

    #[test]
    fn sync_does_not_duplicate_or_override_configured_values() {
        let stripped = format!(
            "[interface]\nprivatekey = {KEY}\nfwmark = 0xca6c\nlistenport = 51820\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = ::/0\n"
        );
        let out = preserve_runtime_settings(&stripped, Some(51820), Some(41000)).unwrap();
        assert_eq!(out, stripped);
    }

    #[test]
    fn sync_refuses_a_config_that_changes_the_running_fwmark() {
        let changed = format!("[Interface]\nPrivateKey = {KEY}\nFwMark = 1234\n");
        let error = preserve_runtime_settings(&changed, Some(51820), None).unwrap_err();
        assert!(error.contains("reconnect"), "{error}");
        let off = format!("[Interface]\nPrivateKey = {KEY}\nFwMark = off\n");
        assert!(preserve_runtime_settings(&off, Some(51820), None).is_err());
        // Setting a mark on a tunnel that has none changes no routing state.
        assert!(preserve_runtime_settings(&changed, None, None).is_ok());
        assert!(preserve_runtime_settings(&off, None, None).is_ok());
    }

    #[test]
    fn sync_without_runtime_values_leaves_the_config_alone() {
        let stripped = format!("[Interface]\nPrivateKey = {KEY}\n");
        assert_eq!(
            preserve_runtime_settings(&stripped, None, None).unwrap(),
            stripped
        );
        assert!(preserve_runtime_settings("[Peer]\nPublicKey = x\n", Some(1), None).is_err());
        assert!(
            preserve_runtime_settings(
                &format!("[Interface]\nPrivateKey = {KEY}\nFwMark = 1; flush\n"),
                None,
                None
            )
            .is_err()
        );
    }
}
