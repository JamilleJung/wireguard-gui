//! iptables/ip6tables backend (used when nftables is unavailable): a dedicated
//! chain reached by one jump at the top of OUTPUT.
//!
//! In iptables an `ACCEPT` inside a jumped-to chain accepts the packet for the
//! whole table, skipping every later OUTPUT rule. The kill switch chain
//! therefore never accepts: allowed traffic `RETURN`s to OUTPUT and continues
//! through the administrator's own rules (which may still deny it); everything
//! else is rejected inside the chain.

use std::time::Duration;

use super::{Allow, Family, Found, Source, kill_comment, parse_rules, rule_comment};
use crate::process::{command_output, have_tool};

pub const CHAIN: &str = "WGH-KILLSWITCH";

pub fn tool(family: Family) -> &'static str {
    match family {
        Family::V4 => "iptables",
        Family::V6 => "ip6tables",
    }
}

pub fn require_backends(
    v4: bool,
    v4_restore: bool,
    v6: bool,
    v6_restore: bool,
    ipv6: bool,
) -> Result<(), String> {
    if !v4 || !v4_restore {
        return Err("kill switch requires iptables and iptables-restore for IPv4".into());
    }
    if ipv6 && (!v6 || !v6_restore) {
        return Err(
            "kill switch requires ip6tables and ip6tables-restore while IPv6 is enabled".into(),
        );
    }
    Ok(())
}

pub fn require_installed(ipv6: bool) -> Result<(), String> {
    require_backends(
        have_tool("iptables"),
        have_tool("iptables-restore"),
        have_tool("ip6tables"),
        have_tool("ip6tables-restore"),
        ipv6,
    )
}

/// The whole filter table in `-S` form (chains and rules).
pub fn snapshot(family: Family) -> Result<String, String> {
    command_output(
        tool(family),
        &["-w", "5", "-S"],
        None,
        Duration::from_secs(10),
    )
}

fn is_output(line: &str) -> bool {
    line.starts_with("-A OUTPUT ")
}

fn is_chain_rule(line: &str) -> bool {
    line.starts_with(&format!("-A {CHAIN} "))
}

fn is_jump(line: &str) -> bool {
    is_output(line) && line.trim_end().ends_with(&format!("-j {CHAIN}"))
}

pub fn parse(text: &str, family: Family) -> Vec<Found> {
    let first_output = text.lines().find(|l| is_output(l));
    let rule = |words: &[&str], tokens: &[(String, bool)]| {
        let _ = tokens;
        let target = words.windows(2).rev().find(|w| w[0] == "-j").map(|w| w[1]);
        super::RuleMatch {
            accept: matches!(target, Some("RETURN" | "ACCEPT")),
            reject: target == Some("REJECT"),
            loopback: words.windows(2).any(|w| w[0] == "-o" && w[1] == "lo"),
            oif: words
                .windows(2)
                .find(|w| w[0] == "-o" && w[1] != "lo")
                .map(|w| w[1].to_string()),
            mark: words
                .windows(2)
                .find(|w| w[0] == "--mark")
                .and_then(|w| crate::runtime::parse_fwmark(w[1].split('/').next()?).ok()),
        }
    };
    // Current layout: rules in the dedicated chain plus the OUTPUT jump.
    let chain: String = text
        .lines()
        .filter(|l| is_chain_rule(l))
        .collect::<Vec<_>>()
        .join("\n");
    let mut found: Vec<Found> = parse_rules(&chain, Source::Iptables(family), rule)
        .into_iter()
        .map(|mut f| {
            let comment = kill_comment(&f.tunnel);
            // The jump must come first: an earlier rule that accepts would
            // let traffic skip the kill switch entirely.
            let jump_first = first_output
                .is_some_and(|l| is_jump(l) && rule_comment(l).as_deref() == Some(&comment));
            f.complete &= jump_first;
            f
        })
        .collect();
    // A jump without its chain rules is a partial (unprotected) install.
    for line in text.lines().filter(|l| is_jump(l)) {
        if let Some(tunnel) = rule_comment(line).and_then(|c| super::comment_tunnel(&c))
            && !found.iter().any(|f| f.tunnel == tunnel)
        {
            found.push(Found {
                tunnel,
                source: Source::Iptables(family),
                mark: None,
                complete: false,
            });
        }
    }
    // Layout of older releases: exception and reject rules directly in OUTPUT.
    let legacy: String = text
        .lines()
        .filter(|l| is_output(l) && !is_jump(l))
        .collect::<Vec<_>>()
        .join("\n");
    found.extend(parse_rules(&legacy, Source::IptablesLegacy(family), rule));
    found
}

fn rules(allow: &Allow, name: &str, family: Family) -> Vec<Vec<String>> {
    let s = |v: &[&str]| vec![v.iter().map(|s| (*s).to_string()).collect::<Vec<_>>()];
    match (allow, family) {
        (Allow::Loopback, _) => s(&["-o", "lo"]),
        (Allow::Tunnel, _) => s(&["-o", name]),
        (Allow::Mark(mark), _) => s(&["-m", "mark", "--mark", &mark.to_string()]),
        (Allow::NeighborDiscovery, Family::V6) => ["133", "135", "136"]
            .iter()
            .map(|t| {
                [
                    "-p",
                    "ipv6-icmp",
                    "-m",
                    "icmp6",
                    "--icmpv6-type",
                    t,
                    "-m",
                    "hl",
                    "--hl-eq",
                    "255",
                ]
                .iter()
                .map(|s| (*s).to_string())
                .collect()
            })
            .collect(),
        (Allow::MulticastListener, Family::V6) => s(&[
            "-p",
            "ipv6-icmp",
            "-m",
            "icmp6",
            "--icmpv6-type",
            "143",
            "-m",
            "hl",
            "--hl-eq",
            "1",
        ]),
        (Allow::Dhcp(Family::V4), Family::V4) => {
            s(&["-p", "udp", "-m", "udp", "--sport", "68", "--dport", "67"])
        }
        (Allow::Dhcp(Family::V6), Family::V6) => {
            s(&["-p", "udp", "-m", "udp", "--sport", "546", "--dport", "547"])
        }
        (Allow::Ssh(port), _) => s(&[
            "-p",
            "tcp",
            "-m",
            "tcp",
            "--sport",
            &port.to_string(),
            "-m",
            "conntrack",
            "--ctstate",
            "RELATED,ESTABLISHED",
        ]),
        _ => Vec::new(),
    }
}

/// One `iptables-restore --noflush` batch for a family. With `install`, the
/// chain is (re)built and the jump re-inserted at the top of OUTPUT; without,
/// this tunnel's chain and jump are removed. Old-layout OUTPUT rules for the
/// tunnel are removed in both cases. Rules of other chains are never touched.
pub fn script(text: &str, name: &str, install: Option<&[Allow]>, family: Family) -> String {
    let comment = kill_comment(name);
    let chain_exists = text.lines().any(|l| l.trim() == format!("-N {CHAIN}"));
    let chain_owned = text
        .lines()
        .filter(|l| is_chain_rule(l))
        .all(|l| rule_comment(l).as_deref() == Some(&comment));
    let mut lines = vec!["*filter".to_string()];
    lines.extend(
        text.lines()
            .filter(|l| is_output(l) && rule_comment(l).as_deref() == Some(&comment))
            .map(|l| l.replacen("-A OUTPUT ", "-D OUTPUT ", 1)),
    );
    let quoted = format!("\"{comment}\"");
    if let Some(allows) = install {
        lines.push(if chain_exists {
            format!("-F {CHAIN}")
        } else {
            format!(":{CHAIN} - [0:0]")
        });
        for allow in allows {
            for rule in rules(allow, name, family) {
                lines.push(format!(
                    "-A {CHAIN} {} -m comment --comment {quoted} -j RETURN",
                    rule.join(" ")
                ));
            }
        }
        lines.push(format!(
            "-A {CHAIN} -m comment --comment {quoted} -j REJECT"
        ));
        lines.push(format!(
            "-I OUTPUT 1 -m comment --comment {quoted} -j {CHAIN}"
        ));
    } else if chain_exists && chain_owned {
        lines.push(format!("-F {CHAIN}"));
        lines.push(format!("-X {CHAIN}"));
    }
    lines.push("COMMIT".into());
    format!("{}\n", lines.join("\n"))
}

/// Whether a batch would change anything beyond the table header and commit.
pub fn has_changes(script: &str) -> bool {
    script.lines().count() > 2
}

pub fn test(family: Family, script: &str) -> Result<(), String> {
    command_output(
        &format!("{}-restore", tool(family)),
        &["--noflush", "--wait", "5", "--test"],
        Some(script),
        Duration::from_secs(10),
    )
    .map(|_| ())
}

pub fn apply(family: Family, script: &str) -> Result<(), String> {
    if !has_changes(script) {
        return Ok(());
    }
    let restore = format!("{}-restore", tool(family));
    if have_tool(&restore) {
        return command_output(
            &restore,
            &["--noflush", "--wait", "5"],
            Some(script),
            Duration::from_secs(10),
        )
        .map(|_| ());
    }
    // Removal of rules from older releases stays possible even if the restore
    // utility has since been uninstalled.
    for line in script
        .lines()
        .filter(|l| l.starts_with("-D ") || l.starts_with("-F ") || l.starts_with("-X "))
    {
        let words = super::rule_words(line)?;
        let mut args = vec!["-w", "5"];
        args.extend(words.iter().map(String::as_str));
        command_output(tool(family), &args, None, Duration::from_secs(5))?;
    }
    Ok(())
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::firewall::allow_list;

    /// Render an install batch the way `iptables -S` lists it afterwards.
    pub fn listing(script: &str, existing_output: &[&str]) -> String {
        let mut out = vec![
            "-P INPUT ACCEPT".to_string(),
            "-P FORWARD ACCEPT".into(),
            "-P OUTPUT ACCEPT".into(),
            format!("-N {CHAIN}"),
        ];
        for line in script.lines() {
            if let Some(rest) = line.strip_prefix("-I OUTPUT 1 ") {
                out.push(format!("-A OUTPUT {rest}"));
            }
        }
        out.extend(existing_output.iter().map(|s| (*s).to_string()));
        out.extend(
            script
                .lines()
                .filter(|l| l.starts_with(&format!("-A {CHAIN} ")))
                .map(str::to_string),
        );
        out.join("\n")
    }

    #[test]
    fn allowed_traffic_returns_instead_of_accepting() {
        let batch = script("wg0", "wg0", Some(&allow_list(51820, Some(22))), Family::V4);
        let chain: Vec<_> = batch
            .lines()
            .filter(|l| l.starts_with(&format!("-A {CHAIN} ")))
            .collect();
        assert!(chain.iter().all(|l| !l.contains("-j ACCEPT")));
        assert!(chain.last().unwrap().ends_with("-j REJECT"));
        assert!(
            chain[..chain.len() - 1]
                .iter()
                .all(|l| l.ends_with("-j RETURN"))
        );
        assert!(batch.contains(&format!(
            "-I OUTPUT 1 -m comment --comment \"wg-helper-killswitch:wg0\" -j {CHAIN}"
        )));
        assert!(batch.contains(&format!(":{CHAIN} - [0:0]")));
    }

    #[test]
    fn neighbor_discovery_is_only_in_the_ipv6_chain() {
        let allows = allow_list(51820, None);
        let v6 = script("", "wg0", Some(&allows), Family::V6);
        let v4 = script("", "wg0", Some(&allows), Family::V4);
        for t in ["133", "135", "136"] {
            assert!(v6.contains(&format!("--icmpv6-type {t} -m hl --hl-eq 255")));
        }
        assert!(!v4.contains("icmp6"));
        assert!(v4.contains("--sport 68 --dport 67"));
        assert!(v6.contains("--sport 546 --dport 547"));
    }

    #[test]
    fn listing_round_trips_and_requires_the_jump_first() {
        let allows = allow_list(51820, None);
        let batch = script("", "wg0", Some(&allows), Family::V4);
        let ok = listing(&batch, &["-A OUTPUT -j ACCEPT"]);
        let found = parse(&ok, Family::V4);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].complete);
        assert_eq!(found[0].mark, Some(51820));
        // Another rule inserted ahead of the jump defeats the switch.
        let bypassed = ok.replacen(
            "-A OUTPUT -m comment",
            "-A OUTPUT -j ACCEPT\n-A OUTPUT -m comment",
            1,
        );
        assert!(!parse(&bypassed, Family::V4)[0].complete);
        // A jump without its chain is not protection.
        let jump_only: String = ok
            .lines()
            .filter(|l| !l.starts_with("-A WGH"))
            .collect::<Vec<_>>()
            .join("\n");
        let found = parse(&jump_only, Family::V4);
        assert_eq!(found.len(), 1);
        assert!(!found[0].complete);
    }

    #[test]
    fn rebuild_flushes_the_existing_chain_and_removes_the_old_jump() {
        let allows = allow_list(51820, None);
        let installed = listing(&script("", "wg0", Some(&allows), Family::V4), &[]);
        let again = script(
            &installed,
            "wg0",
            Some(&allow_list(51821, None)),
            Family::V4,
        );
        assert!(again.contains(&format!("-F {CHAIN}")));
        assert!(!again.contains(&format!(":{CHAIN}")));
        assert!(again.contains(&format!(
            "-D OUTPUT -m comment --comment \"wg-helper-killswitch:wg0\" -j {CHAIN}"
        )));
        let removal = script(&installed, "wg0", None, Family::V4);
        assert!(removal.ends_with(&format!("-F {CHAIN}\n-X {CHAIN}\nCOMMIT\n")));
        // Another tunnel's chain is never deleted.
        assert!(!script(&installed, "wg1", None, Family::V4).contains("-X"));
    }

    #[test]
    fn legacy_output_rules_are_recognised_and_removed() {
        let legacy = "-A OUTPUT -o wg0 -m comment --comment \"wg-helper-killswitch:wg0\" -j ACCEPT\n-A OUTPUT -o lo -m comment --comment \"wg-helper-killswitch:wg0\" -j ACCEPT\n-A OUTPUT -m mark --mark 0xca6c -m comment --comment \"wg-helper-killswitch:wg0\" -j ACCEPT\n-A OUTPUT -m comment --comment \"wg-helper-killswitch:wg0\" -j REJECT\n-A OUTPUT -j ACCEPT";
        let found = parse(legacy, Family::V4);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].source, Source::IptablesLegacy(Family::V4));
        assert!(found[0].complete);
        let batch = script(legacy, "wg0", Some(&allow_list(51820, None)), Family::V4);
        assert_eq!(batch.matches("-D OUTPUT").count(), 4);
        assert!(!batch.contains("-D OUTPUT -j ACCEPT"));
    }

    #[test]
    fn fallback_requires_every_supported_address_family() {
        assert!(require_backends(false, false, true, true, true).is_err());
        assert!(require_backends(true, true, false, false, true).is_err());
        assert!(require_backends(true, false, true, true, true).is_err());
        assert!(require_backends(true, true, false, false, false).is_ok());
        assert!(require_backends(true, true, true, true, true).is_ok());
    }
}
