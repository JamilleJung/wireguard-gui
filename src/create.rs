#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TunnelTemplateKind {
    InterfaceOnly,
    ClientFullTunnel,
    ClientSplitTunnel,
}

impl TunnelTemplateKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::InterfaceOnly => "Interface only",
            Self::ClientFullTunnel => "Client full tunnel",
            Self::ClientSplitTunnel => "Client split tunnel",
        }
    }
}

pub const FALLBACK_PRIVATE_KEY: &str = "<run: wg genkey>";

pub fn generate_interface_only_config(private_key: &str) -> String {
    format!("[Interface]\nPrivateKey = {private_key}\nAddress = 10.0.0.2/32\n")
}

pub fn generate_client_full_tunnel_config(private_key: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = {private_key}\nAddress = 10.0.0.2/32\nDNS = 1.1.1.1\n\n\
         [Peer]\nPublicKey = \nAllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = vpn.example.com:51820\nPersistentKeepalive = 25\n"
    )
}

pub fn generate_client_split_tunnel_config(private_key: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = {private_key}\nAddress = 10.0.0.2/32\n\n\
         [Peer]\nPublicKey = \nAllowedIPs = 10.0.0.0/24\nEndpoint = vpn.example.com:51820\nPersistentKeepalive = 25\n"
    )
}

pub fn generate_template(kind: TunnelTemplateKind, private_key: &str) -> String {
    match kind {
        TunnelTemplateKind::InterfaceOnly => generate_interface_only_config(private_key),
        TunnelTemplateKind::ClientFullTunnel => generate_client_full_tunnel_config(private_key),
        TunnelTemplateKind::ClientSplitTunnel => generate_client_split_tunnel_config(private_key),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        TunnelTemplateKind, generate_client_full_tunnel_config,
        generate_client_split_tunnel_config, generate_interface_only_config, generate_template,
    };

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=";

    #[test]
    fn generates_interface_only_config() {
        let cfg = generate_interface_only_config(KEY);
        assert!(cfg.contains("[Interface]"));
        assert!(cfg.contains(&format!("PrivateKey = {KEY}")));
        assert!(!cfg.contains("[Peer]"));
    }

    #[test]
    fn generates_full_tunnel_defaults() {
        let cfg = generate_client_full_tunnel_config(KEY);
        assert!(cfg.contains("AllowedIPs = 0.0.0.0/0, ::/0"));
        assert!(cfg.contains("PersistentKeepalive = 25"));
        assert!(cfg.contains("DNS = 1.1.1.1"));
    }

    #[test]
    fn generates_split_tunnel_defaults() {
        let cfg = generate_client_split_tunnel_config(KEY);
        assert!(cfg.contains("AllowedIPs = 10.0.0.0/24"));
        assert!(!cfg.contains("0.0.0.0/0, ::/0"));
    }

    #[test]
    fn dispatches_by_kind() {
        let cfg = generate_template(TunnelTemplateKind::InterfaceOnly, KEY);
        assert!(!cfg.contains("[Peer]"));
    }
}
