//! Property tests for config parsing and validation (`src/config.rs`) and the
//! templates in `src/create.rs`. The modules are compiled straight from the
//! source tree (the binaries have no library crate), the same way the helper
//! pulls in `config.rs`.

#[allow(dead_code)]
#[path = "../src/config.rs"]
mod config;
#[allow(dead_code)]
#[path = "../src/create.rs"]
mod create;
mod prop_support;
#[allow(dead_code)]
#[path = "../src/secrets.rs"]
mod secrets;

use config::{
    ParsedConfig, is_endpoint, is_wg_key, looks_like_inet, parse_config,
    validate_basic_wireguard_config,
};
use prop_support::{Rng, check, wg_key};

// ---------------------------------------------------------------- input builders

fn ipv4(rng: &mut Rng) -> String {
    format!(
        "{}.{}.{}.{}",
        rng.below(256),
        rng.below(256),
        rng.below(256),
        rng.below(256)
    )
}

fn ipv6(rng: &mut Rng) -> String {
    let groups: Vec<String> = (0..8)
        .map(|_| format!("{:x}", rng.below(0x1_0000)))
        .collect();
    if rng.chance(1, 3) {
        // Compressed form: keep a prefix, elide the rest.
        let keep = 1 + rng.below(6);
        format!("{}::{}", groups[..keep].join(":"), groups[7])
    } else {
        groups.join(":")
    }
}

fn cidr(rng: &mut Rng) -> String {
    if rng.chance(1, 2) {
        let ip = ipv4(rng);
        if rng.chance(1, 5) {
            ip
        } else {
            format!("{ip}/{}", rng.below(33))
        }
    } else {
        let ip = ipv6(rng);
        if rng.chance(1, 5) {
            ip
        } else {
            format!("{ip}/{}", rng.below(129))
        }
    }
}

fn hostname(rng: &mut Rng) -> String {
    let labels = 1 + rng.below(3);
    let mut parts = Vec::new();
    for _ in 0..labels {
        let len = 1 + rng.below(10);
        let mut label: String = (0..len)
            .map(|_| *rng.pick(&['a', 'b', 'z', '0', '7', '-']))
            .collect();
        // RFC 1123: no leading/trailing hyphen; first label not all digits.
        label = label.trim_matches('-').to_string();
        if label.is_empty() {
            label.push('h');
        }
        parts.push(label);
    }
    parts.push("example".into());
    let mut host = parts.join(".");
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.') {
        host.insert(0, 'h');
    }
    host
}

fn port(rng: &mut Rng) -> u32 {
    1 + rng.below(65_535) as u32
}

fn endpoint(rng: &mut Rng) -> String {
    match rng.below(4) {
        0 => format!("{}:{}", ipv4(rng), port(rng)),
        1 => format!("[{}]:{}", ipv6(rng), port(rng)),
        2 => format!(
            "[fe80::{:x}%eth{}]:{}",
            rng.below(0xffff),
            rng.below(10),
            port(rng)
        ),
        _ => format!("{}:{}", hostname(rng), port(rng)),
    }
}

fn list(rng: &mut Rng, sep_choices: &[&str], item: fn(&mut Rng) -> String) -> String {
    let n = 1 + rng.below(3);
    let sep = *rng.pick(sep_choices);
    (0..n).map(|_| item(rng)).collect::<Vec<_>>().join(sep)
}

struct Sample {
    text: String,
    private_key: String,
    peer_keys: Vec<String>,
}

/// A random config that wg-quick would accept, written with the syntactic
/// freedom the parser is supposed to tolerate: any ASCII key casing, spacing
/// around `=`, inline/whole-line comments, blank lines, repeated multi-value
/// directives, CRLF line endings, and peers before the interface section.
fn valid_config(rng: &mut Rng) -> Sample {
    let private_key = wg_key(rng);
    let mut iface: Vec<String> = Vec::new();
    let directive = |rng: &mut Rng, key: &str, value: &str| {
        let comment = if rng.chance(1, 6) { " # note" } else { "" };
        format!(
            "{}{}={}{}{comment}",
            rng.recase(key),
            rng.spaces(),
            rng.spaces(),
            value
        )
    };
    iface.push(directive(rng, "PrivateKey", &private_key));
    for _ in 0..rng.below(3) {
        let value = list(rng, &[", ", ",", " "], cidr);
        iface.push(directive(rng, "Address", &value));
    }
    if rng.chance(1, 2) {
        let value = list(rng, &[", ", " "], |r| {
            if r.chance(1, 3) { hostname(r) } else { ipv4(r) }
        });
        iface.push(directive(rng, "DNS", &value));
    }
    if rng.chance(1, 3) {
        let value = rng.below(65_536).to_string();
        iface.push(directive(rng, "ListenPort", &value));
    }
    if rng.chance(1, 4) {
        let value = (1 + rng.below(9000)).to_string();
        iface.push(directive(rng, "MTU", &value));
    }
    if rng.chance(1, 4) {
        let value = rng.pick(&["off", "auto", "main", "1234"]).to_string();
        iface.push(directive(rng, "Table", &value));
    }
    if rng.chance(1, 4) {
        let value = rng.pick(&["off", "0", "51820", "0xca6c"]).to_string();
        iface.push(directive(rng, "FwMark", &value));
    }
    if rng.chance(1, 5) {
        let value = rng.pick(&["true", "false", "TRUE"]).to_string();
        iface.push(directive(rng, "SaveConfig", &value));
    }

    let mut sections = vec![format!("[{}]", rng.recase("Interface")), iface.join("\n")];
    let mut peers = Vec::new();
    let mut peer_keys = Vec::new();
    for _ in 0..rng.below(4) {
        let key = wg_key(rng);
        let mut lines = vec![
            format!("[{}]", rng.recase("Peer")),
            directive(rng, "PublicKey", &key),
        ];
        if rng.chance(1, 3) {
            let psk = wg_key(rng);
            lines.push(directive(rng, "PresharedKey", &psk));
        }
        for _ in 0..1 + rng.below(2) {
            let value = list(rng, &[", ", ","], cidr);
            lines.push(directive(rng, "AllowedIPs", &value));
        }
        if rng.chance(2, 3) {
            let value = endpoint(rng);
            lines.push(directive(rng, "Endpoint", &value));
        }
        if rng.chance(1, 2) {
            let value = if rng.chance(1, 5) {
                "off".to_string()
            } else {
                rng.below(65_536).to_string()
            };
            lines.push(directive(rng, "PersistentKeepalive", &value));
        }
        peers.push(lines.join("\n"));
        peer_keys.push(key);
    }
    if rng.chance(1, 6) {
        // wg-quick does not require [Interface] to come first.
        sections.splice(0..0, peers.drain(..));
    } else {
        sections.append(&mut peers);
    }

    let mut text = String::new();
    for block in sections {
        if rng.chance(1, 4) {
            text.push_str("# comment line\n");
        }
        if rng.chance(1, 4) {
            text.push('\n');
        }
        text.push_str(&block);
        text.push('\n');
    }
    if rng.chance(1, 5) {
        text = text.replace('\n', "\r\n");
    }
    Sample {
        text,
        private_key,
        peer_keys,
    }
}

/// Apply a few random edits that a corrupted or hostile file might contain.
fn mutate(rng: &mut Rng, text: &str) -> String {
    let mut chars: Vec<char> = text.chars().collect();
    for _ in 0..1 + rng.below(4) {
        let at = if chars.is_empty() {
            0
        } else {
            rng.below(chars.len() + 1)
        };
        match rng.below(5) {
            0 if at < chars.len() => {
                chars.remove(at);
            }
            1 => {
                let insert = rng.messy_text(4);
                for (i, c) in insert.chars().enumerate() {
                    chars.insert(at + i, c);
                }
            }
            2 if at < chars.len() => {
                chars[at] = *rng.pick(&['=', '#', '[', ']', '\n', ' ', 'x', '0', ':', '/']);
            }
            3 => {
                // Duplicate a whole line somewhere else.
                let s: String = chars.iter().collect();
                let lines: Vec<&str> = s.lines().collect();
                if !lines.is_empty() {
                    let line = format!("{}\n", rng.pick(&lines));
                    let mut out: Vec<char> = s.chars().collect();
                    let pos = rng.below(out.len() + 1);
                    for (i, c) in line.chars().enumerate() {
                        out.insert(pos + i, c);
                    }
                    chars = out;
                }
            }
            _ => {
                let s: String = chars.iter().collect();
                chars = s.replacen('\n', "\n\n", 1).chars().collect();
            }
        }
    }
    chars.into_iter().collect()
}

/// Render the parsed model back to canonical wg-quick text.
fn render(cfg: &ParsedConfig) -> String {
    let mut out = String::from("[Interface]\n");
    if let Some(v) = &cfg.private_key {
        out.push_str(&format!("PrivateKey = {v}\n"));
    }
    if let Some(v) = &cfg.address {
        out.push_str(&format!("Address = {v}\n"));
    }
    if let Some(v) = &cfg.dns {
        out.push_str(&format!("DNS = {v}\n"));
    }
    if let Some(v) = &cfg.listen_port {
        out.push_str(&format!("ListenPort = {v}\n"));
    }
    for p in &cfg.peers {
        out.push_str("\n[Peer]\n");
        out.push_str(&format!("PublicKey = {}\n", p.public_key));
        if !p.preshared_key.is_empty() {
            out.push_str(&format!("PresharedKey = {}\n", p.preshared_key));
        }
        out.push_str(&format!("AllowedIPs = {}\n", p.allowed_ips));
        if !p.endpoint.is_empty() {
            out.push_str(&format!("Endpoint = {}\n", p.endpoint));
        }
        if !p.keepalive.is_empty() {
            out.push_str(&format!("PersistentKeepalive = {}\n", p.keepalive));
        }
    }
    out
}

fn same_model(a: &ParsedConfig, b: &ParsedConfig) -> Result<(), String> {
    let fields = [
        ("PrivateKey", &a.private_key, &b.private_key),
        ("Address", &a.address, &b.address),
        ("DNS", &a.dns, &b.dns),
        ("ListenPort", &a.listen_port, &b.listen_port),
    ];
    for (name, x, y) in fields {
        if x != y {
            return Err(format!("{name}: {x:?} != {y:?}"));
        }
    }
    if a.peers.len() != b.peers.len() {
        return Err(format!("peer count {} != {}", a.peers.len(), b.peers.len()));
    }
    for (i, (p, q)) in a.peers.iter().zip(&b.peers).enumerate() {
        let pairs = [
            ("PublicKey", &p.public_key, &q.public_key),
            ("PresharedKey", &p.preshared_key, &q.preshared_key),
            ("AllowedIPs", &p.allowed_ips, &q.allowed_ips),
            ("Endpoint", &p.endpoint, &q.endpoint),
            ("PersistentKeepalive", &p.keepalive, &q.keepalive),
        ];
        for (name, x, y) in pairs {
            if x != y {
                return Err(format!("peer {i} {name}: {x:?} != {y:?}"));
            }
        }
    }
    Ok(())
}

/// What every accepted config must guarantee to the code that consumes it.
fn assert_accepted_invariants(text: &str) {
    if validate_basic_wireguard_config(text).is_err() {
        return;
    }
    assert!(text.len() <= 1024 * 1024, "accepted an oversized config");
    assert!(
        !text
            .bytes()
            .any(|b| b == 0 || (b.is_ascii_control() && !matches!(b, b'\t' | b'\r' | b'\n'))),
        "accepted control characters: {text:?}"
    );
    let parsed = parse_config(text);
    let key = parsed
        .private_key
        .as_deref()
        .expect("accepted without PrivateKey");
    assert!(
        is_wg_key(key),
        "accepted malformed PrivateKey {key:?} in {text:?}"
    );
    for (i, peer) in parsed.peers.iter().enumerate() {
        assert!(
            is_wg_key(&peer.public_key),
            "peer {i} accepted malformed PublicKey in {text:?}"
        );
        assert!(
            !peer.allowed_ips.is_empty(),
            "peer {i} accepted without AllowedIPs"
        );
        for net in peer.allowed_ips.split(',') {
            assert!(looks_like_inet(net), "peer {i} accepted AllowedIPs {net:?}");
        }
        if !peer.endpoint.is_empty() {
            assert!(
                is_endpoint(&peer.endpoint),
                "peer {i} accepted Endpoint {:?}",
                peer.endpoint
            );
        }
        if !peer.preshared_key.is_empty() {
            assert!(
                is_wg_key(&peer.preshared_key),
                "peer {i} accepted malformed PresharedKey"
            );
        }
    }
}

// ---------------------------------------------------------------- properties

#[test]
fn random_valid_configs_are_accepted_and_parsed_faithfully() {
    check("valid_configs", |rng| {
        let g = valid_config(rng);
        if let Err(e) = validate_basic_wireguard_config(&g.text) {
            panic!("rejected a valid config ({e}):\n{}", g.text);
        }
        let parsed = parse_config(&g.text);
        assert_eq!(parsed.private_key.as_deref(), Some(g.private_key.as_str()));
        let keys: Vec<&str> = parsed.peers.iter().map(|p| p.public_key.as_str()).collect();
        let expected: Vec<&str> = g.peer_keys.iter().map(String::as_str).collect();
        assert_eq!(keys, expected, "peer order or keys changed:\n{}", g.text);
        assert!(!secrets::config_runs_scripts(&g.text));
    });
}

#[test]
fn parse_render_round_trip_is_stable() {
    check("round_trip", |rng| {
        let g = valid_config(rng);
        let first = parse_config(&g.text);
        let rendered = render(&first);
        if let Err(e) = validate_basic_wireguard_config(&rendered) {
            panic!(
                "canonical rendering is invalid ({e}):\n{rendered}\nfrom:\n{}",
                g.text
            );
        }
        let second = parse_config(&rendered);
        if let Err(e) = same_model(&first, &second) {
            panic!(
                "round trip changed {e}\nrendered:\n{rendered}\nfrom:\n{}",
                g.text
            );
        }
        assert_eq!(render(&second), rendered, "rendering is not a fixed point");
    });
}

#[test]
fn line_ending_style_does_not_change_the_verdict() {
    check("crlf_invariance", |rng| {
        let base = valid_config(rng).text.replace("\r\n", "\n");
        let text = if rng.chance(1, 2) {
            mutate(rng, &base)
        } else {
            base
        };
        let lf = validate_basic_wireguard_config(&text).is_ok();
        let crlf = validate_basic_wireguard_config(&text.replace('\n', "\r\n")).is_ok();
        assert_eq!(lf, crlf, "LF vs CRLF verdict differs for {text:?}");
    });
}

#[test]
fn mutated_configs_never_panic_and_accepted_ones_are_well_formed() {
    check("mutations", |rng| {
        let g = valid_config(rng);
        let text = mutate(rng, &g.text);
        let _ = parse_config(&text);
        assert_accepted_invariants(&text);
    });
}

#[test]
fn arbitrary_text_never_panics() {
    check("arbitrary_text", |rng| {
        let text = rng.messy_text(300);
        let _ = parse_config(&text);
        assert_accepted_invariants(&text);
        for line in text.lines() {
            let _ = is_endpoint(line);
            let _ = looks_like_inet(line);
            let _ = is_wg_key(line);
        }
        let _ = secrets::redact_config(&text);
        let _ = secrets::config_runs_scripts(&text);
    });
}

#[test]
fn canonical_keys_are_accepted_and_near_misses_rejected() {
    const LAST: &[u8] = b"AEIMQUYcgkosw048";
    check("keys", |rng| {
        let key = wg_key(rng);
        assert!(is_wg_key(&key), "{key}");
        assert!(
            is_wg_key(&format!(" {key}\t")),
            "surrounding ASCII space is trimmed"
        );
        // Any other final symbol encodes nonzero padding bits.
        let mut bad = key.clone().into_bytes();
        let replacement = loop {
            let c = *rng.pick(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/");
            if !LAST.contains(&c) {
                break c;
            }
        };
        bad[42] = replacement;
        assert!(!is_wg_key(std::str::from_utf8(&bad).unwrap()));
        assert!(!is_wg_key(&key[..43]), "missing padding");
        assert!(!is_wg_key(&format!("A{key}")), "too long");
        let mut wrong_alphabet = key.clone().into_bytes();
        wrong_alphabet[rng.below(42)] = *rng.pick(b"-_.*!");
        assert!(!is_wg_key(std::str::from_utf8(&wrong_alphabet).unwrap()));
    });
}

#[test]
fn endpoints_and_cidrs_follow_their_grammar() {
    check("endpoints_cidrs", |rng| {
        let ep = endpoint(rng);
        assert!(is_endpoint(&ep), "{ep}");
        let host = hostname(rng);
        assert!(!is_endpoint(&format!("{host}:0")), "port 0");
        let too_big = 65_536 + rng.below(1_000_000);
        assert!(!is_endpoint(&format!("{host}:{too_big}")));
        assert!(!is_endpoint(&host), "missing port");
        assert!(
            !is_endpoint(&format!("{}:{}", ipv6(rng), port(rng))),
            "unbracketed IPv6"
        );

        let net = cidr(rng);
        assert!(looks_like_inet(&net), "{net}");
        assert!(!looks_like_inet(&format!(
            "{}/{}",
            ipv4(rng),
            33 + rng.below(100)
        )));
        assert!(!looks_like_inet(&format!(
            "{}/{}",
            ipv6(rng),
            129 + rng.below(100)
        )));
        assert!(!looks_like_inet(&format!(
            "{}/+{}",
            ipv4(rng),
            rng.below(33)
        )));
    });
}

#[test]
fn every_template_from_create_validates_once_completed() {
    use create::{TunnelTemplateKind, generate_template};
    check("templates", |rng| {
        let private = wg_key(rng);
        let peer = wg_key(rng);
        for kind in [
            TunnelTemplateKind::InterfaceOnly,
            TunnelTemplateKind::ClientFullTunnel,
            TunnelTemplateKind::ClientSplitTunnel,
        ] {
            let template = generate_template(kind, &private);
            // Client templates leave the server's PublicKey for the user to
            // fill in; the result must validate once they do.
            let completed = template.replace("PublicKey = \n", &format!("PublicKey = {peer}\n"));
            if let Err(e) = validate_basic_wireguard_config(&completed) {
                panic!("{} template is invalid ({e}):\n{completed}", kind.label());
            }
            let parsed = parse_config(&completed);
            assert_eq!(parsed.private_key.as_deref(), Some(private.as_str()));
            assert!(!secrets::config_runs_scripts(&completed));
            let redacted = secrets::redact_config(&completed);
            assert!(
                !redacted.contains(&private),
                "template key survived redaction"
            );
        }
    });
}
