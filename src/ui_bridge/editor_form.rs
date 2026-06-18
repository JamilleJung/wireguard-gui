/// The structured peer fields shown in the form editor.
#[derive(Clone, Default)]
pub struct PeerFields {
    pub peer_public_key: String,
    pub preshared_key: String,
    pub allowed_ips: String,
    pub endpoint: String,
    pub keepalive: String,
}

/// The structured fields shown in the form editor (Interface + N peers).
#[derive(Clone, Default)]
pub struct Fields {
    pub private_key: String,
    pub address: String,
    pub dns: String,
    pub listen_port: String,
    pub mtu: String,
    pub peers: Vec<PeerFields>,
}

impl Fields {
    pub fn ensure_peer(&mut self) {
        if self.peers.is_empty() {
            self.peers.push(PeerFields::default());
        }
    }
}

/// Value to the right of the first `=` on a `key = value` line, trimmed.
fn kv_value(line: &str) -> String {
    line.split_once('=')
        .map(|(_, v)| v)
        .unwrap_or("")
        .trim()
        .to_string()
}

/// The lowercased key to the left of the first `=`, so keys are matched
/// exactly. `None` for a line with no `=`.
fn line_key(line: &str) -> Option<String> {
    line.split_once('=')
        .map(|(k, _)| k.trim().to_ascii_lowercase())
}

/// Whether the form view can faithfully represent this config. The form maps a
/// fixed set of Interface/Peer keys; scripts, routing directives, unknown keys,
/// and unknown sections would be silently dropped on a round-trip, so such
/// configs must stay in raw-text mode.
pub fn form_representable(cfg: &str) -> bool {
    use std::collections::HashSet;
    let mut peers = 0;
    let mut section = "";
    let mut iface_seen: HashSet<String> = HashSet::new();
    let mut peer_seen: HashSet<String> = HashSet::new();
    for line in cfg.lines() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        // A comment carries layout/intent the structured form cannot round-trip
        // (fields_to_config never re-emits comments), so keep such configs in
        // raw-text mode rather than silently dropping the comment on save.
        if t.starts_with('#') {
            return false;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with('[') {
            if lower.starts_with("[peer]") {
                peers += 1;
                section = "peer";
                peer_seen.clear();
            } else if lower.starts_with("[interface]") {
                section = "interface";
            } else {
                return false;
            }
            continue;
        }
        let Some(key) = line_key(t) else {
            return false;
        };
        let mapped = match section {
            "interface" => matches!(
                key.as_str(),
                "privatekey" | "address" | "dns" | "listenport" | "mtu"
            ),
            "peer" => matches!(
                key.as_str(),
                "publickey" | "presharedkey" | "allowedips" | "endpoint" | "persistentkeepalive"
            ),
            _ => false,
        };
        if !mapped {
            return false;
        }
        // A repeated key within a section would collapse to a single value on
        // round-trip (the form models each as one field), losing e.g. a second
        // Address or AllowedIPs line — keep raw so nothing is dropped.
        let fresh = match section {
            "interface" => iface_seen.insert(key),
            "peer" => peer_seen.insert(key),
            _ => true,
        };
        if !fresh {
            return false;
        }
    }
    peers > 0
}

/// Parse a raw config into the structured form fields.
pub fn config_to_fields(cfg: &str) -> Fields {
    let mut f = Fields::default();
    let mut section = "";
    let mut peer_idx: Option<usize> = None;
    for line in cfg.lines() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        let lower = t.to_ascii_lowercase();
        if lower.starts_with('[') {
            section = if lower.starts_with("[interface]") {
                peer_idx = None;
                "interface"
            } else if lower.starts_with("[peer]") {
                f.peers.push(PeerFields::default());
                peer_idx = Some(f.peers.len() - 1);
                "peer"
            } else {
                peer_idx = None;
                "other"
            };
            continue;
        }
        let Some(key) = line_key(t) else {
            continue;
        };
        match section {
            "interface" => match key.as_str() {
                "privatekey" => f.private_key = kv_value(t),
                "address" => f.address = kv_value(t),
                "dns" => f.dns = kv_value(t),
                "listenport" => f.listen_port = kv_value(t),
                "mtu" => f.mtu = kv_value(t),
                _ => {}
            },
            "peer" => {
                let Some(idx) = peer_idx else { continue };
                let Some(peer) = f.peers.get_mut(idx) else {
                    continue;
                };
                match key.as_str() {
                    "publickey" => peer.peer_public_key = kv_value(t),
                    "presharedkey" => peer.preshared_key = kv_value(t),
                    "allowedips" => peer.allowed_ips = kv_value(t),
                    "endpoint" => peer.endpoint = kv_value(t),
                    "persistentkeepalive" => peer.keepalive = kv_value(t),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    f.ensure_peer();
    f
}

/// Build a raw config from the structured form fields (omitting empty ones).
pub fn fields_to_config(f: &Fields) -> String {
    let mut s = String::from("[Interface]\n");
    let push = |s: &mut String, k: &str, v: &str| {
        if !v.trim().is_empty() {
            s.push_str(&format!("{k} = {}\n", v.trim()));
        }
    };
    push(&mut s, "PrivateKey", &f.private_key);
    push(&mut s, "Address", &f.address);
    push(&mut s, "DNS", &f.dns);
    push(&mut s, "ListenPort", &f.listen_port);
    push(&mut s, "MTU", &f.mtu);
    for peer in &f.peers {
        s.push_str("\n[Peer]\n");
        push(&mut s, "PublicKey", &peer.peer_public_key);
        push(&mut s, "PresharedKey", &peer.preshared_key);
        push(&mut s, "AllowedIPs", &peer.allowed_ips);
        push(&mut s, "Endpoint", &peer.endpoint);
        push(&mut s, "PersistentKeepalive", &peer.keepalive);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=";

    fn single_peer() -> String {
        format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nEndpoint = vpn.example.com:51820\n"
        )
    }

    #[test]
    fn representable_simple_config() {
        assert!(form_representable(&single_peer()));
    }

    #[test]
    fn representable_multi_peer_config() {
        let two = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 10.1.0.0/24\n"
        );
        assert!(form_representable(&two));
        let fields = config_to_fields(&two);
        assert_eq!(fields.peers.len(), 2);
        assert_eq!(fields.peers[1].allowed_ips, "10.1.0.0/24");
        let rebuilt = fields_to_config(&fields);
        let reparsed = config_to_fields(&rebuilt);
        assert_eq!(reparsed.peers.len(), 2);
        assert_eq!(reparsed.peers[1].allowed_ips, "10.1.0.0/24");
    }

    #[test]
    fn interface_only_is_not_form_representable() {
        let cfg = format!("[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n");
        assert!(!form_representable(&cfg));
    }

    #[test]
    fn not_representable_when_scripts_or_unknown_routing_fields() {
        let scripted = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\nPostUp = iptables -A FORWARD -i %i -j ACCEPT\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(!form_representable(&scripted));
        let table = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\nTable = off\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(!form_representable(&table));
    }

    #[test]
    fn fields_roundtrip_preserves_mapped_values() {
        let f = config_to_fields(&single_peer());
        assert_eq!(f.private_key, KEY);
        assert_eq!(f.address, "10.0.0.2/24");
        assert_eq!(f.peers.len(), 1);
        assert_eq!(f.peers[0].peer_public_key, KEY);
        assert_eq!(f.peers[0].allowed_ips, "0.0.0.0/0");
        assert_eq!(f.peers[0].endpoint, "vpn.example.com:51820");
        let rebuilt = fields_to_config(&f);
        assert!(form_representable(&rebuilt));
        let f2 = config_to_fields(&rebuilt);
        assert_eq!(f2.private_key, f.private_key);
        assert_eq!(f2.peers[0].endpoint, f.peers[0].endpoint);
        assert_eq!(f2.peers[0].allowed_ips, f.peers[0].allowed_ips);
    }

    #[test]
    fn unknown_prefix_keys_are_not_mapped() {
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\nPrivateKeyFile = /tmp/x\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nEndpointBackup = other:1\n"
        );
        assert!(!form_representable(&cfg));
        let f = config_to_fields(&cfg);
        assert_eq!(f.private_key, KEY);
        assert_eq!(f.peers[0].endpoint, "");
    }

    #[test]
    fn comment_bearing_config_is_not_form_representable() {
        // The form can't round-trip comments, so such a config must stay raw.
        let cfg = format!(
            "[Interface]\n# my home server\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(!form_representable(&cfg));
    }

    #[test]
    fn duplicate_mapped_keys_are_not_form_representable() {
        // Two Address lines (IPv4 + IPv6) would collapse to one on round-trip.
        let dup_addr = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\nAddress = fd00::2/64\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(!form_representable(&dup_addr));
        // Two AllowedIPs lines in one peer likewise.
        let dup_aips = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nAllowedIPs = ::/0\n"
        );
        assert!(!form_representable(&dup_aips));
        // But the same key appearing once per peer across two peers is fine.
        let two_peers = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n\n\
             [Peer]\nPublicKey = {KEY}\nAllowedIPs = 10.1.0.0/24\n"
        );
        assert!(form_representable(&two_peers));
    }
}
