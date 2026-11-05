//! nftables backend: one dedicated table with its own OUTPUT base chain.
//!
//! nftables evaluates every base chain registered on a hook. `accept` ends
//! evaluation only for the chain that issued it, while `drop`/`reject` are
//! final for the packet. A dedicated table therefore lets the kill switch
//! reject leaks without its exceptions overriding any deny rule another table
//! (firewalld, a distro ruleset, the administrator) applies to the same packet,
//! and it never edits a table the helper does not own.

use std::time::Duration;

use super::{Allow, Family, Found, Source, kill_comment, parse_rules, rule_has_comment};
use crate::process::{command_output, have_tool};

pub const TABLE: &str = "wg_helper_killswitch";

/// Older releases inserted rules into `inet filter output`.
const LEGACY: [&str; 3] = ["inet", "filter", "output"];

fn list(args: &[&str]) -> Result<Option<String>, String> {
    match command_output("nft", args, None, Duration::from_secs(5)) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.contains("No such file or directory") => Ok(None),
        Err(error) => Err(error),
    }
}

/// Whether nftables is installed and the kernel answers it.
pub fn usable() -> bool {
    have_tool("nft")
        && command_output("nft", &["list", "tables"], None, Duration::from_secs(5)).is_ok()
}

pub fn snapshot() -> Result<Option<String>, String> {
    list(&["-a", "list", "table", "inet", TABLE])
}

pub fn legacy_snapshot() -> Result<Option<String>, String> {
    list(&["-a", "list", "chain", LEGACY[0], LEGACY[1], LEGACY[2]])
}

pub fn is_output_base_chain(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim().starts_with("type filter hook output priority "))
}

/// Parse an nft listing into the kill switches it contains.
pub fn parse(text: &str, source: Source) -> Vec<Found> {
    let base = is_output_base_chain(text);
    parse_rules(text, source, |words, tokens| {
        let verdict = |v: &str| tokens.iter().any(|(w, quoted)| !quoted && w == v);
        let pairs = |a: &str, b: &str| words.windows(2).any(|w| w[0] == a && w[1] == b);
        super::RuleMatch {
            accept: verdict("accept"),
            reject: verdict("reject"),
            loopback: pairs("oif", "lo") || pairs("oifname", "lo"),
            oif: words
                .windows(2)
                .find(|w| w[0] == "oifname" && w[1] != "lo")
                .map(|w| w[1].to_string()),
            mark: words
                .windows(3)
                .find(|w| w[0] == "meta" && w[1] == "mark")
                .and_then(|w| crate::runtime::parse_fwmark(w[2]).ok()),
        }
    })
    .into_iter()
    .map(|mut found| {
        found.complete &= base;
        found
    })
    .collect()
}

fn expression(allow: &Allow, name: &str) -> Vec<String> {
    match allow {
        Allow::Loopback => vec!["oifname \"lo\"".into()],
        Allow::Tunnel => vec![format!("oifname \"{name}\"")],
        Allow::Mark(mark) => vec![format!("meta mark {mark}")],
        // Neighbor Discovery resolves the link-layer address of the next hop
        // towards the endpoint. It is unmarked locally generated ICMPv6; block
        // it and an IPv6 endpoint becomes unreachable once the neighbor cache
        // entry expires. Hop limit 255 keeps it on-link (RFC 4861).
        Allow::NeighborDiscovery => vec![
            "icmpv6 type { nd-router-solicit, nd-neighbor-solicit, nd-neighbor-advert } ip6 hoplimit 255"
                .into(),
        ],
        Allow::MulticastListener => vec!["icmpv6 type mld2-listener-report ip6 hoplimit 1".into()],
        Allow::Dhcp(Family::V4) => vec!["meta nfproto ipv4 udp sport 68 udp dport 67".into()],
        Allow::Dhcp(Family::V6) => vec!["meta nfproto ipv6 udp sport 546 udp dport 547".into()],
        Allow::Ssh(port) => vec![format!("tcp sport {port} ct state established,related")],
    }
}

fn delete_legacy(script: &mut String, legacy: Option<&str>, name: &str) -> Result<(), String> {
    let Some(text) = legacy else {
        return Ok(());
    };
    let comment = kill_comment(name);
    for line in text.lines().filter(|line| rule_has_comment(line, &comment)) {
        let handle = line
            .rsplit_once("# handle ")
            .and_then(|(_, h)| h.trim().parse::<u64>().ok())
            .ok_or("managed nft rule is missing its handle")?;
        script.push_str(&format!(
            "delete rule {} {} {} handle {handle}\n",
            LEGACY[0], LEGACY[1], LEGACY[2]
        ));
    }
    Ok(())
}

/// One transaction: drop legacy rules for this tunnel, then atomically replace
/// the dedicated table. A failure anywhere leaves the previous ruleset intact.
pub fn install_script(
    name: &str,
    allows: &[Allow],
    legacy: Option<&str>,
) -> Result<String, String> {
    let comment = kill_comment(name);
    let mut script = String::new();
    delete_legacy(&mut script, legacy, name)?;
    // `add` then `delete` makes the delete valid whether or not it exists.
    script.push_str(&format!(
        "add table inet {TABLE}\ndelete table inet {TABLE}\n"
    ));
    script.push_str(&format!("add table inet {TABLE}\n"));
    script.push_str(&format!(
        "add chain inet {TABLE} output {{ type filter hook output priority 0; policy accept; }}\n"
    ));
    for allow in allows {
        for expr in expression(allow, name) {
            script.push_str(&format!(
                "add rule inet {TABLE} output {expr} accept comment \"{comment}\"\n"
            ));
        }
    }
    script.push_str(&format!(
        "add rule inet {TABLE} output reject comment \"{comment}\"\n"
    ));
    Ok(script)
}

/// Remove this tunnel's switch. The dedicated table is deleted only when it
/// holds nothing but this tunnel's rules.
pub fn remove_script(
    name: &str,
    table: Option<&str>,
    legacy: Option<&str>,
) -> Result<String, String> {
    let mut script = String::new();
    delete_legacy(&mut script, legacy, name)?;
    if let Some(text) = table {
        let found = parse(text, Source::Nft);
        if found.iter().all(|f| f.tunnel == name) {
            script.push_str(&format!("delete table inet {TABLE}\n"));
        }
    }
    Ok(script)
}

pub fn apply(script: &str) -> Result<(), String> {
    if script.is_empty() {
        return Ok(());
    }
    command_output("nft", &["-f", "-"], Some(script), Duration::from_secs(10)).map(|_| ())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::firewall::allow_list;

    /// Render a script's rules the way `nft -a list table` prints them.
    pub fn listing(script: &str) -> String {
        let mut out = format!(
            "table inet {TABLE} {{ # handle 1\n\tchain output {{ # handle 1\n\t\ttype filter hook output priority filter; policy accept;\n"
        );
        for (i, line) in script
            .lines()
            .filter_map(|l| l.strip_prefix(&format!("add rule inet {TABLE} output ")))
            .enumerate()
        {
            out.push_str(&format!("\t\t{line} # handle {}\n", i + 2));
        }
        out.push_str("\t}\n}\n");
        out
    }

    #[test]
    fn install_replaces_the_dedicated_table_in_one_transaction() {
        let script = install_script("wg0", &allow_list(51820, None), None).unwrap();
        let lines: Vec<_> = script.lines().collect();
        assert_eq!(lines[0], format!("add table inet {TABLE}"));
        assert_eq!(lines[1], format!("delete table inet {TABLE}"));
        assert!(lines[3].contains("type filter hook output priority 0; policy accept;"));
        assert!(lines.last().unwrap().contains(" reject comment "));
        assert!(
            !script.contains("inet filter"),
            "never edits a foreign table"
        );
        let found = parse(&listing(&script), Source::Nft);
        assert_eq!(found.len(), 1);
        assert!(found[0].complete);
        assert_eq!(found[0].mark, Some(51820));
    }

    #[test]
    fn ipv6_neighbor_discovery_is_allowed_before_the_reject() {
        let script = install_script("wg0", &allow_list(51820, None), None).unwrap();
        let nd = script.find("nd-neighbor-solicit").unwrap();
        let reject = script.find(" reject ").unwrap();
        assert!(nd < reject);
        assert!(script.contains("ip6 hoplimit 255"));
    }

    #[test]
    fn legacy_rules_are_removed_with_the_install() {
        let legacy = "type filter hook output priority 0; policy accept;\nreject comment \"wg-helper-killswitch:wg0\" # handle 5\naccept comment \"unrelated\" # handle 6";
        let script = install_script("wg0", &allow_list(51820, None), Some(legacy)).unwrap();
        assert!(script.starts_with("delete rule inet filter output handle 5\n"));
        assert!(!script.contains("handle 6"));
        assert!(
            install_script(
                "wg0",
                &[],
                Some("reject comment \"wg-helper-killswitch:wg0\"")
            )
            .is_err(),
            "a managed rule without a handle cannot be removed safely"
        );
    }

    #[test]
    fn removal_keeps_a_table_that_holds_another_tunnel() {
        let mine = listing(&install_script("wg0", &allow_list(51820, None), None).unwrap());
        assert!(
            remove_script("wg0", Some(&mine), None)
                .unwrap()
                .contains(&format!("delete table inet {TABLE}"))
        );
        assert!(remove_script("wg1", Some(&mine), None).unwrap().is_empty());
    }

    #[test]
    fn a_regular_chain_is_not_protection() {
        let script = install_script("wg0", &allow_list(51820, None), None).unwrap();
        let regular = listing(&script).replace(
            "type filter hook output priority filter; policy accept;",
            "",
        );
        assert!(!parse(&regular, Source::Nft)[0].complete);
    }
}
