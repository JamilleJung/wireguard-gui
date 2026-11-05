//! Connection health per peer and per interface.
//!
//! WireGuard only handshakes when there is traffic (or a persistent
//! keepalive). A handshake older than 180 s on a quiet tunnel is normal, so
//! age alone never means "broken". The classification uses the handshake age,
//! rx/tx deltas between refreshes, the keepalive setting and the time since
//! the tunnel was seen coming up.

use std::collections::HashMap;
use std::time::Duration;

use super::status::{Peer, fmt_ago};

/// After this long without a new handshake the session keys are rejected
/// (WireGuard's REJECT_AFTER_TIME).
pub const SESSION_SECS: u64 = 180;
/// WireGuard keeps retrying a handshake for this long (REKEY_ATTEMPT_TIME)
/// before giving up on the queued packets.
pub const ATTEMPT_SECS: u64 = 90;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Health {
    Inactive,
    /// A handshake is being attempted and is still within its retry window.
    Connecting,
    /// A handshake completed within the last 180 s.
    Healthy,
    /// Quiet tunnel: no recent traffic and no keepalive, so no handshake is
    /// expected. Normal.
    Idle,
    /// Up, but nothing has been sent or received yet.
    NoTraffic,
    /// Persistent keepalive is on, yet the handshake has expired.
    HandshakeStale,
    /// Packets have been sent for longer than the handshake retry window with
    /// no reply, or a keepalive peer never completed a handshake.
    ConnectionFailed,
}

impl Health {
    pub fn label(self) -> &'static str {
        match self {
            Self::Inactive => "",
            Self::Connecting => "Connecting…",
            Self::Healthy => "Healthy",
            Self::Idle => "Idle",
            Self::NoTraffic => "No traffic",
            Self::HandshakeStale => "Handshake stale",
            Self::ConnectionFailed => "Connection failed",
        }
    }

    /// "good" | "warn" | "bad" | "neutral" — drives the colour in the UI.
    pub fn level(self) -> &'static str {
        match self {
            Self::Healthy => "good",
            Self::HandshakeStale => "warn",
            Self::ConnectionFailed => "bad",
            Self::Inactive | Self::Connecting | Self::Idle | Self::NoTraffic => "neutral",
        }
    }

    pub fn is_problem(self) -> bool {
        matches!(self, Self::HandshakeStale | Self::ConnectionFailed)
    }

    fn severity(self) -> u8 {
        match self {
            Self::Inactive => 0,
            Self::Idle => 1,
            Self::NoTraffic => 2,
            Self::Healthy => 3,
            Self::Connecting => 4,
            Self::HandshakeStale => 5,
            Self::ConnectionFailed => 6,
        }
    }

    pub fn explanation(self) -> &'static str {
        match self {
            Self::Inactive => "",
            Self::Connecting => "handshake in progress",
            Self::Healthy => "recent handshake",
            Self::Idle => "no recent traffic; WireGuard handshakes only when there is traffic",
            Self::NoTraffic => "nothing sent or received yet",
            Self::HandshakeStale => "keepalive is on but the handshake has expired",
            Self::ConnectionFailed => "packets sent but the peer is not answering",
        }
    }
}

/// Everything the classifier needs about one peer at one refresh.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HealthInput {
    pub active: bool,
    pub handshake_age: Option<u64>,
    pub keepalive_secs: u64,
    /// tx grew since the previous refresh.
    pub sending: bool,
    /// rx grew since the previous refresh.
    pub receiving: bool,
    /// How long tx has been growing with neither rx growth nor a fresh
    /// handshake.
    pub unanswered_secs: u64,
    /// Seconds since this app saw the interface come up (None: it was
    /// already up at start, or not yet observed).
    pub secs_since_activation: Option<u64>,
    /// Any bytes ever counted for this peer.
    pub ever_traffic: bool,
}

/// Pure classification of one peer's health.
pub fn classify(i: &HealthInput) -> Health {
    if !i.active {
        return Health::Inactive;
    }
    if i.handshake_age.is_some_and(|age| age < SESSION_SECS) {
        return Health::Healthy;
    }
    if i.unanswered_secs >= ATTEMPT_SECS {
        return Health::ConnectionFailed;
    }
    let keepalive = i.keepalive_secs > 0;
    match i.handshake_age {
        None => {
            // A keepalive peer starts handshaking immediately; if it has not
            // succeeded within the retry window plus one interval, it failed.
            if keepalive
                && i.secs_since_activation
                    .is_some_and(|s| s >= ATTEMPT_SECS + i.keepalive_secs)
            {
                Health::ConnectionFailed
            } else if i.sending || i.receiving || keepalive {
                Health::Connecting
            } else if !i.ever_traffic {
                Health::NoTraffic
            } else {
                Health::Idle
            }
        }
        Some(_) => {
            if i.sending || i.receiving {
                Health::Connecting
            } else if keepalive {
                Health::HandshakeStale
            } else {
                Health::Idle
            }
        }
    }
}

/// Interface health: the most significant peer state (a failing peer is
/// reported even when another peer is healthy).
pub fn aggregate(active: bool, peers: &[Health]) -> Health {
    if !active {
        return Health::Inactive;
    }
    peers
        .iter()
        .copied()
        .max_by_key(|h| h.severity())
        .unwrap_or(Health::NoTraffic)
}

/// One peer's health line, e.g. "Healthy (last handshake 12s ago)".
pub fn peer_summary(health: Health, handshake_age: Option<u64>) -> String {
    match (health, handshake_age) {
        (Health::Inactive, _) => String::new(),
        (Health::Healthy, Some(age)) => format!("Healthy (last handshake {} ago)", fmt_ago(age)),
        (h, Some(age)) if h != Health::Connecting => format!(
            "{} — {} (last handshake {} ago)",
            h.label(),
            h.explanation(),
            fmt_ago(age)
        ),
        (h, _) => format!("{} — {}", h.label(), h.explanation()),
    }
}

/// Interface summary: the aggregate plus how many peers share a problem.
pub fn interface_summary(active: bool, peers: &[(Health, Option<u64>)]) -> String {
    let states: Vec<Health> = peers.iter().map(|(h, _)| *h).collect();
    let overall = aggregate(active, &states);
    if overall == Health::Inactive {
        return String::new();
    }
    if peers.len() <= 1 {
        let age = peers.first().and_then(|(_, age)| *age);
        return peer_summary(overall, age);
    }
    let count = states.iter().filter(|h| **h == overall).count();
    let newest = peers.iter().filter_map(|(_, age)| *age).min();
    if overall == Health::Healthy {
        let suffix = newest
            .map(|a| format!(", last handshake {} ago", fmt_ago(a)))
            .unwrap_or_default();
        format!("Healthy ({count} of {} peers{suffix})", peers.len())
    } else {
        format!(
            "{} — {count} of {} peers: {}",
            overall.label(),
            peers.len(),
            overall.explanation()
        )
    }
}

#[derive(Clone, Copy, Debug)]
struct Sample {
    rx: u64,
    tx: u64,
    at: Duration,
    unanswered_since: Option<Duration>,
}

/// Remembers the previous refresh per (tunnel, peer) so health can use rx/tx
/// deltas, and when each tunnel was seen coming up.
#[derive(Default)]
pub struct HealthTracker {
    samples: HashMap<(String, String), Sample>,
    activated_at: HashMap<String, Duration>,
    seen_inactive: HashMap<String, bool>,
}

impl HealthTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Classify every peer of `tunnel` at monotonic time `now`.
    pub fn observe(
        &mut self,
        tunnel: &str,
        active: bool,
        peers: &[Peer],
        now: Duration,
    ) -> Vec<Health> {
        if !active {
            self.seen_inactive.insert(tunnel.to_string(), true);
            self.activated_at.remove(tunnel);
            self.samples.retain(|(t, _), _| t != tunnel);
            return vec![Health::Inactive; peers.len()];
        }
        // Only an inactive→active transition seen by this process gives a
        // trustworthy activation time; an interface up at start stays None.
        if self.seen_inactive.remove(tunnel).is_some() {
            self.activated_at.insert(tunnel.to_string(), now);
        }
        let since_activation = self
            .activated_at
            .get(tunnel)
            .map(|at| now.saturating_sub(*at).as_secs());
        peers
            .iter()
            .map(|peer| {
                let key = (tunnel.to_string(), peer.public_key.clone());
                let previous = self.samples.get(&key).copied();
                let (sending, receiving) = match previous {
                    Some(p) => (peer.tx > p.tx, peer.rx > p.rx),
                    None => (false, false),
                };
                let fresh = peer.handshake_age.is_some_and(|age| age < SESSION_SECS);
                let unanswered_since = if receiving || fresh {
                    None
                } else if sending {
                    previous
                        .and_then(|p| p.unanswered_since)
                        .or(previous.map(|p| p.at))
                } else {
                    previous.and_then(|p| p.unanswered_since)
                };
                self.samples.insert(
                    key,
                    Sample {
                        rx: peer.rx,
                        tx: peer.tx,
                        at: now,
                        unanswered_since,
                    },
                );
                classify(&HealthInput {
                    active,
                    handshake_age: peer.handshake_age,
                    keepalive_secs: peer.keepalive_secs,
                    sending,
                    receiving,
                    unanswered_secs: unanswered_since
                        .map(|since| now.saturating_sub(since).as_secs())
                        .unwrap_or(0),
                    secs_since_activation: since_activation,
                    ever_traffic: peer.rx > 0 || peer.tx > 0,
                })
            })
            .collect()
    }
}

/// Stateless classification for a one-off view (no previous refresh).
pub fn snapshot(active: bool, peers: &[Peer]) -> Vec<Health> {
    peers
        .iter()
        .map(|peer| {
            classify(&HealthInput {
                active,
                handshake_age: peer.handshake_age,
                keepalive_secs: peer.keepalive_secs,
                ever_traffic: peer.rx > 0 || peer.tx > 0,
                ..Default::default()
            })
        })
        .collect()
}

/// The clock hint is only offered as a *possible* cause, and only when
/// handshakes are actually failing while the clock is known to be unsynced.
pub fn clock_hint(health: Health, clock_synced: Option<bool>) -> Option<&'static str> {
    (health.is_problem() && clock_synced == Some(false)).then_some(
        "The system clock is not NTP-synced, which is one possible cause (peers reject handshakes from a clock that is far off).",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> HealthInput {
        HealthInput {
            active: true,
            ..Default::default()
        }
    }

    #[test]
    fn old_handshake_on_a_quiet_tunnel_is_idle_not_broken() {
        let quiet = HealthInput {
            handshake_age: Some(3600),
            ever_traffic: true,
            ..input()
        };
        assert_eq!(classify(&quiet), Health::Idle);
        assert!(!classify(&quiet).is_problem());
        assert_eq!(
            classify(&HealthInput {
                handshake_age: Some(SESSION_SECS),
                ..input()
            }),
            Health::Idle
        );
    }

    #[test]
    fn explicit_states_cover_each_situation() {
        assert_eq!(classify(&HealthInput::default()), Health::Inactive);
        assert_eq!(
            classify(&HealthInput {
                handshake_age: Some(5),
                ..input()
            }),
            Health::Healthy
        );
        assert_eq!(classify(&input()), Health::NoTraffic);
        assert_eq!(
            classify(&HealthInput {
                sending: true,
                unanswered_secs: 10,
                ..input()
            }),
            Health::Connecting
        );
        assert_eq!(
            classify(&HealthInput {
                sending: true,
                unanswered_secs: ATTEMPT_SECS,
                ..input()
            }),
            Health::ConnectionFailed
        );
        assert_eq!(
            classify(&HealthInput {
                handshake_age: Some(400),
                keepalive_secs: 25,
                ..input()
            }),
            Health::HandshakeStale
        );
        // Stale handshake, traffic resuming: a re-handshake, not a failure.
        assert_eq!(
            classify(&HealthInput {
                handshake_age: Some(4000),
                sending: true,
                unanswered_secs: 2,
                ..input()
            }),
            Health::Connecting
        );
    }

    #[test]
    fn keepalive_peer_that_never_handshakes_fails_after_the_retry_window() {
        let mut i = HealthInput {
            keepalive_secs: 25,
            secs_since_activation: Some(30),
            ..input()
        };
        assert_eq!(classify(&i), Health::Connecting);
        i.secs_since_activation = Some(ATTEMPT_SECS + 25);
        assert_eq!(classify(&i), Health::ConnectionFailed);
        // Unknown activation time (already up at start): no false alarm.
        i.secs_since_activation = None;
        assert_eq!(classify(&i), Health::Connecting);
    }

    fn peer(key: &str, age: Option<u64>, rx: u64, tx: u64) -> Peer {
        Peer {
            public_key: key.into(),
            handshake_age: age,
            rx,
            tx,
            ..Default::default()
        }
    }

    #[test]
    fn tracker_uses_deltas_and_reports_per_peer() {
        let mut t = HealthTracker::new();
        let s = Duration::from_secs;
        // Two peers: one healthy, one sending into the void.
        let peers = [peer("a", Some(10), 100, 100), peer("b", Some(5000), 0, 100)];
        assert_eq!(
            t.observe("wg0", true, &peers, s(0)),
            vec![Health::Healthy, Health::Idle]
        );
        let peers = [peer("a", Some(11), 200, 200), peer("b", Some(5001), 0, 200)];
        assert_eq!(
            t.observe("wg0", true, &peers, s(1)),
            vec![Health::Healthy, Health::Connecting]
        );
        let peers = [peer("a", Some(12), 300, 300), peer("b", Some(5100), 0, 900)];
        let states = t.observe("wg0", true, &peers, s(100));
        assert_eq!(states, vec![Health::Healthy, Health::ConnectionFailed]);
        assert_eq!(aggregate(true, &states), Health::ConnectionFailed);
        // A reply clears the failure.
        let peers = [peer("a", Some(13), 300, 300), peer("b", Some(1), 50, 950)];
        assert_eq!(
            t.observe("wg0", true, &peers, s(101)),
            vec![Health::Healthy, Health::Healthy]
        );
    }

    #[test]
    fn activation_time_only_comes_from_an_observed_transition() {
        let mut t = HealthTracker::new();
        let s = Duration::from_secs;
        let mut p = peer("a", None, 0, 0);
        p.keepalive_secs = 25;
        // Already up when first seen: never marks failed from age alone.
        assert_eq!(
            t.observe("wg0", true, std::slice::from_ref(&p), s(0)),
            vec![Health::Connecting]
        );
        assert_eq!(
            t.observe("wg0", true, std::slice::from_ref(&p), s(500)),
            vec![Health::Connecting]
        );
        t.observe("wg1", false, std::slice::from_ref(&p), s(0));
        t.observe("wg1", true, std::slice::from_ref(&p), s(1));
        assert_eq!(
            t.observe(
                "wg1",
                true,
                std::slice::from_ref(&p),
                s(1 + ATTEMPT_SECS + 25)
            ),
            vec![Health::ConnectionFailed]
        );
    }

    #[test]
    fn summaries_name_states_and_peer_counts() {
        assert_eq!(
            peer_summary(Health::Healthy, Some(12)),
            "Healthy (last handshake 12s ago)"
        );
        assert!(peer_summary(Health::Idle, Some(4000)).contains("only when there is traffic"));
        let s = interface_summary(
            true,
            &[
                (Health::Healthy, Some(3)),
                (Health::ConnectionFailed, Some(900)),
                (Health::Idle, None),
            ],
        );
        assert!(s.starts_with("Connection failed — 1 of 3 peers"), "{s}");
        assert_eq!(interface_summary(false, &[(Health::Inactive, None)]), "");
        assert!(
            interface_summary(
                true,
                &[(Health::Healthy, Some(4)), (Health::Healthy, Some(9))]
            )
            .contains("2 of 2")
        );
    }

    #[test]
    fn clock_is_only_a_possible_cause_when_handshakes_fail() {
        assert!(clock_hint(Health::Healthy, Some(false)).is_none());
        assert!(clock_hint(Health::Idle, Some(false)).is_none());
        assert!(clock_hint(Health::ConnectionFailed, Some(true)).is_none());
        assert!(clock_hint(Health::ConnectionFailed, None).is_none());
        let hint = clock_hint(Health::ConnectionFailed, Some(false)).unwrap();
        assert!(hint.contains("possible cause"));
    }
}
