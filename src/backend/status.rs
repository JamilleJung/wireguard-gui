//! The tunnel status model handed to the UI, live `wg show dump` parsing, and
//! display formatting.

use std::time::{SystemTime, UNIX_EPOCH};

use super::error::HelperError;
use crate::config;

/// The firewall kill switch as reported by `killswitch-status`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KillSwitch {
    Enabled,
    #[default]
    Disabled,
    /// Turned on for this tunnel, but the firewall rules are gone/incomplete.
    Missing,
    /// Rules present but stale: their mark no longer matches the tunnel's
    /// runtime fwmark, or the interface is down.
    Mismatch,
    /// The status could not be read or was not understood. Never "off".
    Unknown,
}

impl KillSwitch {
    pub fn parse(output: Result<String, HelperError>) -> Self {
        match output {
            Ok(text) => match text.trim() {
                "enabled" => Self::Enabled,
                "disabled" => Self::Disabled,
                "missing" => Self::Missing,
                "mismatch" => Self::Mismatch,
                _ => Self::Unknown,
            },
            Err(_) => Self::Unknown,
        }
    }

    /// The user asked for protection (the checkbox shows as on).
    pub fn intended_on(self) -> bool {
        matches!(self, Self::Enabled | Self::Missing | Self::Mismatch)
    }

    /// Protection was requested but is not actually in force.
    pub fn is_broken(self) -> bool {
        matches!(self, Self::Missing | Self::Mismatch)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::Missing => "missing",
            Self::Mismatch => "mismatch",
            Self::Unknown => "unknown",
        }
    }

    /// One-line explanation for the detail pane ("" when nothing to say).
    pub fn problem(self, active: bool) -> &'static str {
        match self {
            Self::Missing if active => {
                "Kill switch is ON but its firewall rules are missing: traffic is NOT blocked outside the tunnel."
            }
            Self::Mismatch if active => {
                "Kill switch rules no longer match this tunnel's firewall mark: protection may be incomplete."
            }
            Self::Missing | Self::Mismatch => {
                "Kill switch is configured but not in force while the tunnel is down."
            }
            Self::Unknown => "Kill switch status could not be read.",
            Self::Enabled | Self::Disabled => "",
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Peer {
    pub public_key: String,
    pub preshared: bool,
    pub allowed_ips: String,
    pub endpoint: String,
    pub keepalive: String,
    pub latest_handshake: String,
    pub transfer: String,
    /// Seconds since this peer's latest handshake (None: never / not live).
    pub handshake_age: Option<u64>,
    pub rx: u64,
    pub tx: u64,
    /// Persistent keepalive interval in seconds, 0 when off.
    pub keepalive_secs: u64,
}

#[derive(Clone, Debug, Default)]
pub struct Detail {
    pub name: String,
    pub active: bool,
    pub autostart: bool,
    pub killswitch: KillSwitch,
    pub public_key: String,
    pub listen_port: String,
    pub addresses: String,
    pub dns: String,
    pub peers: Vec<Peer>,
    /// Live interface totals (summed across peers), for computing throughput.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
}

pub(crate) struct LivePeer {
    pub public_key: String,
    pub preshared: bool,
    pub endpoint: String,
    pub allowed_ips: String,
    pub keepalive: String,
    pub latest_handshake: u64,
    pub rx: u64,
    pub tx: u64,
}

pub(crate) struct LiveDump {
    pub iface_public: String,
    pub listen_port: String,
    pub peers: Vec<LivePeer>,
}

pub(crate) fn now_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// `wg show <iface> dump`:
///   line 1 (interface): private-key  public-key  listen-port  fwmark
///   line N (peer):      public-key  preshared-key  endpoint  allowed-ips
///                       latest-handshake  rx  tx  persistent-keepalive
pub(crate) fn parse_dump(text: &str) -> Option<LiveDump> {
    let mut lines = text.lines();
    let fields: Vec<_> = lines.next()?.split('\t').collect();
    if fields.len() != 4 || fields[2].parse::<u16>().is_err() {
        return None;
    }
    let mut dump = LiveDump {
        iface_public: if fields[1] == "(none)" {
            String::new()
        } else {
            fields[1].to_string()
        },
        listen_port: fields[2].to_string(),
        peers: Vec::new(),
    };
    let none_empty = |s: &str| {
        if s == "(none)" {
            String::new()
        } else {
            s.to_string()
        }
    };
    for line in lines {
        let p: Vec<_> = line.split('\t').collect();
        if p.len() != 8 || !config::is_wg_key(p[0]) {
            continue;
        }
        let (Ok(latest_handshake), Ok(rx), Ok(tx)) = (p[4].parse(), p[5].parse(), p[6].parse())
        else {
            continue;
        };
        if p[7] != "off" && p[7].parse::<u16>().is_err() {
            continue;
        }
        dump.peers.push(LivePeer {
            public_key: p[0].to_string(),
            preshared: p[1] != "(none)",
            endpoint: none_empty(p[2]),
            allowed_ips: none_empty(p[3]),
            keepalive: p[7].to_string(),
            latest_handshake,
            rx,
            tx,
        });
    }
    Some(dump)
}

fn keepalive_secs(text: &str) -> u64 {
    text.trim().parse().unwrap_or(0)
}

fn age_of(epoch: u64, now: u64) -> Option<u64> {
    (epoch != 0 && epoch <= now).then(|| now - epoch)
}

fn live_peer(p: &LivePeer, now: u64) -> Peer {
    Peer {
        public_key: p.public_key.clone(),
        preshared: p.preshared,
        allowed_ips: p.allowed_ips.clone(),
        endpoint: p.endpoint.clone(),
        keepalive: p.keepalive.clone(),
        latest_handshake: fmt_handshake_at(p.latest_handshake, now),
        transfer: fmt_transfer(p.rx, p.tx),
        handshake_age: age_of(p.latest_handshake, now),
        rx: p.rx,
        tx: p.tx,
        keepalive_secs: keepalive_secs(&p.keepalive),
    }
}

pub(crate) fn merge_peers(
    saved: Vec<config::ParsedPeer>,
    live: Option<&LiveDump>,
    now: u64,
) -> Vec<Peer> {
    let mut peers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in saved {
        seen.insert(p.public_key.clone());
        let lp = live.and_then(|dump| dump.peers.iter().find(|v| v.public_key == p.public_key));
        match lp {
            Some(lp) => {
                let mut peer = live_peer(lp, now);
                if peer.endpoint.is_empty() {
                    peer.endpoint = p.endpoint;
                }
                peers.push(peer);
            }
            None => peers.push(Peer {
                keepalive_secs: keepalive_secs(&p.keepalive),
                public_key: p.public_key,
                preshared: !p.preshared_key.is_empty(),
                allowed_ips: p.allowed_ips,
                endpoint: p.endpoint,
                keepalive: p.keepalive,
                ..Default::default()
            }),
        }
    }
    if let Some(dump) = live {
        for p in &dump.peers {
            if seen.insert(p.public_key.clone()) {
                peers.push(live_peer(p, now));
            }
        }
    }
    peers
}

pub(crate) fn live_totals(live: Option<&LiveDump>, now: u64) -> (u64, u64, Option<u64>) {
    let Some(live) = live else {
        return (0, 0, None);
    };
    let (rx, tx) = live.peers.iter().fold((0u64, 0u64), |(rx, tx), p| {
        (rx.saturating_add(p.rx), tx.saturating_add(p.tx))
    });
    let latest = live
        .peers
        .iter()
        .map(|p| p.latest_handshake)
        .max()
        .unwrap_or(0);
    (rx, tx, age_of(latest, now))
}

fn fmt_handshake_at(epoch: u64, now: u64) -> String {
    if epoch == 0 {
        return String::new();
    }
    if epoch > now {
        return "in the future (check system clock)".to_string();
    }
    match now - epoch {
        0 => "now".to_string(),
        1 => "1 second ago".to_string(),
        s if s < 60 => format!("{s} seconds ago"),
        s if s < 120 => "1 minute ago".to_string(),
        s if s < 3600 => format!("{} minutes ago", s / 60),
        s if s < 7200 => "1 hour ago".to_string(),
        s if s < 86400 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    }
}

pub fn fmt_bytes(b: u64) -> String {
    if b < 1024 {
        return format!("{b} B");
    }
    // Closed-form unit pick instead of an if/else ladder: the unit index is
    // floor(log2(b) / 10) — i.e. how many whole times 1024 divides b. The
    // high-bit position is `63 - leading_zeros` (one CPU instruction), and the
    // 1024-power thresholds are exact in f64, so this is byte-for-byte identical
    // to the ladder while collapsing five format! sites into one. Clamped to TiB.
    const UNITS: [&str; 4] = ["KiB", "MiB", "GiB", "TiB"];
    let exp = (((63 - b.leading_zeros()) / 10) as usize).min(4);
    let value = b as f64 / 1024f64.powi(exp as i32);
    format!("{value:.2} {}", UNITS[exp - 1])
}

fn fmt_transfer(rx: u64, tx: u64) -> String {
    if rx == 0 && tx == 0 {
        return String::new();
    }
    format!("{} received, {} sent", fmt_bytes(rx), fmt_bytes(tx))
}

/// Compact "seconds ago" → "12s" / "3m" / "2h".
pub fn fmt_ago(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    #[test]
    fn live_dump_keeps_unsaved_peers_and_actual_runtime_settings() {
        let other = "A".repeat(43) + "=";
        let dump = parse_dump(&format!("private\t{KEY}\t51820\toff\n{KEY}\t(none)\t(none)\t10.0.0.0/24\t100\t20\t30\toff\n{other}\t{KEY}\t[::1]:5\t::/0\t200\t40\t50\t25\n")).unwrap();
        let saved = config::ParsedPeer {
            public_key: KEY.into(),
            allowed_ips: "old".into(),
            keepalive: "old".into(),
            endpoint: "saved:1".into(),
            ..Default::default()
        };
        let peers = merge_peers(vec![saved], Some(&dump), 300);
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].allowed_ips, "10.0.0.0/24");
        assert_eq!(peers[0].keepalive, "off");
        assert_eq!(peers[0].endpoint, "saved:1");
        assert_eq!(peers[0].handshake_age, Some(200));
        assert_eq!(peers[1].keepalive_secs, 25);
        assert_eq!((peers[1].rx, peers[1].tx), (40, 50));
        assert!(peers[1].preshared);
        assert_eq!(live_totals(Some(&dump), 300), (60, 80, Some(100)));
        assert_eq!(live_totals(Some(&dump), 150).2, None);
        let mut dump = dump;
        dump.peers[0].rx = u64::MAX;
        assert_eq!(live_totals(Some(&dump), 300).0, u64::MAX);
        assert!(parse_dump("invalid\tdump\t0").is_none());
    }

    #[test]
    fn killswitch_status_is_never_silently_off() {
        assert_eq!(
            KillSwitch::parse(Ok("enabled\n".into())),
            KillSwitch::Enabled
        );
        assert_eq!(
            KillSwitch::parse(Ok("disabled".into())),
            KillSwitch::Disabled
        );
        assert_eq!(KillSwitch::parse(Ok("missing".into())), KillSwitch::Missing);
        assert_eq!(
            KillSwitch::parse(Ok("mismatch".into())),
            KillSwitch::Mismatch
        );
        assert_eq!(KillSwitch::parse(Ok("garbage".into())), KillSwitch::Unknown);
        assert_eq!(
            KillSwitch::parse(Err(HelperError::TimedOut)),
            KillSwitch::Unknown
        );
        assert!(KillSwitch::Missing.intended_on() && KillSwitch::Missing.is_broken());
        assert!(!KillSwitch::Unknown.intended_on());
        assert!(
            KillSwitch::Unknown
                .problem(true)
                .contains("could not be read")
        );
        assert!(KillSwitch::Missing.problem(true).contains("NOT blocked"));
    }

    #[test]
    fn handshake_formatting_uses_the_given_clock() {
        assert_eq!(fmt_handshake_at(0, 100), "");
        assert_eq!(fmt_handshake_at(100, 100), "now");
        assert_eq!(fmt_handshake_at(40, 100), "1 minute ago");
        assert!(fmt_handshake_at(200, 100).contains("future"));
        assert_eq!(fmt_ago(59), "59s");
        assert_eq!(fmt_ago(61), "1m");
        assert_eq!(fmt_ago(7200), "2h");
    }

    // The previous if/else ladder, kept here as the behavioral oracle so the
    // closed-form `fmt_bytes` is proven byte-for-byte identical to it.
    fn fmt_bytes_ladder(b: u64) -> String {
        const KIB: f64 = 1024.0;
        let b = b as f64;
        if b < KIB {
            format!("{b:.0} B")
        } else if b < KIB * KIB {
            format!("{:.2} KiB", b / KIB)
        } else if b < KIB * KIB * KIB {
            format!("{:.2} MiB", b / (KIB * KIB))
        } else if b < KIB * KIB * KIB * KIB {
            format!("{:.2} GiB", b / (KIB * KIB * KIB))
        } else {
            format!("{:.2} TiB", b / (KIB * KIB * KIB * KIB))
        }
    }

    #[test]
    fn fmt_bytes_matches_ladder() {
        // Every unit boundary (and ±1 around it), plus 0 and u64::MAX.
        let mut cases = vec![0u64, 1, 1023, 1024, 1025, u64::MAX];
        for p in [10u32, 20, 30, 40, 50, 60] {
            let t = 1u64 << p;
            cases.extend([t - 1, t, t + 1]);
        }
        // A dense-ish multiplicative sweep across the whole range.
        let mut v = 1u64;
        while v < u64::MAX / 3 {
            cases.push(v);
            cases.push(v * 3);
            v = v.saturating_mul(2);
        }
        for b in cases {
            assert_eq!(fmt_bytes(b), fmt_bytes_ladder(b), "mismatch at b={b}");
        }
    }
}
