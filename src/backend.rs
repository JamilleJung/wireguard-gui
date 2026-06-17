//! Talks to WireGuard through the privileged `wg-helper` binary.
//!
//! Everything that needs root (reading /etc/wireguard, `wg show`, `wg-quick`)
//! goes through `helper()`, which runs the helper as:
//!   * nothing            — when we are already root
//!   * `sudo -n wg-helper`— the normal case (NOPASSWD sudoers drop-in)
//!   * `pkexec wg-helper` — fallback when sudo is not set up (prompts)

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{config, secrets, validation};

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

/// Whether to honour a `$WG_HELPER` override. Since the helper runs as root,
/// an attacker who can set this env var could otherwise get their own script run
/// with privilege (notably when the app itself is run as root). In **debug**
/// builds it is always honoured (dev convenience). In **release** builds it is
/// refused unless the operator opts in with `WG_ALLOW_UNSAFE_HELPER=1` *and* the
/// target is a safe file: an absolute path to a regular file owned by root and
/// not writable by group/other (so it can't be swapped under us).
fn wg_helper_override_allowed(p: &str) -> bool {
    if cfg!(debug_assertions) {
        return true;
    }
    if std::env::var("WG_ALLOW_UNSAFE_HELPER").as_deref() != Ok("1") {
        return false;
    }
    let path = std::path::Path::new(p);
    if !path.is_absolute() {
        return false;
    }
    match std::fs::metadata(path) {
        Ok(m) => {
            use std::os::unix::fs::MetadataExt;
            m.is_file() && m.uid() == 0 && (m.mode() & 0o022) == 0
        }
        Err(_) => false,
    }
}

/// Resolve the helper path: `$WG_HELPER` (when allowed), an installed location,
/// or the in-tree copy used during `cargo run`.
fn helper_path() -> &'static str {
    HELPER.get_or_init(|| {
        if let Ok(p) = std::env::var("WG_HELPER")
            && wg_helper_override_allowed(&p)
        {
            return p;
        }
        // Unsafe override in a release build — ignore it and fall back to the
        // trusted installed paths rather than running an attacker's script.
        let candidates = [
            "/usr/local/lib/wireguard-gui/wg-helper",
            "/usr/lib/wireguard-gui/wg-helper",
        ];
        for c in candidates {
            if PathBuf::from(c).exists() {
                return c.to_string();
            }
        }
        // A wg-helper sitting next to the binary — e.g. an extracted release
        // tarball that hasn't run install.sh. Tried only after the trusted
        // installed paths; under sudoers/polkit the grant is scoped to the
        // installed path, so this mainly helps the run-as-root / portable case.
        if let Some(adj) = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join("wg-helper")))
            && adj.is_file()
        {
            return adj.to_string_lossy().into_owned();
        }
        // dev fallback: a helper built from src/bin/wg-helper.rs.
        let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        for rel in ["target/debug/wg-helper", "target/release/wg-helper"] {
            let dev = manifest.join(rel);
            if dev.is_file() {
                return dev.to_string_lossy().into_owned();
            }
        }
        manifest
            .join("target/debug/wg-helper")
            .to_string_lossy()
            .into_owned()
    })
}

/// Turn a failed spawn into an actionable message. The common dead-end is a box
/// where the user isn't a sudoer and `pkexec` (polkit) isn't installed - "No
/// such file or directory" then refers to the missing `pkexec`, not the helper.
fn spawn_error(esc: Escalation, helper: &str, e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        return match esc {
            Escalation::Direct => {
                format!("helper not found at {helper} - run ./install.sh (or set it up) first")
            }
            Escalation::Sudo => "'sudo' not found on PATH".to_string(),
            Escalation::Pkexec => "can't gain root: no passwordless sudo, and pkexec (polkit) \
                 isn't installed. Re-run ./install.sh (it now sets up sudo for you), or start \
                 this as root."
                .to_string(),
        };
    }
    format!("spawn failed: {e}")
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

    let mut child = cmd.spawn().map_err(|e| spawn_error(esc, helper, &e))?;
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
    pub killswitch: bool,
    pub public_key: String,
    pub listen_port: String,
    pub addresses: String,
    pub dns: String,
    pub peers: Vec<Peer>,
    /// Live interface totals (summed across peers), for computing throughput.
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Seconds since the most recent handshake (None if inactive / never).
    pub handshake_age: Option<u64>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

pub fn list_tunnels() -> Vec<Tunnel> {
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
    unique_name_with(base, tunnel_exists)
}

fn unique_name_with<F>(base: &str, exists: F) -> String
where
    F: Fn(&str) -> bool,
{
    let base = sanitize_name(base);
    if !exists(&base) {
        return base;
    }
    for n in 2..1000 {
        let suffix = format!("-{n}");
        let keep = 15usize.saturating_sub(suffix.len());
        let candidate = format!("{}{}", base.chars().take(keep).collect::<String>(), suffix);
        if !exists(&candidate) {
            return candidate;
        }
    }
    base
}

pub fn read_config(name: &str) -> Result<String, String> {
    helper(&["read", name], None)
}

pub fn validate_tunnel_name(name: &str) -> Result<(), String> {
    validation::validate_tunnel_name(name)
}

pub fn save_config(name: &str, content: &str) -> Result<(), String> {
    helper(&["save", name], Some(content)).map(|_| ())
}

pub fn rename_config(old: &str, new: &str, content: &str) -> Result<(), String> {
    helper(&["rename", old, new], Some(content)).map(|_| ())
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
    match helper(&["log"], None) {
        Ok(s) if !s.trim().is_empty() => secrets::redact_config(&s),
        Ok(_) => "(no recent log entries)".to_string(),
        Err(e) => format!("Could not read the log: {e}"),
    }
}

/// Build the full detail view for a tunnel by merging its on-disk config with
/// the live `wg show <name> dump` output.
pub fn get_detail(name: &str) -> Detail {
    let cfg = read_config(name).unwrap_or_default();
    let parsed = config::parse_config(&cfg);
    let dump = helper(&["dump", name], None).unwrap_or_default();
    let live = parse_dump(&dump);

    // Active: use the active interfaces list (fast, reliable), fall back to dump.
    let active = helper(&["active"], None)
        .map(|out| out.split_whitespace().any(|iface| iface == name))
        .unwrap_or(!dump.trim().is_empty());

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

    // Live interface totals + most-recent handshake age, for throughput/health.
    let (rx_bytes, tx_bytes) = live
        .as_ref()
        .map(|l| {
            l.peers
                .iter()
                .fold((0u64, 0u64), |(r, t), p| (r + p.rx, t + p.tx))
        })
        .unwrap_or((0, 0));
    let handshake_age = live.as_ref().and_then(|l| {
        let latest = l
            .peers
            .iter()
            .map(|p| p.latest_handshake)
            .max()
            .unwrap_or(0);
        if latest == 0 {
            return None;
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(latest);
        Some(now.saturating_sub(latest))
    });

    Detail {
        name: name.to_string(),
        active,
        autostart: is_autostart(name),
        killswitch: is_killswitch(name),
        public_key,
        listen_port,
        addresses: parsed.address.unwrap_or_default(),
        dns: parsed.dns.unwrap_or_default(),
        peers,
        rx_bytes,
        tx_bytes,
        handshake_age,
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
// Live status parsing
// ---------------------------------------------------------------------------

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

    match secs {
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

pub fn validate_config(text: &str) -> Result<(), String> {
    config::validate_basic_wireguard_config(text)
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
    config::private_key(text)
        .as_deref()
        .filter(|key| config::is_wg_key(key))
        .and_then(pubkey_of)
        .unwrap_or_default()
}

/// Generate a fresh preshared key via `wg genpsk` (no root).
pub fn generate_psk() -> Result<String, String> {
    let out = Command::new("wg")
        .arg("genpsk")
        .output()
        .map_err(|e| format!("wg genpsk: {e}"))?;
    if !out.status.success() {
        return Err("wg genpsk failed".into());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Apply the saved config to a RUNNING tunnel without dropping sessions
/// (`wg syncconf`). Only wg-level fields take effect; Address/DNS/MTU/Table
/// changes still need a full reconnect.
pub fn sync_running(name: &str) -> Result<(), String> {
    helper(&["sync", name], None).map(|_| ())
}

/// The live running wg-level config (`wg showconf`).
pub fn running_config(name: &str) -> Result<String, String> {
    helper(&["showconf", name], None)
}

/// Save the live running state back to the `.conf` (`wg-quick save`).
pub fn persist_live(name: &str) -> Result<(), String> {
    helper(&["persist", name], None).map(|_| ())
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

/// Whether the helper-installed firewall kill switch is present for this tunnel.
pub fn is_killswitch(name: &str) -> bool {
    helper(&["killswitch-status", name], None)
        .map(|s| s.trim() == "enabled")
        .unwrap_or(false)
}

/// Enable/disable the helper-installed firewall kill switch. This is intentionally
/// not persistent: it uses standard firewall rules and no daemon.
pub fn set_killswitch(name: &str, on: bool) -> Result<(), String> {
    helper(
        &[
            if on {
                "killswitch-enable"
            } else {
                "killswitch-disable"
            },
            name,
        ],
        None,
    )
    .map(|_| ())
}

/// True if the config contains directives that `wg-quick` runs as **root** on
/// activation (`PostUp`/`PreUp`/`PostDown`/`PreDown`). Used to warn the user
/// before they save/activate a config from an untrusted source.
pub fn config_runs_scripts(text: &str) -> bool {
    secrets::config_runs_scripts(text)
}

/// Make a safe tunnel/interface name from an imported file name. The result
/// satisfies the helper's rule (starts with an alphanumeric, then
/// alphanumeric/_/-/., max 15 chars): truncate first, then trim the ends so a
/// cut can't re-introduce a trailing dot or a non-alphanumeric leading char.
pub fn sanitize_name(file_stem: &str) -> String {
    validation::sanitize_import_name(file_stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A syntactically valid 44-char base64 WireGuard key (43 chars + '=').
    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=";

    #[test]
    fn sanitize_name_rules() {
        assert_eq!(sanitize_name("home"), "home");
        assert_eq!(sanitize_name("home server"), "home_server");
        assert_eq!(sanitize_name("name.conf"), "name");
        assert_eq!(sanitize_name("@#$"), "tunnel");
        assert_eq!(sanitize_name("___abc"), "abc");
        assert_eq!(sanitize_name("a.b.c."), "a.b.c");
        assert_eq!(sanitize_name(""), "tunnel");
        let long = sanitize_name("averylongtunnelname1234567");
        assert!(long.chars().count() <= 15);
        for input in ["...x", "___y", "@@@9abc", "valid-name"] {
            let n = sanitize_name(input);
            assert!(
                n.chars().next().unwrap().is_ascii_alphanumeric(),
                "{input} -> {n}"
            );
        }
    }

    #[test]
    fn unique_name_deduplicates() {
        let name = unique_name_with("vpn config", |candidate| {
            matches!(candidate, "vpn_config" | "vpn_config-2")
        });
        assert_eq!(name, "vpn_config-3");
    }

    #[test]
    fn validate_config_allows_full_and_interface_only_configs() {
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/24, fd00::2/64\n\
             DNS = 1.1.1.1\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0, ::/0\n\
             Endpoint = vpn.example.com:51820\nPersistentKeepalive = 25\n"
        );
        assert!(validate_config(&cfg).is_ok());
        assert!(validate_config(&format!("[Interface]\nPrivateKey = {KEY}\n")).is_ok());
    }

    #[test]
    fn validate_rejects_missing_private_key() {
        assert!(validate_config("not a config").is_err());
        assert!(validate_config(&format!(
            "[Interface]\nAddress = 10.0.0.2/24\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        ))
        .is_err());
    }
}
