//! Talks to WireGuard through the privileged `wg-helper` script.
//!
//! Everything that needs root (reading /etc/wireguard, `wg show`, `wg-quick`)
//! goes through `helper()`, which shells out as:
//!   * nothing            — when we are already root
//!   * `sudo -n wg-helper`— the normal case (NOPASSWD sudoers drop-in)
//!   * `pkexec wg-helper` — fallback when sudo is not set up (prompts)

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Clone, Copy, PartialEq)]
enum Escalation {
    Direct,
    Sudo,
    Pkexec,
}

static ESC: OnceLock<Escalation> = OnceLock::new();
static HELPER: OnceLock<String> = OnceLock::new();

/// Decide once how we gain privilege and where the helper lives.
pub fn init() {
    let esc = if unsafe { libc::geteuid() } == 0 {
        Escalation::Direct
    } else if Command::new("sudo")
        .args(["-n", helper_path(), "list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        Escalation::Sudo
    } else {
        Escalation::Pkexec
    };
    let _ = ESC.set(esc);
}

/// Resolve the helper path: $WG_HELPER, the installed location, or the
/// in-tree copy used during `cargo run`.
fn helper_path() -> &'static str {
    HELPER.get_or_init(|| {
        if let Ok(p) = std::env::var("WG_HELPER") {
            return p;
        }
        let candidates = [
            "/usr/local/lib/wireguard-gui/wg-helper",
            "/usr/lib/wireguard-gui/wg-helper",
        ];
        for c in candidates {
            if PathBuf::from(c).exists() {
                return c.to_string();
            }
        }
        // dev fallback: <manifest>/packaging/wg-helper
        let dev = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("packaging/wg-helper");
        dev.to_string_lossy().into_owned()
    })
}

/// Run the helper with a verb (+ optional name) and optional stdin payload.
fn helper(args: &[&str], stdin: Option<&str>) -> Result<String, String> {
    let esc = *ESC.get().unwrap_or(&Escalation::Pkexec);
    let helper = helper_path();

    let mut cmd = match esc {
        Escalation::Direct => {
            let mut c = Command::new(helper);
            c.args(args);
            c
        }
        Escalation::Sudo => {
            let mut c = Command::new("sudo");
            c.arg("-n").arg(helper).args(args);
            c
        }
        Escalation::Pkexec => {
            let mut c = Command::new("pkexec");
            c.arg(helper).args(args);
            c
        }
    };

    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let mut child = cmd.spawn().map_err(|e| format!("spawn failed: {e}"))?;
    if let Some(payload) = stdin {
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .map_err(|e| format!("write stdin: {e}"))?;
    }
    let out = child
        .wait_with_output()
        .map_err(|e| format!("wait failed: {e}"))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("{} {}: {}", helper, args.join(" "), err.trim()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

// ---------------------------------------------------------------------------
// Data model handed up to the UI layer.
// ---------------------------------------------------------------------------

pub struct Tunnel {
    pub name: String,
    pub active: bool,
}

#[derive(Default)]
pub struct Peer {
    pub public_key: String,
    pub preshared: bool,
    pub allowed_ips: String,
    pub endpoint: String,
    pub keepalive: String,
    pub latest_handshake: String,
    pub transfer: String,
}

#[derive(Default)]
pub struct Detail {
    pub name: String,
    pub active: bool,
    pub autostart: bool,
    pub public_key: String,
    pub listen_port: String,
    pub addresses: String,
    pub dns: String,
    pub peers: Vec<Peer>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Demo mode (`WGGUI_DEMO=1`) shows polished fake tunnels for screenshots —
/// no helper calls, no real `/etc/wireguard` access.
fn demo_mode() -> bool {
    std::env::var("WGGUI_DEMO")
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn demo_tunnels() -> Vec<Tunnel> {
    [
        ("home-server", true),
        ("work-vpn", false),
        ("tokyo-relay", false),
        ("us-east-1", false),
    ]
    .iter()
    .map(|(n, a)| Tunnel {
        name: n.to_string(),
        active: *a,
    })
    .collect()
}

fn demo_detail(name: &str) -> Detail {
    let active = name == "home-server";
    Detail {
        name: name.to_string(),
        active,
        autostart: active,
        public_key: "Hk3pQ2vN8sLrYwZ1aFcJ4mD6tB9eU0xKgPiR7oVnQ4=".to_string(),
        listen_port: if active {
            "51820".into()
        } else {
            String::new()
        },
        addresses: "10.7.0.2/24, fd00:7::2/64".to_string(),
        dns: "1.1.1.1, 1.0.0.1".to_string(),
        peers: vec![Peer {
            public_key: "T9bXm2Kp5LqWv8RcZ1hN6sJ3dY7uA0eFgB4iO+wQ5k=".to_string(),
            preshared: true,
            allowed_ips: "0.0.0.0/0, ::/0".to_string(),
            endpoint: "vpn.example.com:51820".to_string(),
            keepalive: "25".to_string(),
            latest_handshake: if active {
                "38 seconds ago".into()
            } else {
                String::new()
            },
            transfer: if active {
                "1.24 GiB received, 318.66 MiB sent".into()
            } else {
                String::new()
            },
        }],
    }
}

pub fn list_tunnels() -> Vec<Tunnel> {
    if demo_mode() {
        return demo_tunnels();
    }
    let names = helper(&["list"], None).unwrap_or_default();
    let active: Vec<String> = helper(&["active"], None)
        .unwrap_or_default()
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    names
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| Tunnel {
            name: n.to_string(),
            active: active.iter().any(|a| a == n),
        })
        .collect()
}

pub fn tunnel_exists(name: &str) -> bool {
    list_tunnels().iter().any(|t| t.name == name)
}

/// A collision-free tunnel name based on `base`: returns `base`, else
/// `base-2`, `base-3`, … (kept within the 15-char interface-name limit).
pub fn unique_name(base: &str) -> String {
    let base = sanitize_name(base);
    if !tunnel_exists(&base) {
        return base;
    }
    for n in 2..1000 {
        let suffix = format!("-{n}");
        let keep = 15usize.saturating_sub(suffix.len());
        let candidate = format!("{}{}", base.chars().take(keep).collect::<String>(), suffix);
        if !tunnel_exists(&candidate) {
            return candidate;
        }
    }
    base
}

pub fn read_config(name: &str) -> Result<String, String> {
    if demo_mode() {
        return Ok(format!(
            "[Interface]\nPrivateKey = oMM7e2Kf4pQ1sLrYwZ1aFcJ4mD6tB9eU0xKgPiR7oV=\n\
             Address = 10.7.0.2/24, fd00:7::2/64\nDNS = 1.1.1.1, 1.0.0.1\n\n\
             [Peer]\nPublicKey = T9bXm2Kp5LqWv8RcZ1hN6sJ3dY7uA0eFgB4iO+wQ5k=\n\
             PresharedKey = aBcDeF2Kf4pQ1sLrYwZ1aFcJ4mD6tB9eU0xKgPiR7oV=\n\
             AllowedIPs = 0.0.0.0/0, ::/0\nEndpoint = vpn.example.com:51820\n\
             PersistentKeepalive = 25\n# tunnel: {name}\n"
        ));
    }
    helper(&["read", name], None)
}

pub fn save_config(name: &str, content: &str) -> Result<(), String> {
    helper(&["save", name], Some(content)).map(|_| ())
}

pub fn activate(name: &str) -> Result<(), String> {
    helper(&["up", name], None).map(|_| ())
}

pub fn deactivate(name: &str) -> Result<(), String> {
    helper(&["down", name], None).map(|_| ())
}

pub fn delete(name: &str) -> Result<(), String> {
    helper(&["delete", name], None).map(|_| ())
}

/// Recent WireGuard-related log lines (this app's audit log + wg-quick units).
pub fn get_log() -> String {
    if demo_mode() {
        return "===== wireguard-gui activity =====\n\
                2026-06-16T09:41:02 home  wireguard-gui: user=demo action=up home-server\n\
                2026-06-16T09:41:02 home  wireguard-gui: user=demo action=save home-server\n\
                2026-06-16T09:12:55 home  wireguard-gui: user=demo action=down work-vpn\n\
                \n===== wg-quick (systemd) =====\n\
                2026-06-16T09:41:02 home  systemd: Started WireGuard via wg-quick(8) for home-server.\n"
            .to_string();
    }
    match helper(&["log"], None) {
        Ok(s) if !s.trim().is_empty() => s,
        Ok(_) => "(no recent log entries)".to_string(),
        Err(e) => format!("Could not read the log: {e}"),
    }
}

/// Build the full detail view for a tunnel by merging its on-disk config with
/// the live `wg show <name> dump` output.
pub fn get_detail(name: &str) -> Detail {
    if demo_mode() {
        return demo_detail(name);
    }
    let cfg = read_config(name).unwrap_or_default();
    let parsed = parse_config(&cfg);
    let dump = helper(&["dump", name], None).unwrap_or_default();
    let live = parse_dump(&dump);

    let active = !dump.trim().is_empty();

    // Interface public key: prefer the live value, else derive from privkey.
    let public_key = live
        .as_ref()
        .map(|l| l.iface_public.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| parsed.private_key.as_deref().and_then(pubkey_of))
        .unwrap_or_default();

    let listen_port = live
        .as_ref()
        .map(|l| l.listen_port.clone())
        .filter(|s| !s.is_empty())
        .or_else(|| parsed.listen_port.clone())
        .unwrap_or_default();

    let peers = parsed
        .peers
        .into_iter()
        .map(|p| {
            let lp = live
                .as_ref()
                .and_then(|l| l.peers.iter().find(|x| x.public_key == p.public_key));
            Peer {
                preshared: !p.preshared_key.is_empty(),
                allowed_ips: if p.allowed_ips.is_empty() {
                    lp.map(|x| x.allowed_ips.clone()).unwrap_or_default()
                } else {
                    p.allowed_ips.clone()
                },
                endpoint: lp
                    .map(|x| x.endpoint.clone())
                    .filter(|s| !s.is_empty() && s != "(none)")
                    .unwrap_or(p.endpoint.clone()),
                keepalive: p.keepalive.clone(),
                latest_handshake: lp
                    .map(|x| fmt_handshake(x.latest_handshake))
                    .unwrap_or_default(),
                transfer: lp.map(|x| fmt_transfer(x.rx, x.tx)).unwrap_or_default(),
                public_key: p.public_key,
            }
        })
        .collect();

    Detail {
        name: name.to_string(),
        active,
        autostart: is_autostart(name),
        public_key,
        listen_port,
        addresses: parsed.address.unwrap_or_default(),
        dns: parsed.dns.unwrap_or_default(),
        peers,
    }
}

/// `wg pubkey` is pure crypto and needs no privilege.
fn pubkey_of(private_key: &str) -> Option<String> {
    let mut child = Command::new("wg")
        .arg("pubkey")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    child
        .stdin
        .take()?
        .write_all(format!("{private_key}\n").as_bytes())
        .ok()?;
    let out = child.wait_with_output().ok()?;
    if out.status.success() {
        Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

#[derive(Default)]
struct ParsedPeer {
    public_key: String,
    preshared_key: String,
    allowed_ips: String,
    endpoint: String,
    keepalive: String,
}

#[derive(Default)]
struct ParsedConfig {
    private_key: Option<String>,
    address: Option<String>,
    dns: Option<String>,
    listen_port: Option<String>,
    peers: Vec<ParsedPeer>,
}

fn parse_config(text: &str) -> ParsedConfig {
    let mut cfg = ParsedConfig::default();
    let mut section = "";
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            let s = line.trim_matches(|c| c == '[' || c == ']').trim();
            if s.eq_ignore_ascii_case("Peer") {
                cfg.peers.push(ParsedPeer::default());
                section = "peer";
            } else if s.eq_ignore_ascii_case("Interface") {
                section = "interface";
            } else {
                section = "";
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        // Values can legitimately contain '=' (base64 keys), so rejoin.
        let value = value.trim().to_string();
        match section {
            "interface" => match key.to_ascii_lowercase().as_str() {
                "privatekey" => cfg.private_key = Some(value),
                "address" => cfg.address = Some(value),
                "dns" => cfg.dns = Some(value),
                "listenport" => cfg.listen_port = Some(value),
                _ => {}
            },
            "peer" => {
                if let Some(p) = cfg.peers.last_mut() {
                    match key.to_ascii_lowercase().as_str() {
                        "publickey" => p.public_key = value,
                        "presharedkey" => p.preshared_key = value,
                        "allowedips" => p.allowed_ips = value,
                        "endpoint" => p.endpoint = value,
                        "persistentkeepalive" => p.keepalive = value,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    cfg
}

struct LivePeer {
    public_key: String,
    endpoint: String,
    allowed_ips: String,
    latest_handshake: u64,
    rx: u64,
    tx: u64,
}

struct LiveDump {
    iface_public: String,
    listen_port: String,
    peers: Vec<LivePeer>,
}

/// `wg show <iface> dump`:
///   line 1 (interface): private-key  public-key  listen-port  fwmark
///   line N (peer):      public-key  preshared-key  endpoint  allowed-ips
///                       latest-handshake  rx  tx  persistent-keepalive
fn parse_dump(text: &str) -> Option<LiveDump> {
    let mut lines = text.lines();
    let first = lines.next()?;
    let f: Vec<&str> = first.split('\t').collect();
    if f.len() < 3 {
        return None;
    }
    let mut dump = LiveDump {
        iface_public: f.get(1).unwrap_or(&"").to_string(),
        listen_port: f.get(2).unwrap_or(&"").to_string(),
        peers: Vec::new(),
    };
    for line in lines {
        let p: Vec<&str> = line.split('\t').collect();
        if p.len() < 7 {
            continue;
        }
        dump.peers.push(LivePeer {
            public_key: p[0].to_string(),
            endpoint: p[2].to_string(),
            allowed_ips: p[3].to_string(),
            latest_handshake: p[4].parse().unwrap_or(0),
            rx: p[5].parse().unwrap_or(0),
            tx: p[6].parse().unwrap_or(0),
        });
    }
    Some(dump)
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

fn fmt_handshake(epoch: u64) -> String {
    if epoch == 0 {
        return String::new();
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(epoch);
    let secs = now.saturating_sub(epoch);
    let ago = match secs {
        0 => "now".to_string(),
        1 => "1 second ago".to_string(),
        s if s < 60 => format!("{s} seconds ago"),
        s if s < 120 => "1 minute ago".to_string(),
        s if s < 3600 => format!("{} minutes ago", s / 60),
        s if s < 7200 => "1 hour ago".to_string(),
        s if s < 86400 => format!("{} hours ago", s / 3600),
        s => format!("{} days ago", s / 86400),
    };
    ago
}

fn fmt_bytes(b: u64) -> String {
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

fn fmt_transfer(rx: u64, tx: u64) -> String {
    if rx == 0 && tx == 0 {
        return String::new();
    }
    format!("{} received, {} sent", fmt_bytes(rx), fmt_bytes(tx))
}

// ---------------------------------------------------------------------------
// Config validation
// ---------------------------------------------------------------------------

/// A WireGuard key is base64 of 32 bytes → 43 chars + one '=' padding.
fn is_wg_key(s: &str) -> bool {
    let s = s.trim();
    s.len() == 44
        && s.ends_with('=')
        && s[..43]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

/// host:port, including bracketed IPv6 `[::1]:51820`.
fn is_endpoint(s: &str) -> bool {
    let s = s.trim();
    let (host, port) = match s.rsplit_once(':') {
        Some(hp) => hp,
        None => return false,
    };
    if host.is_empty() {
        return false;
    }
    match port.parse::<u32>() {
        Ok(p) => (1..=65535).contains(&p),
        Err(_) => false,
    }
}

/// A loose CIDR / address check: `10.0.0.2/24`, `::1/128`, or a bare IP.
fn looks_like_inet(s: &str) -> bool {
    let s = s.trim();
    let addr = s.split('/').next().unwrap_or("");
    !addr.is_empty()
        && (addr.contains('.') || addr.contains(':'))
        && addr
            .chars()
            .all(|c| c.is_ascii_hexdigit() || c == '.' || c == ':')
}

/// Validate a tunnel config the way the WireGuard tools would expect, so the
/// user gets a clear message before we ever hand it to `wg-quick`.
pub fn validate_config(text: &str) -> Result<(), String> {
    let has_iface = text
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case("[Interface]"));
    if !has_iface {
        return Err("Missing an [Interface] section.".into());
    }

    let cfg = parse_config(text);

    match cfg.private_key.as_deref() {
        None | Some("") => return Err("[Interface] is missing PrivateKey.".into()),
        Some(k) if !is_wg_key(k) => {
            return Err("PrivateKey is not a valid WireGuard key (expected 44-char base64).".into())
        }
        _ => {}
    }

    match cfg.address.as_deref() {
        None | Some("") => return Err("[Interface] is missing Address.".into()),
        Some(addrs) => {
            for a in addrs.split(',') {
                if !looks_like_inet(a) {
                    return Err(format!("Address “{}” is not a valid IP/CIDR.", a.trim()));
                }
            }
        }
    }

    if let Some(port) = cfg.listen_port.as_deref() {
        if !port.is_empty() && port.parse::<u32>().map(|p| p > 65535).unwrap_or(true) {
            return Err(format!("ListenPort “{port}” is not a valid port."));
        }
    }

    if cfg.peers.is_empty() {
        return Err("At least one [Peer] section is required.".into());
    }

    for (i, p) in cfg.peers.iter().enumerate() {
        let n = i + 1;
        if p.public_key.is_empty() {
            return Err(format!("Peer {n} is missing PublicKey."));
        }
        if !is_wg_key(&p.public_key) {
            return Err(format!("Peer {n} has an invalid PublicKey."));
        }
        if !p.preshared_key.is_empty() && !is_wg_key(&p.preshared_key) {
            return Err(format!("Peer {n} has an invalid PresharedKey."));
        }
        if p.allowed_ips.trim().is_empty() {
            return Err(format!("Peer {n} is missing AllowedIPs."));
        }
        for a in p.allowed_ips.split(',') {
            if !looks_like_inet(a) {
                return Err(format!("Peer {n}: AllowedIPs “{}” is not valid.", a.trim()));
            }
        }
        if !p.endpoint.is_empty() && !is_endpoint(&p.endpoint) {
            return Err(format!(
                "Peer {n}: Endpoint “{}” must be host:port.",
                p.endpoint
            ));
        }
        if !p.keepalive.is_empty() && p.keepalive.parse::<u32>().is_err() {
            return Err(format!(
                "Peer {n}: PersistentKeepalive “{}” must be a number.",
                p.keepalive
            ));
        }
    }

    Ok(())
}

/// Generate a fresh WireGuard keypair via `wg genkey` / `wg pubkey` (no root).
pub fn generate_keypair() -> Result<(String, String), String> {
    let genkey = Command::new("wg")
        .arg("genkey")
        .output()
        .map_err(|e| format!("wg genkey: {e}"))?;
    if !genkey.status.success() {
        return Err("wg genkey failed".into());
    }
    let private = String::from_utf8_lossy(&genkey.stdout).trim().to_string();
    let public = pubkey_of(&private).ok_or("wg pubkey failed")?;
    Ok((private, public))
}

/// Derive the public key from whatever `PrivateKey = …` is in a config (live,
/// for the editor's "Public key" line). Empty string if missing/invalid.
pub fn public_key_for_config(text: &str) -> String {
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if k.trim().eq_ignore_ascii_case("privatekey") {
                let key = v.trim();
                if is_wg_key(key) {
                    return pubkey_of(key).unwrap_or_default();
                }
                return String::new();
            }
        }
    }
    String::new()
}

/// All tunnel configs as (filename, contents), for export.
pub fn read_all_configs() -> Vec<(String, String)> {
    list_tunnels()
        .into_iter()
        .filter_map(|t| {
            read_config(&t.name)
                .ok()
                .map(|c| (format!("{}.conf", t.name), c))
        })
        .collect()
}

/// Write every tunnel config into a `.zip` at `dest`. Returns the count.
pub fn export_zip(dest: &std::path::Path) -> Result<usize, String> {
    let files = read_all_configs();
    if files.is_empty() {
        return Err("No tunnels to export.".into());
    }
    let f = std::fs::File::create(dest).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(f);
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o600);
    for (name, content) in &files {
        zip.start_file(name, opts).map_err(|e| e.to_string())?;
        zip.write_all(content.as_bytes())
            .map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(files.len())
}

/// Decode a QR-code image file into its text (a WireGuard `.conf`).
pub fn decode_qr(path: &std::path::Path) -> Result<String, String> {
    let img = image::open(path)
        .map_err(|e| format!("Couldn't open image: {e}"))?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let grids = prepared.detect_grids();
    let grid = grids.first().ok_or("No QR code found in the image.")?;
    let (_meta, content) = grid
        .decode()
        .map_err(|e| format!("QR decode failed: {e}"))?;
    Ok(content)
}

/// Whether `wg-quick@<name>` is enabled to start on boot.
pub fn is_autostart(name: &str) -> bool {
    helper(&["is-enabled", name], None)
        .map(|s| s.trim() == "enabled")
        .unwrap_or(false)
}

/// Enable/disable starting the tunnel on boot.
pub fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    helper(&[if on { "enable" } else { "disable" }, name], None).map(|_| ())
}

/// True if the config contains directives that `wg-quick` runs as **root** on
/// activation (`PostUp`/`PreUp`/`PostDown`/`PreDown`). Used to warn the user
/// before they save/activate a config from an untrusted source.
pub fn config_runs_scripts(text: &str) -> bool {
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('#') {
            continue;
        }
        if let Some((key, _)) = line.split_once('=') {
            let k = key.trim().to_ascii_lowercase();
            if matches!(k.as_str(), "postup" | "preup" | "postdown" | "predown") {
                return true;
            }
        }
    }
    false
}

/// Make a safe tunnel/interface name from an imported file name.
pub fn sanitize_name(file_stem: &str) -> String {
    let cleaned: String = file_stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches('.');
    let s = if trimmed.is_empty() {
        "tunnel"
    } else {
        trimmed
    };
    s.chars().take(15).collect()
}
