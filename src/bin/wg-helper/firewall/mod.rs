//! The kill switch: reject every locally generated packet that does not leave
//! through the tunnel, while letting WireGuard's own encrypted transport, the
//! loopback interface and the on-link control traffic the transport depends
//! on through.
//!
//! * nftables (preferred) uses a dedicated table — see [`nft`].
//! * iptables/ip6tables (fallback) uses a dedicated chain — see [`iptables`].
//!
//! Only one tunnel can own the kill switch at a time. Whether a switch was
//! requested is recorded in a marker under `/run` (cleared on reboot, like the
//! rules themselves), so a firewall reload that wipes the rules is reported as
//! `missing` and can be repaired instead of silently reading as `disabled`.

pub mod iptables;
pub mod nft;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::{fsutil, logging, name_ok, process::have_tool, runtime};

pub const COMMENT_PREFIX: &str = "wg-helper-killswitch:";
const STATE_DIR: &str = "/run/wg-helper";
const MARKER: &str = "/run/wg-helper/killswitch";

pub fn kill_comment(name: &str) -> String {
    format!("{COMMENT_PREFIX}{name}")
}

pub fn comment_tunnel(comment: &str) -> Option<String> {
    comment
        .strip_prefix(COMMENT_PREFIX)
        .filter(|name| name_ok(name))
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// Listing parser shared by both backends
// ---------------------------------------------------------------------------

/// Parse the quoting emitted by nft/iptables listing commands; never interpret
/// quoted comment contents as more rule syntax.
pub fn rule_tokens(line: &str) -> Result<Vec<(String, bool)>, String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    let mut escaped = false;
    let mut started = false;
    let mut quoted = false;
    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            started = true;
            continue;
        }
        if c == '\\' && quote != Some('\'') {
            escaped = true;
            quoted = true;
            continue;
        }
        if let Some(q) = quote {
            if c == q {
                quote = None;
            } else {
                word.push(c);
            }
            continue;
        }
        if matches!(c, '\'' | '"') {
            quote = Some(c);
            started = true;
            quoted = true;
        } else if c.is_whitespace() {
            if started {
                words.push((std::mem::take(&mut word), quoted));
                started = false;
                quoted = false;
            }
        } else {
            word.push(c);
            started = true;
        }
    }
    if quote.is_some() || escaped {
        return Err("malformed firewall rule listing".into());
    }
    if started {
        words.push((word, quoted));
    }
    Ok(words)
}

pub fn rule_words(line: &str) -> Result<Vec<String>, String> {
    Ok(rule_tokens(line)?
        .into_iter()
        .map(|(word, _)| word)
        .collect())
}

pub fn rule_comment(line: &str) -> Option<String> {
    let words = rule_tokens(line).ok()?;
    for (i, (word, quoted)) in words.iter().enumerate() {
        if !quoted
            && (word == "--comment"
                || (word == "comment"
                    && i.checked_sub(1).is_none_or(|p| {
                        !matches!(
                            words[p].0.as_str(),
                            "-m" | "--match"
                                | "-o"
                                | "--out-interface"
                                | "-i"
                                | "--in-interface"
                                | "oif"
                                | "oifname"
                                | "iif"
                                | "iifname"
                        )
                    })))
        {
            return words.get(i + 1).map(|(value, _)| value.clone());
        }
    }
    None
}

pub fn rule_has_comment(line: &str, expected: &str) -> bool {
    rule_comment(line).as_deref() == Some(expected)
}

/// What one managed rule does, as recognised by a backend.
pub struct RuleMatch {
    /// Allowed traffic (nft `accept`, iptables `RETURN`, or `ACCEPT` in the
    /// layout of older releases).
    pub accept: bool,
    pub reject: bool,
    pub loopback: bool,
    pub oif: Option<String>,
    pub mark: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    V4,
    V6,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Nft,
    NftLegacy,
    Iptables(Family),
    IptablesLegacy(Family),
}

/// A kill switch (complete or not) found in one backend listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub tunnel: String,
    pub source: Source,
    /// The transport mark the rules allow, when exactly one is present.
    pub mark: Option<u32>,
    /// Loopback, tunnel egress, transport mark and the final reject are all
    /// present (and the backend-specific placement checks passed).
    pub complete: bool,
}

pub fn parse_rules(
    text: &str,
    source: Source,
    recognise: impl Fn(&[&str], &[(String, bool)]) -> RuleMatch,
) -> Vec<Found> {
    #[derive(Default)]
    struct Acc {
        loopback: bool,
        tunnel: bool,
        reject: bool,
        malformed: bool,
        marks: std::collections::BTreeSet<u32>,
    }
    let mut by_tunnel: BTreeMap<String, Acc> = BTreeMap::new();
    for line in text.lines() {
        let Some(tunnel) = rule_comment(line).and_then(|c| comment_tunnel(&c)) else {
            continue;
        };
        let acc = by_tunnel.entry(tunnel.clone()).or_default();
        let Ok(tokens) = rule_tokens(line) else {
            acc.malformed = true;
            continue;
        };
        let words: Vec<&str> = tokens.iter().map(|(w, _)| w.as_str()).collect();
        let m = recognise(&words, &tokens);
        if m.reject {
            acc.reject = true;
        } else if m.accept {
            acc.loopback |= m.loopback;
            acc.tunnel |= m.oif.as_deref() == Some(tunnel.as_str());
            if let Some(mark) = m.mark {
                acc.marks.insert(mark);
            }
        }
    }
    by_tunnel
        .into_iter()
        .map(|(tunnel, acc)| {
            let mark = (acc.marks.len() == 1)
                .then(|| acc.marks.first().copied())
                .flatten();
            Found {
                tunnel,
                source,
                mark,
                complete: acc.loopback
                    && acc.tunnel
                    && mark.is_some()
                    && acc.reject
                    && !acc.malformed,
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

/// Traffic the kill switch lets through, in evaluation order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Allow {
    Loopback,
    /// Plaintext packets routed into the tunnel interface (they carry no mark;
    /// only WireGuard's encapsulated packets do, on a second pass).
    Tunnel,
    /// WireGuard's own encrypted UDP transport, tagged with the runtime fwmark.
    Mark(u32),
    /// IPv6 router/neighbor solicitation and neighbor advertisement.
    NeighborDiscovery,
    /// MLDv2 reports, so switches with MLD snooping keep delivering the
    /// solicited-node multicast that Neighbor Discovery relies on.
    MulticastListener,
    /// DHCP client traffic, so the underlying address lease is renewed.
    Dhcp(Family),
    /// Replies on an SSH session the switch was enabled from.
    Ssh(u16),
}

pub fn allow_list(mark: u32, ssh: Option<u16>) -> Vec<Allow> {
    let mut allows = vec![
        Allow::Loopback,
        Allow::Tunnel,
        Allow::Mark(mark),
        Allow::NeighborDiscovery,
        Allow::MulticastListener,
        Allow::Dhcp(Family::V4),
        Allow::Dhcp(Family::V6),
    ];
    if let Some(port) = ssh {
        allows.push(Allow::Ssh(port));
    }
    allows
}

fn ssh_port() -> Option<u16> {
    std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|s| s.split_whitespace().nth(3)?.parse::<u16>().ok())
}

pub fn ipv6_available() -> bool {
    Path::new("/proc/net/if_inet6").exists()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Complete rules whose transport mark matches the running interface.
    Enabled,
    Disabled,
    /// Requested, but the rules are gone or incomplete (e.g. a firewall reload).
    Missing,
    /// Rules are present but stale: the mark differs from the runtime fwmark,
    /// the interface is down, they use the layout of an older release, or
    /// another backend still holds leftovers.
    Mismatch,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Enabled => "enabled",
            Status::Disabled => "disabled",
            Status::Missing => "missing",
            Status::Mismatch => "mismatch",
        }
    }
}

/// Everything status decisions depend on, gathered without side effects.
pub struct View<'a> {
    pub found: &'a [Found],
    pub ipv6: bool,
    pub marker: Option<&'a str>,
    pub active: bool,
    pub runtime_mark: Option<u32>,
}

pub fn decide(name: &str, view: &View) -> Status {
    let mine: Vec<&Found> = view.found.iter().filter(|f| f.tunnel == name).collect();
    let complete = |source: Source| mine.iter().any(|f| f.source == source && f.complete);
    let families = |make: fn(Family) -> Source| {
        complete(make(Family::V4)) && (!view.ipv6 || complete(make(Family::V6)))
    };
    let protecting: Option<Vec<Source>> = if complete(Source::Nft) {
        Some(vec![Source::Nft])
    } else if families(Source::Iptables) {
        Some(vec![
            Source::Iptables(Family::V4),
            Source::Iptables(Family::V6),
        ])
    } else {
        None
    };
    if let Some(sources) = protecting {
        let used: Vec<&&Found> = mine
            .iter()
            .filter(|f| sources.contains(&f.source))
            .collect();
        let marks_match =
            view.runtime_mark.is_some() && used.iter().all(|f| f.mark == view.runtime_mark);
        let leftovers = mine.iter().any(|f| !sources.contains(&f.source));
        return if view.active && marks_match && !leftovers {
            Status::Enabled
        } else {
            Status::Mismatch
        };
    }
    if complete(Source::NftLegacy) || families(Source::IptablesLegacy) {
        return Status::Mismatch;
    }
    if !mine.is_empty() || view.marker == Some(name) {
        Status::Missing
    } else {
        Status::Disabled
    }
}

// ---------------------------------------------------------------------------
// Requested-state marker
// ---------------------------------------------------------------------------

fn read_marker() -> Result<Option<String>, String> {
    match fs::symlink_metadata(MARKER) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("inspect {MARKER}: {e}")),
        Ok(_) => {}
    }
    fsutil::check_private_dir(Path::new(STATE_DIR))?;
    let text = fsutil::read_config_text(Path::new(MARKER))?;
    let name = text.trim();
    if !name_ok(name) {
        return Err(format!("invalid kill switch marker in {MARKER}"));
    }
    Ok(Some(name.to_string()))
}

fn write_marker(name: &str) -> Result<(), String> {
    fsutil::ensure_private_dir(Path::new(STATE_DIR))?;
    fsutil::atomic_write(Path::new(MARKER), &format!("{name}\n"))
}

fn clear_marker(name: &str) -> Result<(), String> {
    if read_marker()?.as_deref() == Some(name) {
        fs::remove_file(MARKER).map_err(|e| format!("remove {MARKER}: {e}"))?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Observation and changes
// ---------------------------------------------------------------------------

struct Observation {
    found: Vec<Found>,
    nft_table: Option<String>,
    nft_legacy: Option<String>,
    iptables: Vec<(Family, String)>,
    ipv6: bool,
}

fn observe() -> Result<Observation, String> {
    let ipv6 = ipv6_available();
    let mut found = Vec::new();
    let (mut nft_table, mut nft_legacy) = (None, None);
    if nft::usable() {
        nft_table = nft::snapshot()?;
        nft_legacy = nft::legacy_snapshot()?;
        if let Some(text) = &nft_table {
            found.extend(nft::parse(text, Source::Nft));
        }
        if let Some(text) = &nft_legacy {
            found.extend(nft::parse(text, Source::NftLegacy));
        }
    }
    let mut tables = Vec::new();
    for family in [Family::V4, Family::V6] {
        if family == Family::V6 && !ipv6 {
            continue;
        }
        if have_tool(iptables::tool(family)) {
            let text = iptables::snapshot(family)?;
            found.extend(iptables::parse(&text, family));
            tables.push((family, text));
        }
    }
    Ok(Observation {
        found,
        nft_table,
        nft_legacy,
        iptables: tables,
        ipv6,
    })
}

fn current_status(name: &str) -> Result<Status, String> {
    let obs = observe()?;
    let marker = read_marker()?;
    let active = runtime::interface_active(name)?;
    let runtime_mark = if active {
        runtime::runtime_fwmark(name)?
    } else {
        None
    };
    Ok(decide(
        name,
        &View {
            found: &obs.found,
            ipv6: obs.ipv6,
            marker: marker.as_deref(),
            active,
            runtime_mark,
        },
    ))
}

fn remove_nft(name: &str, obs: &Observation) -> Result<(), String> {
    if obs.nft_table.is_none() && obs.nft_legacy.is_none() {
        return Ok(());
    }
    nft::apply(&nft::remove_script(
        name,
        obs.nft_table.as_deref(),
        obs.nft_legacy.as_deref(),
    )?)
}

fn remove_iptables(name: &str, obs: &Observation) -> Result<(), String> {
    let mut errors = Vec::new();
    for (family, text) in &obs.iptables {
        if let Err(error) = iptables::apply(*family, &iptables::script(text, name, None, *family)) {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn install_iptables(name: &str, allows: &[Allow], obs: &Observation) -> Result<(), String> {
    iptables::require_installed(obs.ipv6)?;
    let mut batches = Vec::new();
    for family in [Family::V4, Family::V6] {
        if family == Family::V6 && !obs.ipv6 {
            continue;
        }
        let text = obs
            .iptables
            .iter()
            .find(|(f, _)| *f == family)
            .map(|(_, text)| text.as_str())
            .ok_or_else(|| format!("{} listing unavailable", iptables::tool(family)))?;
        let batch = iptables::script(text, name, Some(allows), family);
        // Parse and check BOTH families before the first commit. The kernel
        // cannot transact across families; status requires both rule sets.
        iptables::test(family, &batch)?;
        batches.push((family, batch));
    }
    for (family, batch) in batches {
        iptables::apply(family, &batch)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Enable,
    Repair,
}

fn install(name: &str, mode: Mode) -> Result<(), String> {
    let obs = observe()?;
    let marker = read_marker()?;
    if let Some(other) = obs.found.iter().find(|f| f.tunnel != name) {
        return Err(format!(
            "kill switch for {} is already installed; disable it before enabling {name}",
            other.tunnel
        ));
    }
    if mode == Mode::Repair
        && marker.as_deref() != Some(name)
        && !obs.found.iter().any(|f| f.tunnel == name && f.complete)
    {
        return Err(format!(
            "no kill switch is enabled for {name}; turn it on again instead of repairing"
        ));
    }
    let use_nft = nft::usable();
    if !use_nft {
        iptables::require_installed(obs.ipv6)?;
    }
    let mark = runtime::ensure_tunnel_has_fwmark(name)?;
    let allows = allow_list(mark, ssh_port());
    // Install the new protection first; remove leftovers from the other
    // backend (or an older layout) only once it is in place: fail closed.
    if use_nft {
        nft::apply(&nft::install_script(
            name,
            &allows,
            obs.nft_legacy.as_deref(),
        )?)?;
        remove_iptables(name, &obs)?;
    } else {
        install_iptables(name, &allows, &obs)?;
        remove_nft(name, &obs)?;
    }
    write_marker(name)?;
    match current_status(name)? {
        Status::Enabled => Ok(()),
        other => Err(format!(
            "kill switch rules were installed but verification reports '{}'",
            other.as_str()
        )),
    }
}

// ---------------------------------------------------------------------------
// Verbs
// ---------------------------------------------------------------------------

pub fn status(name: &str) -> Result<(), String> {
    println!("{}", current_status(name)?.as_str());
    Ok(())
}

pub fn enable(name: &str) -> Result<(), String> {
    install(name, Mode::Enable)?;
    logging::event("killswitch-enable", Some(name), &[]);
    Ok(())
}

pub fn repair(name: &str) -> Result<(), String> {
    install(name, Mode::Repair)?;
    logging::event("killswitch-repair", Some(name), &[]);
    Ok(())
}

pub fn disable(name: &str) -> Result<(), String> {
    // Record the intent first, so a partial cleanup is never "repaired" back.
    let mut errors = Vec::new();
    if let Err(error) = clear_marker(name) {
        errors.push(error);
    }
    match observe() {
        Ok(obs) => {
            if let Err(error) = remove_nft(name, &obs) {
                errors.push(error);
            }
            if let Err(error) = remove_iptables(name, &obs) {
                errors.push(error);
            }
        }
        Err(error) => errors.push(error),
    }
    if !errors.is_empty() {
        return Err(format!("kill switch cleanup failed: {}", errors.join("; ")));
    }
    logging::event("killswitch-disable", Some(name), &[]);
    Ok(())
}

/// Whether a kill switch was requested or is (partly) present for `name`.
pub fn requested(name: &str) -> Result<bool, String> {
    Ok(
        read_marker()?.as_deref() == Some(name)
            || observe()?.found.iter().any(|f| f.tunnel == name),
    )
}

/// After a live config change, make sure the kill switch still matches the
/// interface, rebuilding it from the runtime state when it does not.
pub fn reconcile_after_sync(name: &str) -> Result<(), String> {
    if !requested(name)? {
        return Ok(());
    }
    match current_status(name)? {
        Status::Enabled => Ok(()),
        _ => repair(name).map_err(|e| format!("kill switch needs attention after sync: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(tunnel: &str, source: Source, mark: u32, complete: bool) -> Found {
        Found {
            tunnel: tunnel.into(),
            source,
            mark: Some(mark),
            complete,
        }
    }

    fn view<'a>(found: &'a [Found], marker: Option<&'a str>, mark: Option<u32>) -> View<'a> {
        View {
            found,
            ipv6: true,
            marker,
            active: true,
            runtime_mark: mark,
        }
    }

    #[test]
    fn status_requires_the_rules_to_carry_the_runtime_mark() {
        let nft = [found("wg0", Source::Nft, 51820, true)];
        assert_eq!(
            decide("wg0", &view(&nft, Some("wg0"), Some(51820))),
            Status::Enabled
        );
        assert_eq!(
            decide("wg0", &view(&nft, Some("wg0"), Some(51821))),
            Status::Mismatch
        );
        assert_eq!(
            decide("wg0", &view(&nft, Some("wg0"), None)),
            Status::Mismatch
        );
        let down = View {
            active: false,
            ..view(&nft, Some("wg0"), Some(51820))
        };
        assert_eq!(decide("wg0", &down), Status::Mismatch);
    }

    #[test]
    fn lost_rules_are_missing_not_disabled() {
        assert_eq!(
            decide("wg0", &view(&[], Some("wg0"), Some(1))),
            Status::Missing
        );
        assert_eq!(decide("wg0", &view(&[], None, Some(1))), Status::Disabled);
        assert_eq!(
            decide("wg0", &view(&[], Some("wg1"), Some(1))),
            Status::Disabled
        );
        let partial = [found("wg0", Source::Nft, 1, false)];
        assert_eq!(
            decide("wg0", &view(&partial, None, Some(1))),
            Status::Missing
        );
    }

    #[test]
    fn iptables_needs_both_families_while_ipv6_is_enabled() {
        let v4 = [found("wg0", Source::Iptables(Family::V4), 7, true)];
        assert_eq!(
            decide("wg0", &view(&v4, Some("wg0"), Some(7))),
            Status::Missing
        );
        let no_v6 = View {
            ipv6: false,
            ..view(&v4, Some("wg0"), Some(7))
        };
        assert_eq!(decide("wg0", &no_v6), Status::Enabled);
        let both = [
            found("wg0", Source::Iptables(Family::V4), 7, true),
            found("wg0", Source::Iptables(Family::V6), 7, true),
        ];
        assert_eq!(
            decide("wg0", &view(&both, Some("wg0"), Some(7))),
            Status::Enabled
        );
    }

    #[test]
    fn older_layouts_and_leftovers_ask_for_repair() {
        let legacy = [found("wg0", Source::NftLegacy, 7, true)];
        assert_eq!(
            decide("wg0", &view(&legacy, None, Some(7))),
            Status::Mismatch
        );
        let mixed = [
            found("wg0", Source::Nft, 7, true),
            found("wg0", Source::Iptables(Family::V4), 7, false),
        ];
        assert_eq!(
            decide("wg0", &view(&mixed, Some("wg0"), Some(7))),
            Status::Mismatch
        );
    }

    #[test]
    fn other_tunnels_do_not_affect_status() {
        let other = [found("wg1", Source::Nft, 7, true)];
        assert_eq!(
            decide("wg0", &view(&other, None, Some(7))),
            Status::Disabled
        );
    }

    #[test]
    fn firewall_comments_match_only_the_exact_tunnel() {
        let comment = kill_comment("wg0");
        for line in [
            "reject comment \"wg-helper-killswitch:wg0\" # handle 12",
            "-A OUTPUT -m comment --comment wg-helper-killswitch:wg0 -j REJECT",
            "-A OUTPUT -m comment --comment \"wg-helper-killswitch:wg0\" -j REJECT",
        ] {
            assert!(rule_has_comment(line, &comment), "{line}");
            assert!(!rule_has_comment(&line.replace(":wg0", ":wg01"), &comment));
        }
        assert!(!rule_has_comment(
            "reject # wg-helper-killswitch:wg0",
            &comment
        ));
        for line in [
            "accept comment \"unrelated comment wg-helper-killswitch:wg0\"",
            "-A OUTPUT -m comment --comment \"unrelated --comment wg-helper-killswitch:wg0\" -j ACCEPT",
            "accept comment \"wg-helper-killswitch:wg0 extra\"",
        ] {
            assert!(!rule_has_comment(line, &comment), "{line}");
        }
        assert_eq!(comment_tunnel("wg-helper-killswitch:../x"), None);
        assert_eq!(
            comment_tunnel("wg-helper-killswitch:wg0").as_deref(),
            Some("wg0")
        );
    }

    #[test]
    fn firewall_keyword_interface_names_are_values_not_rule_syntax() {
        for name in ["comment", "reject", "accept", "123"] {
            let allows = allow_list(51820, None);
            let script = nft::install_script(name, &allows, None).unwrap();
            let listing = nft::tests::listing(&script);
            let found = nft::parse(&listing, Source::Nft);
            assert_eq!(found.len(), 1, "{name}");
            assert!(found[0].complete, "{name}");
            let no_reject: String = listing
                .lines()
                .filter(|l| !l.trim_start().starts_with("reject "))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !nft::parse(&no_reject, Source::Nft)[0].complete,
                "a quoted interface named {name} is not a verdict"
            );
            let batch = iptables::script("", name, Some(&allows), Family::V4);
            let found = iptables::parse(&iptables::tests::listing(&batch, &[]), Family::V4);
            assert!(found[0].complete, "{name}");
        }
    }

    #[test]
    fn allow_list_keeps_transport_and_control_traffic() {
        let allows = allow_list(51820, Some(22));
        assert_eq!(allows[0], Allow::Loopback);
        assert!(allows.contains(&Allow::Mark(51820)));
        assert!(allows.contains(&Allow::NeighborDiscovery));
        assert_eq!(allows.last(), Some(&Allow::Ssh(22)));
        assert!(
            !allow_list(1, None)
                .iter()
                .any(|a| matches!(a, Allow::Ssh(_)))
        );
    }

    /// Run the built test binary with `sudo unshare -n ... --ignored --exact
    /// firewall::tests::firewall_batches_in_isolated_network_namespace`.
    /// The guard precedes every firewall command: the test is deliberately
    /// destructive inside its disposable namespace and never touches the host.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root, nftables, iptables and a disposable network namespace"]
    fn firewall_batches_in_isolated_network_namespace() {
        use crate::process::command_output;
        use std::time::Duration;
        let own_ns = fs::read_link("/proc/self/ns/net").expect("read own network namespace");
        let init_ns = fs::read_link("/proc/1/ns/net").expect("read PID1 network namespace");
        assert_ne!(
            own_ns, init_ns,
            "refusing to alter the PID1 network namespace; run under unshare -n"
        );
        assert_eq!(unsafe { libc::geteuid() }, 0, "test requires root");
        for tool in [
            "nft",
            "iptables",
            "ip6tables",
            "iptables-restore",
            "ip6tables-restore",
        ] {
            assert!(
                have_tool(tool),
                "required integration-test tool is missing: {tool}"
            );
        }
        let nft_run = |script: &str| {
            command_output("nft", &["-f", "-"], Some(script), Duration::from_secs(10))
        };
        let allows = allow_list(51820, Some(22));

        // A foreign table with its own deny rule and a legacy rule left by an
        // older release.
        nft_run("add table inet filter\nadd chain inet filter output { type filter hook output priority 0; policy accept; }\nadd rule inet filter output ip daddr 192.0.2.1 drop comment \"admin-deny\"\nadd rule inet filter output accept comment \"wg-helper-killswitch:wg0\"\n").unwrap();
        let legacy = nft::legacy_snapshot().unwrap().unwrap();
        assert_eq!(nft::snapshot().unwrap(), None);
        nft_run(&nft::install_script("wg0", &allows, Some(&legacy)).unwrap()).unwrap();
        let table = nft::snapshot().unwrap().unwrap();
        let found = nft::parse(&table, Source::Nft);
        assert_eq!(found.len(), 1);
        assert!(found[0].complete, "{table}");
        assert_eq!(found[0].mark, Some(51820));
        let foreign = nft::legacy_snapshot().unwrap().unwrap();
        assert!(foreign.contains("admin-deny"), "foreign rules are kept");
        assert!(!foreign.contains(COMMENT_PREFIX), "legacy rule removed");

        // A failed batch keeps the previous table intact.
        let broken = format!(
            "{}add rule inet {} output this_is_invalid\n",
            nft::install_script("wg0", &allow_list(51821, None), None).unwrap(),
            nft::TABLE
        );
        assert!(nft_run(&broken).is_err());
        assert_eq!(nft::snapshot().unwrap().unwrap(), table);

        // Rebuilding replaces the table atomically with the new mark.
        nft_run(&nft::install_script("wg0", &allow_list(51821, None), None).unwrap()).unwrap();
        let rebuilt = nft::parse(&nft::snapshot().unwrap().unwrap(), Source::Nft);
        assert_eq!(rebuilt[0].mark, Some(51821));
        let table = nft::snapshot().unwrap().unwrap();
        nft_run(&nft::remove_script("wg0", Some(&table), None).unwrap()).unwrap();
        assert_eq!(nft::snapshot().unwrap(), None);
        assert!(
            nft::legacy_snapshot()
                .unwrap()
                .unwrap()
                .contains("admin-deny")
        );

        for family in [Family::V4, Family::V6] {
            let tool = iptables::tool(family);
            let run = |args: &[&str]| command_output(tool, args, None, Duration::from_secs(5));
            run(&[
                "-A",
                "OUTPUT",
                "-p",
                "tcp",
                "--dport",
                "9",
                "-m",
                "comment",
                "--comment",
                "admin-deny",
                "-j",
                "DROP",
            ])
            .unwrap();
            run(&[
                "-A",
                "OUTPUT",
                "-m",
                "comment",
                "--comment",
                "wg-helper-killswitch:wg0",
                "-j",
                "ACCEPT",
            ])
            .unwrap();
            let initial = iptables::snapshot(family).unwrap();
            let batch = iptables::script(&initial, "wg0", Some(&allows), family);
            iptables::test(family, &batch).unwrap();
            assert_eq!(
                iptables::snapshot(family).unwrap(),
                initial,
                "--test commits nothing"
            );
            iptables::apply(family, &batch).unwrap();
            let installed = iptables::snapshot(family).unwrap();
            let found = iptables::parse(&installed, family);
            assert_eq!(found.len(), 1, "{installed}");
            assert!(found[0].complete, "{installed}");
            let first = installed
                .lines()
                .find(|l| l.starts_with("-A OUTPUT "))
                .unwrap();
            assert!(
                first.ends_with(&format!("-j {}", iptables::CHAIN)),
                "{installed}"
            );
            assert!(installed.contains("admin-deny"), "foreign rules are kept");
            assert!(
                !installed
                    .lines()
                    .filter(|l| l.starts_with(&format!("-A {} ", iptables::CHAIN)))
                    .any(|l| l.contains("-j ACCEPT")),
                "exceptions must RETURN so later deny rules still apply"
            );

            let broken =
                iptables::script(&installed, "wg0", Some(&allow_list(51821, None)), family)
                    .replace("COMMIT\n", "-A OUTPUT --this-is-invalid\nCOMMIT\n");
            assert!(iptables::apply(family, &broken).is_err());
            assert_eq!(iptables::snapshot(family).unwrap(), installed);

            let rebuild =
                iptables::script(&installed, "wg0", Some(&allow_list(51821, None)), family);
            iptables::apply(family, &rebuild).unwrap();
            let rebuilt = iptables::snapshot(family).unwrap();
            let found = iptables::parse(&rebuilt, family);
            assert_eq!(found.len(), 1, "{rebuilt}");
            assert_eq!(found[0].mark, Some(51821));
            assert_eq!(
                rebuilt.matches(&format!("-j {}", iptables::CHAIN)).count(),
                1
            );

            iptables::apply(family, &iptables::script(&rebuilt, "wg0", None, family)).unwrap();
            let cleaned = iptables::snapshot(family).unwrap();
            assert!(!cleaned.contains(COMMENT_PREFIX), "{cleaned}");
            assert!(!cleaned.contains(iptables::CHAIN), "{cleaned}");
            assert!(cleaned.contains("admin-deny"));
        }
    }
}
