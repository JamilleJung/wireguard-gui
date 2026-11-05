//! Connection diagnostics: a friendly "why isn't this connecting?" checklist.

use super::health::{self, Health};
use super::status::{Detail, fmt_ago};
use crate::doctor;

fn diag_line(s: &mut String, mark: &str, label: &str, detail: &str) {
    s.push_str(&format!("  {mark} {label:<12} {detail}\n"));
}

pub(crate) fn endpoint_host(ep: &str) -> String {
    if let Some(rest) = ep.strip_prefix('[')
        && let Some((h, _)) = rest.split_once(']')
    {
        return h.to_string();
    }
    ep.rsplit_once(':')
        .map(|(h, _)| h.to_string())
        .unwrap_or_else(|| ep.to_string())
}

fn ping_host(host: &str) -> Option<bool> {
    use std::process::{Command, Stdio};
    let st = Command::new("ping")
        .args(["-c", "1", "-W", "2", host])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    Some(st.success())
}

fn dns_ok() -> bool {
    use std::net::ToSocketAddrs;
    ("one.one.one.one", 443)
        .to_socket_addrs()
        .map(|mut a| a.next().is_some())
        .unwrap_or(false)
}

/// Everything the report states, gathered beforehand so the wording can be
/// tested without a network or a tunnel.
pub(crate) struct Findings {
    pub clock_synced: Option<bool>,
    pub active: bool,
    /// Per-peer health and handshake age.
    pub peers: Vec<(Health, Option<u64>)>,
    pub endpoint: Option<(String, Option<bool>)>,
    pub dns_ok: bool,
}

/// Run a connection-health checklist for `name` and return a formatted report.
/// `known` is the live per-peer health from the UI's poller (it has seen rx/tx
/// deltas); without it a one-off snapshot is used. Safe on a background thread.
pub fn diagnose_report(name: &str, known: Option<Vec<Health>>) -> String {
    let d: Detail = super::get_detail(name);
    let states = known
        .filter(|states| states.len() == d.peers.len())
        .unwrap_or_else(|| health::snapshot(d.active, &d.peers));
    let endpoint = d
        .peers
        .iter()
        .map(|p| p.endpoint.clone())
        .find(|e| !e.is_empty())
        .map(|ep| {
            let host = endpoint_host(&ep);
            let reachable = if d.active { ping_host(&host) } else { None };
            (host, reachable)
        });
    let findings = Findings {
        clock_synced: doctor::clock_synced(),
        active: d.active,
        peers: states
            .into_iter()
            .zip(d.peers.iter().map(|p| p.handshake_age))
            .collect(),
        endpoint,
        dns_ok: d.active && dns_ok(),
    };
    build_report(name, &findings)
}

pub(crate) fn build_report(name: &str, f: &Findings) -> String {
    let mut s = format!("Connection check for {name}\n\n");
    let states: Vec<Health> = f.peers.iter().map(|(h, _)| *h).collect();
    let overall = health::aggregate(f.active, &states);
    let failing = overall.is_problem();
    match f.clock_synced {
        Some(true) => diag_line(&mut s, "[OK]", "Clock", "system clock is NTP-synced"),
        Some(false) if failing => {
            diag_line(
                &mut s,
                "[!]",
                "Clock",
                "not NTP-synced — a possible cause of the failing handshake (peers reject a clock that is far off)",
            );
            s.push_str("       -> sync time (enable NTP/chrony); if it just drifted, the server may need its peer reset\n");
        }
        Some(false) => diag_line(
            &mut s,
            "[i]",
            "Clock",
            "not NTP-synced; handshakes are not failing, so this is not affecting the tunnel now",
        ),
        None => diag_line(&mut s, "[?]", "Clock", "could not read clock-sync status"),
    }
    if !f.active {
        diag_line(
            &mut s,
            "[!]",
            "Tunnel",
            "not active — activate it first, then re-run",
        );
        return s;
    }
    diag_line(&mut s, "[OK]", "Tunnel", &format!("{name} is up"));
    let mark = match overall {
        Health::Healthy => "[OK]",
        Health::ConnectionFailed => "[X]",
        Health::HandshakeStale => "[!]",
        _ => "[i]",
    };
    let detail = health::interface_summary(f.active, &f.peers);
    diag_line(&mut s, mark, "Handshake", &detail);
    if f.peers.len() > 1 {
        for (index, (h, age)) in f.peers.iter().enumerate() {
            let age = age
                .map(|a| format!(", last handshake {} ago", fmt_ago(a)))
                .unwrap_or_default();
            s.push_str(&format!("       peer {}: {}{age}\n", index + 1, h.label()));
        }
    }
    if matches!(overall, Health::Idle | Health::NoTraffic) {
        s.push_str("       -> no traffic yet or a quiet tunnel: send some traffic (e.g. ping a host inside the VPN), then re-run\n");
    }
    if failing {
        let mut causes = vec![
            "endpoint unreachable or blocked (UDP)",
            "the server has not accepted this peer's public key or preshared key",
        ];
        if f.clock_synced == Some(false) {
            causes.push("the unsynced system clock");
        }
        s.push_str(&format!(
            "       -> possible causes: {}\n",
            causes.join("; ")
        ));
    }
    match &f.endpoint {
        Some((host, Some(true))) => {
            diag_line(&mut s, "[OK]", "Endpoint", &format!("{host} is reachable"))
        }
        Some((host, Some(false))) => {
            diag_line(
                &mut s,
                "[!]",
                "Endpoint",
                &format!("{host} did not answer ping"),
            );
            s.push_str(
                "       -> server may block ICMP (handshake can still work) or the host is down\n",
            );
        }
        Some((_, None)) => diag_line(&mut s, "[?]", "Endpoint", "could not test the endpoint"),
        None => diag_line(&mut s, "[?]", "Endpoint", "no endpoint set in this config"),
    }
    if f.dns_ok {
        diag_line(&mut s, "[OK]", "DNS", "name resolution works");
    } else {
        diag_line(
            &mut s,
            "[!]",
            "DNS",
            "DNS lookup failed — check the tunnel's DNS = line / resolvconf",
        );
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn findings(clock: Option<bool>, peers: Vec<(Health, Option<u64>)>) -> Findings {
        Findings {
            clock_synced: clock,
            active: true,
            peers,
            endpoint: Some(("vpn.example.com".into(), Some(true))),
            dns_ok: true,
        }
    }

    #[test]
    fn unsynced_clock_is_not_blamed_when_handshakes_are_fine() {
        let report = build_report(
            "wg0",
            &findings(Some(false), vec![(Health::Healthy, Some(5))]),
        );
        assert!(report.contains("not affecting"), "{report}");
        assert!(!report.contains("possible cause"), "{report}");
        let report = build_report(
            "wg0",
            &findings(Some(false), vec![(Health::Idle, Some(4000))]),
        );
        assert!(!report.contains("possible cause"), "{report}");
        assert!(!report.contains("[X]"), "{report}");
    }

    #[test]
    fn unsynced_clock_is_a_possible_cause_only_for_failing_handshakes() {
        let report = build_report(
            "wg0",
            &findings(Some(false), vec![(Health::ConnectionFailed, Some(900))]),
        );
        assert!(
            report.contains("possible cause of the failing handshake"),
            "{report}"
        );
        assert!(report.contains("the unsynced system clock"), "{report}");
        let report = build_report(
            "wg0",
            &findings(Some(true), vec![(Health::ConnectionFailed, None)]),
        );
        assert!(!report.contains("unsynced"), "{report}");
        assert!(report.contains("[X] Handshake"), "{report}");
    }

    #[test]
    fn stale_handshake_alone_is_reported_as_idle() {
        let report = build_report(
            "wg0",
            &findings(Some(true), vec![(Health::Idle, Some(600))]),
        );
        assert!(report.contains("[i] Handshake"), "{report}");
        assert!(report.contains("send some traffic"), "{report}");
    }

    #[test]
    fn multi_peer_reports_list_each_peer() {
        let report = build_report(
            "wg0",
            &findings(
                None,
                vec![(Health::Healthy, Some(3)), (Health::NoTraffic, None)],
            ),
        );
        assert!(report.contains("peer 1: Healthy"), "{report}");
        assert!(report.contains("peer 2: No traffic"), "{report}");
    }

    #[test]
    fn endpoint_host_strips_ports_and_brackets() {
        assert_eq!(endpoint_host("[2001:db8::1]:51820"), "2001:db8::1");
        assert_eq!(endpoint_host("vpn.example.com:51820"), "vpn.example.com");
        assert_eq!(endpoint_host("host"), "host");
    }
}
