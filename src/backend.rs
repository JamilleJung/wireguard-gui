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

use crate::{config, doctor, secrets, validation};

#[derive(Clone, Copy, PartialEq)]
enum Escalation {
    Direct,
    Sudo,
    Pkexec,
}

static ESC: OnceLock<Escalation> = OnceLock::new();
static HELPER: OnceLock<Option<String>> = OnceLock::new();

/// Decide once how we gain privilege and where the helper lives.
pub fn init() {
    let esc = if unsafe { libc::geteuid() } == 0 {
        Escalation::Direct
    } else if helper_path().is_some_and(sudo_helper_available) {
        Escalation::Sudo
    } else {
        Escalation::Pkexec
    };
    let _ = ESC.set(esc);
}

fn sudo_helper_available(path: &str) -> bool {
    use std::os::unix::process::CommandExt;
    let mut command = Command::new("sudo");
    command
        .args(["-n", path, "list"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
        .spawn()
        .ok()
        .and_then(|child| {
            helper_output_timeout(child, None, std::time::Duration::from_secs(2)).ok()
        })
        .is_some_and(|out| out.status.success())
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
    trusted_helper(std::path::Path::new(p))
}

/// Resolve the helper path: `$WG_HELPER` (when allowed), an installed location,
/// or the in-tree copy used during `cargo run`.
pub fn helper_path() -> Option<&'static str> {
    HELPER
        .get_or_init(|| {
            if let Ok(path) = std::env::var("WG_HELPER")
                && wg_helper_override_allowed(&path)
            {
                return Some(path);
            }
            for path in [
                "/usr/local/lib/wireguard-gui/wg-helper",
                "/usr/lib/wireguard-gui/wg-helper",
            ] {
                if trusted_helper(std::path::Path::new(path)) {
                    return Some(path.to_string());
                }
            }
            if let Some(path) = std::env::current_exe()
                .ok()
                .and_then(|p| p.parent().map(|d| d.join("wg-helper")))
                && (trusted_helper(&path) || (cfg!(debug_assertions) && path.is_file()))
            {
                return Some(path.to_string_lossy().into_owned());
            }
            // Development fallbacks are never baked into release helper selection:
            // an old build directory must not become a root executable later.
            if cfg!(debug_assertions) {
                for rel in ["target/debug/wg-helper", "target/release/wg-helper"] {
                    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel);
                    if path.is_file() {
                        return Some(path.to_string_lossy().into_owned());
                    }
                }
            }
            None
        })
        .as_deref()
}

fn trusted_helper(path: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    if !path.is_absolute() {
        return false;
    }
    let Ok(canonical) = path.canonicalize() else {
        return false;
    };
    let Ok(meta) = canonical.metadata() else {
        return false;
    };
    if !meta.is_file() || meta.uid() != 0 || meta.mode() & 0o022 != 0 || meta.mode() & 0o111 == 0 {
        return false;
    }
    // A root-owned file in a user-writable directory can still be replaced.
    // Verify both the spelling's parents and the resolved target's parents.
    [path, canonical.as_path()].into_iter().all(|file| {
        file.ancestors().skip(1).all(|dir| {
            dir.metadata()
                .is_ok_and(|meta| meta.is_dir() && meta.uid() == 0 && meta.mode() & 0o022 == 0)
        })
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
    let helper = helper_path()
        .ok_or("No trusted executable wg-helper found; run ./install.sh to install it.")?;

    // Polling must not open authentication dialogs every refresh. pkcheck
    // without --allow-user-interaction only accepts an existing authorization;
    // explicit mutating actions still use pkexec's normal authentication flow.
    if esc == Escalation::Pkexec
        && read_only_helper_verb(args.first().copied().unwrap_or(""))
        && !polkit_authorized(helper)
    {
        return Err("Helper authorization is required; run ./install.sh to configure the scoped helper grant.".into());
    }

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

    use std::os::unix::process::CommandExt;
    cmd.process_group(0);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let child = cmd.spawn().map_err(|e| spawn_error(esc, helper, &e))?;
    let out = helper_output(child, stdin.map(str::to_owned))?;
    if !out.status.success() {
        let err = secrets::redact_config(&String::from_utf8_lossy(&out.stderr));
        return Err(format!("{} {}: {}", helper, args.join(" "), err.trim()));
    }
    String::from_utf8(out.stdout).map_err(|_| "wg-helper returned invalid UTF-8.".to_string())
}

const MAX_HELPER_OUTPUT: usize = 4 * 1024 * 1024;

fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<(), String> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(std::io::Error::last_os_error().to_string())
    } else {
        Ok(())
    }
}

fn drain_helper_pipe(
    pipe: &mut Option<impl std::io::Read>,
    bytes: &mut Vec<u8>,
) -> Result<(), String> {
    let mut chunk = [0; 8192];
    for _ in 0..16 {
        let Some(reader) = pipe else {
            return Ok(());
        };
        match reader.read(&mut chunk) {
            Ok(0) => {
                *pipe = None;
                return Ok(());
            }
            Ok(n) if bytes.len() + n <= MAX_HELPER_OUTPUT => bytes.extend_from_slice(&chunk[..n]),
            Ok(_) => return Err("Helper output exceeds the 4 MiB safety limit.".into()),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(())
}

fn helper_output(
    child: std::process::Child,
    input: Option<String>,
) -> Result<std::process::Output, String> {
    helper_output_timeout(child, input, std::time::Duration::from_secs(90))
}

fn helper_output_timeout(
    mut child: std::process::Child,
    input: Option<String>,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut stdin = child.stdin.take();
    let input = input.unwrap_or_default().into_bytes();
    let mut offset = 0;
    let mut failed_input = false;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let mut status = None;
    let started = std::time::Instant::now();
    let result = (|| {
        nonblocking(stdout.as_ref().ok_or("Helper stdout is unavailable.")?)?;
        nonblocking(stderr.as_ref().ok_or("Helper stderr is unavailable.")?)?;
        if let Some(pipe) = &stdin {
            nonblocking(pipe)?;
        }
        loop {
            drain_helper_pipe(&mut stdout, &mut out)?;
            drain_helper_pipe(&mut stderr, &mut err)?;
            if offset == input.len() {
                stdin = None;
            }
            if let Some(pipe) = &mut stdin {
                match pipe.write(&input[offset..]) {
                    Ok(0) => {
                        failed_input = true;
                        stdin = None;
                    }
                    Ok(n) => offset += n,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) => {}
                    Err(_) => {
                        failed_input = true;
                        stdin = None;
                    }
                }
            }
            // Keep the leader unreaped while any pipe can delay completion:
            // its PID then remains reserved until group cleanup.
            if status.is_none() && stdout.is_none() && stderr.is_none() && stdin.is_none() {
                status = child.try_wait().map_err(|e| e.to_string())?;
            }
            if status.is_some() && stdout.is_none() && stderr.is_none() {
                break;
            }
            if started.elapsed() >= timeout {
                return Err("Helper operation timed out.".into());
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        let status = status.ok_or("Missing helper exit status.")?;
        if status.success() && (failed_input || offset != input.len()) {
            return Err("Could not send the complete config to wg-helper.".into());
        }
        Ok(std::process::Output {
            status,
            stdout: out,
            stderr: err,
        })
    })();
    if result.is_err() && status.is_none() {
        // Each helper command starts in its own group. Close our pipes and
        // terminate what we can; do not wait forever on an escaped descendant
        // or a privileged process that the UI user cannot signal.
        stdin.take();
        stdout.take();
        stderr.take();
        unsafe {
            libc::kill(-(child.id() as i32), libc::SIGKILL);
        }
        let _ = child.kill();
        let deadline = std::time::Instant::now();
        while deadline.elapsed() < std::time::Duration::from_millis(200) {
            if child.try_wait().ok().flatten().is_some() {
                return result;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

fn read_only_helper_verb(verb: &str) -> bool {
    matches!(
        verb,
        "list"
            | "active"
            | "read"
            | "dump"
            | "is-enabled"
            | "showconf"
            | "log"
            | "killswitch-status"
    )
}

fn polkit_subject(stat: &str, pid: u32, uid: u32) -> Option<String> {
    // /proc stat field 2 (comm) can itself contain spaces and ')'. Field22 is
    // starttime; use pid,starttime,uid as required by pkcheck to avoid PID reuse.
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    if start.is_empty() || !start.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some(format!("{pid},{start},{uid}"))
}

fn polkit_authorized(helper: &str) -> bool {
    let Ok(stat) = std::fs::read_to_string("/proc/self/stat") else {
        return false;
    };
    let Some(subject) = polkit_subject(&stat, std::process::id(), unsafe { libc::geteuid() })
    else {
        return false;
    };
    let mut cmd = Command::new("pkcheck");
    cmd.args([
        "--action-id",
        "org.freedesktop.policykit.exec",
        "--process",
        &subject,
        "--detail",
        "program",
        helper,
        "--detail",
        "user",
        "root",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::null())
    .stderr(Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return false;
    };
    let start = std::time::Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if start.elapsed() < std::time::Duration::from_secs(3) => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
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

pub fn try_list_tunnels() -> Result<Vec<Tunnel>, String> {
    let names = helper(&["list"], None)?;
    let active: Vec<String> = helper(&["active"], None)?
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();
    Ok(names
        .lines()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(|n| Tunnel {
            name: n.to_string(),
            active: active.iter().any(|a| a == n),
        })
        .collect())
}

pub fn list_tunnels() -> Vec<Tunnel> {
    try_list_tunnels().unwrap_or_default()
}

/// A collision-free tunnel name based on `base`: returns `base`, else
/// `base-2`, `base-3`, … (kept within the 15-char interface-name limit).
pub fn unique_name(base: &str) -> Result<String, String> {
    let existing: std::collections::HashSet<_> =
        try_list_tunnels()?.into_iter().map(|t| t.name).collect();
    unique_name_with(base, |name| existing.contains(name))
}

fn unique_name_with<F>(base: &str, exists: F) -> Result<String, String>
where
    F: Fn(&str) -> bool,
{
    let base = sanitize_name(base);
    if !exists(&base) {
        return Ok(base);
    }
    for n in 2..=10000 {
        let suffix = format!("-{n}");
        let keep = 15usize.saturating_sub(suffix.len());
        let candidate = format!("{}{}", base.chars().take(keep).collect::<String>(), suffix);
        if !exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err("No unused tunnel name could be found; choose a different name.".into())
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

    let peers = merge_peers(parsed.peers, live.as_ref());
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (rx_bytes, tx_bytes, handshake_age) = live_totals(live.as_ref(), now);

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

// ---------------------------------------------------------------------------
// Connection diagnostics: a friendly "why isn't this connecting?" checklist.
// ---------------------------------------------------------------------------

fn diag_line(s: &mut String, mark: &str, label: &str, detail: &str) {
    s.push_str(&format!("  {mark} {label:<12} {detail}\n"));
}

fn endpoint_host(ep: &str) -> String {
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

/// Run a connection-health checklist for `name` and return a formatted report.
/// Safe on a background thread (pings / resolves / shells out).
pub fn diagnose_report(name: &str) -> String {
    let mut s = format!("Connection check for {name}\n\n");
    match doctor::clock_synced() {
        Some(true) => diag_line(&mut s, "[OK]", "Clock", "system clock is NTP-synced"),
        Some(false) => {
            diag_line(
                &mut s,
                "[X]",
                "Clock",
                "NOT synced — server rejects handshakes from a wrong clock (anti-replay)",
            );
            s.push_str("       -> sync time (enable NTP/chrony); if it just drifted, the server may need its peer reset\n");
        }
        None => diag_line(&mut s, "[?]", "Clock", "could not read clock-sync status"),
    }
    let d = get_detail(name);
    if !d.active {
        diag_line(
            &mut s,
            "[!]",
            "Tunnel",
            "not active — activate it first, then re-run",
        );
        return s;
    }
    diag_line(&mut s, "[OK]", "Tunnel", &format!("{name} is up"));
    match d.handshake_age {
        Some(a) if a < 180 => diag_line(
            &mut s,
            "[OK]",
            "Handshake",
            &format!("last handshake {a}s ago"),
        ),
        _ => {
            diag_line(
                &mut s,
                "[X]",
                "Handshake",
                "no recent handshake — up but NOT actually connected",
            );
            s.push_str("       -> likely (in order): clock not synced; endpoint unreachable; or server has not accepted this peer's key/preshared-key\n");
        }
    }
    match d
        .peers
        .iter()
        .map(|p| p.endpoint.clone())
        .find(|e| !e.is_empty())
    {
        Some(ep) => {
            let host = endpoint_host(&ep);
            match ping_host(&host) {
                Some(true) => {
                    diag_line(&mut s, "[OK]", "Endpoint", &format!("{host} is reachable"))
                }
                Some(false) => {
                    diag_line(
                        &mut s,
                        "[!]",
                        "Endpoint",
                        &format!("{host} did not answer ping"),
                    );
                    s.push_str("       -> server may block ICMP (handshake can still work) or the host is down\n");
                }
                None => diag_line(&mut s, "[?]", "Endpoint", "could not test the endpoint"),
            }
        }
        None => diag_line(&mut s, "[?]", "Endpoint", "no endpoint set in this config"),
    }
    if dns_ok() {
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

/// `wg pubkey` is pure crypto and needs no privilege.
fn pubkey_of(private_key: &str) -> Option<String> {
    if !config::is_wg_key(private_key) {
        return None;
    }
    let output = wg_output("pubkey", Some(format!("{private_key}\n"))).ok()?;
    output
        .status
        .success()
        .then(|| key_output(&output.stdout))
        .flatten()
}

fn wg_output(verb: &str, input: Option<String>) -> Result<std::process::Output, String> {
    use std::os::unix::process::CommandExt;
    let executable = if unsafe { libc::geteuid() } == 0 && !cfg!(debug_assertions) {
        ["/usr/sbin/wg", "/usr/bin/wg", "/sbin/wg", "/bin/wg"]
            .into_iter()
            .find(|path| trusted_helper(std::path::Path::new(path)))
            .ok_or("No trusted wg executable found.")?
    } else {
        "wg"
    };
    let mut command = Command::new(executable);
    command
        .arg(verb)
        .process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().map_err(|e| format!("wg {verb}: {e}"))?;
    helper_output_timeout(child, input, std::time::Duration::from_secs(10))
}

// ---------------------------------------------------------------------------
// Live status parsing
// ---------------------------------------------------------------------------

struct LivePeer {
    public_key: String,
    preshared: bool,
    endpoint: String,
    allowed_ips: String,
    keepalive: String,
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
            endpoint: if p[2] == "(none)" {
                String::new()
            } else {
                p[2].to_string()
            },
            allowed_ips: if p[3] == "(none)" {
                String::new()
            } else {
                p[3].to_string()
            },
            keepalive: p[7].to_string(),
            latest_handshake,
            rx,
            tx,
        });
    }
    Some(dump)
}

fn merge_peers(saved: Vec<config::ParsedPeer>, live: Option<&LiveDump>) -> Vec<Peer> {
    let mut peers = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in saved {
        seen.insert(p.public_key.clone());
        let lp = live.and_then(|dump| dump.peers.iter().find(|v| v.public_key == p.public_key));
        peers.push(Peer {
            public_key: p.public_key,
            preshared: lp
                .map(|p| p.preshared)
                .unwrap_or(!p.preshared_key.is_empty()),
            allowed_ips: lp.map(|p| p.allowed_ips.clone()).unwrap_or(p.allowed_ips),
            endpoint: lp
                .map(|p| p.endpoint.clone())
                .filter(|p| !p.is_empty())
                .unwrap_or(p.endpoint),
            keepalive: lp.map(|p| p.keepalive.clone()).unwrap_or(p.keepalive),
            latest_handshake: lp
                .map(|p| fmt_handshake(p.latest_handshake))
                .unwrap_or_default(),
            transfer: lp.map(|p| fmt_transfer(p.rx, p.tx)).unwrap_or_default(),
        });
    }
    if let Some(dump) = live {
        for p in &dump.peers {
            if seen.insert(p.public_key.clone()) {
                peers.push(Peer {
                    public_key: p.public_key.clone(),
                    preshared: p.preshared,
                    allowed_ips: p.allowed_ips.clone(),
                    endpoint: p.endpoint.clone(),
                    keepalive: p.keepalive.clone(),
                    latest_handshake: fmt_handshake(p.latest_handshake),
                    transfer: fmt_transfer(p.rx, p.tx),
                });
            }
        }
    }
    peers
}

fn live_totals(live: Option<&LiveDump>, now: u64) -> (u64, u64, Option<u64>) {
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
    (rx, tx, (latest != 0 && latest <= now).then(|| now - latest))
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
    if epoch > now {
        return "in the future (check system clock)".to_string();
    }
    let secs = now - epoch;

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

pub fn validate_config(text: &str) -> Result<(), String> {
    config::validate_basic_wireguard_config(text)
}

/// Generate a fresh WireGuard keypair via `wg genkey` / `wg pubkey` (no root).
pub fn generate_keypair() -> Result<(String, String), String> {
    let genkey = wg_output("genkey", None)?;
    if !genkey.status.success() {
        return Err("wg genkey failed".into());
    }
    let private =
        key_output(&genkey.stdout).ok_or("wg genkey returned an invalid WireGuard key")?;
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
    let out = wg_output("genpsk", None)?;
    if !out.status.success() {
        return Err("wg genpsk failed".into());
    }
    key_output(&out.stdout).ok_or("wg genpsk returned an invalid WireGuard key".into())
}

fn key_output(bytes: &[u8]) -> Option<String> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    config::is_wg_key(text).then(|| text.to_string())
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
pub fn read_all_configs() -> Result<Vec<(String, String)>, String> {
    collect_configs(try_list_tunnels()?.into_iter().map(|t| t.name), |name| {
        read_config(name)
    })
}

fn collect_configs(
    names: impl IntoIterator<Item = String>,
    mut read: impl FnMut(&str) -> Result<String, String>,
) -> Result<Vec<(String, String)>, String> {
    names
        .into_iter()
        .map(|name| {
            validate_tunnel_name(&name)?;
            let content = read(&name)?;
            Ok((format!("{name}.conf"), content))
        })
        .collect()
}

/// Export is all-or-error: unreadable configs never silently disappear from a
/// successful backup, and failed writes preserve an existing destination.
pub fn export_zip(dest: &std::path::Path) -> Result<usize, String> {
    let files = read_all_configs()?;
    write_zip_files(dest, &files, true)
}

fn write_zip_files(
    dest: &std::path::Path,
    files: &[(String, String)],
    replace: bool,
) -> Result<usize, String> {
    if files.is_empty() {
        return Err("No tunnels to export.".into());
    }
    atomic_private_write(dest, replace, |file| {
        let mut zip = zip::ZipWriter::new(file);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated)
            .unix_permissions(0o600);
        for (name, content) in files {
            zip.start_file(name, opts).map_err(|e| e.to_string())?;
            zip.write_all(content.as_bytes())
                .map_err(|e| e.to_string())?;
        }
        zip.finish().map_err(|e| e.to_string())?;
        Ok(files.len())
    })
}

/// Publish a completed 0600 sibling file. Replacing a hardlink changes only
/// the requested directory entry; a partial write never truncates another file.
fn atomic_private_write<T>(
    dest: &std::path::Path,
    replace: bool,
    write: impl FnOnce(&mut std::fs::File) -> Result<T, String>,
) -> Result<T, String> {
    use std::os::fd::FromRawFd;
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    if std::fs::symlink_metadata(dest).is_ok_and(|meta| !meta.is_file()) {
        return Err("Destination must be a regular file, not a directory or symlink.".into());
    }
    let dir = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let template = dir.join(".wireguard-export-XXXXXX");
    let mut name = std::ffi::CString::new(template.as_os_str().as_bytes())
        .map_err(|e| e.to_string())?
        .into_bytes_with_nul();
    let fd = unsafe { libc::mkstemp(name.as_mut_ptr().cast()) };
    if fd < 0 {
        return Err(format!(
            "create private file: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    name.pop();
    let tmp = PathBuf::from(std::ffi::OsString::from_vec(name));
    let result = (|| {
        let result = write(&mut file)?;
        file.sync_all().map_err(|e| e.to_string())?;
        if replace {
            std::fs::rename(&tmp, dest).map_err(|e| format!("publish {}: {e}", dest.display()))?;
        } else {
            // link is an atomic no-clobber publication; remove the temporary
            // link afterward, leaving the completed archive with one link.
            std::fs::hard_link(&tmp, dest)
                .map_err(|e| format!("publish {}: {e}", dest.display()))?;
        }
        if let Ok(parent) = std::fs::File::open(dir) {
            let _ = parent.sync_all();
        }
        Ok(result)
    })();
    let _ = std::fs::remove_file(tmp);
    result
}

// ===========================================================================
// Backup management — timestamped archives of every tunnel config, kept under
// the user's data dir (NOT /etc/wireguard, so no helper needed for storage).
// ===========================================================================

/// Metadata about one backup archive.
#[derive(Clone)]
pub struct BackupInfo {
    #[allow(dead_code)]
    pub path: PathBuf,
    pub name: String,
    pub when_secs: i64,
    pub size: u64,
    pub count: usize,
}

/// `$XDG_DATA_HOME` (or `~/.local/share`) `/wireguard/backups`, created 0700 —
/// the archives bundle private keys, so keep the whole tree private. The dir is
/// shared with wireguard-tui (same /etc/wireguard configs).
pub fn backup_dir() -> Result<PathBuf, String> {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")))
        .filter(|p| p.is_absolute())
        .ok_or("Can't locate an absolute home directory for backups.")?;
    ensure_backup_dir(&base)
}

fn ensure_backup_dir(base: &std::path::Path) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let dir = base.join("wireguard").join("backups");
    for path in [base.join("wireguard"), dir.clone()] {
        match std::fs::DirBuilder::new().mode(0o700).create(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.to_string()),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| e.to_string())?;
        let meta = file.metadata().map_err(|e| e.to_string())?;
        if meta.uid() != unsafe { libc::geteuid() } {
            return Err("Backup directory is not owned by the current user.".into());
        }
        file.set_permissions(std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    dir.canonicalize().map_err(|e| e.to_string())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn tm_of(secs: i64) -> Option<libc::tm> {
    let t = secs as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        None
    } else {
        Some(tm)
    }
}

/// Local "YYYY-MM-DD HH:MM:SS" for display.
pub fn fmt_time(secs: i64) -> String {
    match tm_of(secs) {
        Some(tm) => format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => "unknown".into(),
    }
}

fn stamp_compact(secs: i64) -> String {
    match tm_of(secs) {
        Some(tm) => format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday,
            tm.tm_hour,
            tm.tm_min,
            tm.tm_sec
        ),
        None => format!("{secs}"),
    }
}

/// Human-readable size, e.g. "12.3 KiB".
pub fn fmt_size(bytes: u64) -> String {
    fmt_bytes(bytes)
}

/// Create a timestamped backup of every tunnel config. Returns its metadata.
pub fn create_backup() -> Result<BackupInfo, String> {
    let dir = backup_dir()?;
    let secs = now_secs();
    static NEXT_BACKUP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_BACKUP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let name = format!(
        "wg-backup-{}-{}-{sequence}.zip",
        stamp_compact(secs),
        std::process::id()
    );
    let path = dir.join(&name);
    let count = write_zip_files(&path, &read_all_configs()?, false)?;
    let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
    Ok(BackupInfo {
        path,
        name,
        when_secs: secs,
        size,
        count,
    })
}

fn count_zip_entries(path: &std::path::Path) -> usize {
    open_backup(path)
        .ok()
        .and_then(|f| zip::ZipArchive::new(f).ok())
        .map(|z| z.len())
        .unwrap_or(0)
}

/// List existing backups, newest first.
pub fn list_backups() -> Vec<BackupInfo> {
    let dir = match backup_dir() {
        Ok(d) => d,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for ent in rd.flatten() {
        let path = ent.path();
        if path.extension().and_then(|e| e.to_str()) != Some("zip") {
            continue;
        }
        let Ok(file) = open_backup(&path) else {
            continue;
        };
        let Ok(md) = file.metadata() else { continue };
        let when_secs = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push(BackupInfo {
            name: ent.file_name().to_string_lossy().into_owned(),
            when_secs,
            size: md.len(),
            count: count_zip_entries(&path),
            path,
        });
    }
    out.sort_by_key(|b| std::cmp::Reverse(b.when_secs));
    out
}

const MAX_BACKUP_BYTES: u64 = 64 * 1024 * 1024;

/// Open relative to the already-open backup directory. Do not canonicalize a
/// file then reopen its spelling: a symlink swap could redirect that second IO.
fn open_backup(path: &std::path::Path) -> Result<std::fs::File, String> {
    let dir = backup_dir()?;
    open_backup_in(&dir, path)
}

fn open_backup_in(dir: &std::path::Path, path: &std::path::Path) -> Result<std::fs::File, String> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if path
        .parent()
        .ok_or("Backup path has no directory.")?
        .canonicalize()
        .map_err(|e| e.to_string())?
        != dir
    {
        return Err("Refusing a path outside the backup directory.".into());
    }
    let name = path.file_name().ok_or("Backup path has no filename.")?;
    let name = std::ffi::CString::new(name.as_bytes()).map_err(|e| e.to_string())?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| e.to_string())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(format!("open backup: {}", std::io::Error::last_os_error()));
    }
    let file = unsafe { std::fs::File::from_raw_fd(fd) };
    let meta = file.metadata().map_err(|e| e.to_string())?;
    if !meta.is_file()
        || meta.nlink() != 1
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.len() > MAX_BACKUP_BYTES
    {
        return Err(
            "Backup must be an owned regular file with one link and at most 64 MiB.".into(),
        );
    }
    Ok(file)
}

/// Restore every `.conf` inside a backup archive (overwriting tunnels of the
/// same name). Returns the number restored.
pub fn restore_backup(path: &std::path::Path) -> Result<usize, String> {
    let configs = backup_configs(open_backup(path)?)?;
    for (name, content) in &configs {
        save_config(name, content)?;
    }
    Ok(configs.len())
}

// Validate the whole archive before the first privileged write. Bound both
// declared and actual uncompressed sizes so a corrupt backup cannot exhaust
// memory or overwrite earlier tunnels before a later invalid entry is found.
fn backup_configs<R: std::io::Read + std::io::Seek>(
    mut reader: R,
) -> Result<Vec<(String, String)>, String> {
    use std::io::Read;
    const MAX_CONFIG_BYTES: u64 = 1024 * 1024;
    const MAX_TOTAL_BYTES: u64 = 16 * MAX_CONFIG_BYTES;
    let length = reader
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| e.to_string())?;
    if length > MAX_BACKUP_BYTES {
        return Err("Backup exceeds 64 MiB.".into());
    }
    reader
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|e| e.to_string())?;
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| e.to_string())?;
    if zip.has_duplicate_names() {
        return Err("Backup contains duplicate archive filenames.".into());
    }
    if zip.len() > 4096 {
        return Err("Backup contains too many archive entries.".into());
    }
    let mut configs = Vec::new();
    let mut names = std::collections::HashSet::new();
    let mut total = 0;
    for i in 0..zip.len() {
        let entry = zip.by_index(i).map_err(|e| e.to_string())?;
        if !entry.is_file() {
            continue;
        }
        let raw = entry.name().map_err(|e| e.to_string())?.into_owned();
        let Some(name) = raw.strip_suffix(".conf") else {
            continue;
        };
        validate_tunnel_name(name)?;
        if name.trim() != name || !names.insert(name.to_string()) {
            return Err(format!("Invalid or duplicate tunnel name in backup: {raw}"));
        }
        if entry.size() > MAX_CONFIG_BYTES || configs.len() >= 1024 {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        let mut content = String::new();
        entry
            .take(MAX_CONFIG_BYTES + 1)
            .read_to_string(&mut content)
            .map_err(|e| e.to_string())?;
        total += content.len() as u64;
        if content.len() as u64 > MAX_CONFIG_BYTES || total > MAX_TOTAL_BYTES {
            return Err("Backup exceeds the tunnel config size limit.".into());
        }
        crate::config::validate_basic_wireguard_config(&content)?;
        if config_runs_scripts(&content) {
            return Err(format!(
                "Script hooks are not allowed in backup tunnel {name}."
            ));
        }
        configs.push((name.to_string(), content));
    }
    if configs.is_empty() {
        return Err("The backup contained no tunnel configs.".into());
    }
    Ok(configs)
}

/// Delete a backup archive (only within the backup directory).
pub fn delete_backup(path: &std::path::Path) -> Result<(), String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::OpenOptionsExt;
    let _file = open_backup(path)?;
    let dir = backup_dir()?;
    let directory = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| e.to_string())?;
    let name = std::ffi::CString::new(
        path.file_name()
            .ok_or("Missing backup filename.")?
            .as_bytes(),
    )
    .map_err(|e| e.to_string())?;
    let result = unsafe { libc::unlinkat(directory.as_raw_fd(), name.as_ptr(), 0) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

fn write_private(dest: &std::path::Path, data: &[u8]) -> Result<(), String> {
    atomic_private_write(dest, true, |file| {
        file.write_all(data).map_err(|e| e.to_string())
    })
}

pub fn export_backup_to(src: &std::path::Path, dest: &std::path::Path) -> Result<(), String> {
    use std::io::Read;
    let mut data = Vec::new();
    open_backup(src)?
        .take(MAX_BACKUP_BYTES + 1)
        .read_to_end(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > MAX_BACKUP_BYTES {
        return Err("Backup exceeds 64 MiB.".into());
    }
    write_private(dest, &data)
}

pub fn save_text_to(dest: &std::path::Path, text: &str) -> Result<(), String> {
    write_private(dest, text.as_bytes())
}

/// Decode a QR-code image file into its text (a WireGuard `.conf`).
pub fn decode_qr(path: &std::path::Path) -> Result<String, String> {
    let file = std::fs::File::open(path).map_err(|e| format!("Couldn't open image: {e}"))?;
    if file.metadata().map_err(|e| e.to_string())?.len() > 32 * 1024 * 1024 {
        return Err("QR image exceeds the 32 MiB file limit.".into());
    }
    let mut reader = image::ImageReader::new(std::io::BufReader::new(file))
        .with_guessed_format()
        .map_err(|e| e.to_string())?;
    let mut limits = image::Limits::default();
    // Bounds cover a full-size screenshot while limiting both decoder buffers
    // and rqrr's additional per-pixel working memory.
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(4096);
    limits.max_alloc = Some(128 * 1024 * 1024);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| format!("Couldn't decode bounded QR image: {e}"))?
        .to_luma8();
    let mut prepared = rqrr::PreparedImage::prepare(img);
    let grids = prepared.detect_grids();
    if grids.is_empty() {
        return Err("No QR code found in the image.".into());
    }
    select_qr_config(grids.iter().map(|grid| {
        grid.decode()
            .map(|(_, content)| content)
            .map_err(|e| e.to_string())
    }))
}

fn select_qr_config(
    results: impl IntoIterator<Item = Result<String, String>>,
) -> Result<String, String> {
    let mut configs = Vec::new();
    for result in results {
        let Ok(content) = result else {
            continue;
        };
        if config::validate_basic_wireguard_config(&content).is_ok()
            && !config_runs_scripts(&content)
            && !configs.contains(&content)
        {
            configs.push(content);
            if configs.len() > 1 {
                return Err("Image contains multiple different WireGuard configs; use an image with one tunnel.".into());
            }
        }
    }
    configs
        .pop()
        .ok_or("No valid WireGuard config QR code found in the image.".into())
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
    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    #[test]
    fn generated_keys_must_be_valid_utf8_and_canonical_wireguard_keys() {
        assert_eq!(
            key_output(format!("{KEY}\n").as_bytes()),
            Some(KEY.to_string())
        );
        assert!(key_output(b"invalid").is_none());
        assert!(key_output(&[0xff]).is_none());
        assert!(key_output(b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=").is_none());
    }

    #[test]
    fn helper_capture_drains_both_streams_and_caps_memory() {
        let child = Command::new("sh")
            .args([
                "-c",
                "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let out = helper_output(child, None).unwrap();
        assert!(out.status.success());
        assert_eq!((out.stdout.len(), out.stderr.len()), (262144, 262144));
        let child = Command::new("sh")
            .args(["-c", "head -c 4194305 /dev/zero"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        assert!(helper_output(child, None).unwrap_err().contains("4 MiB"));
    }

    #[test]
    fn helper_escaped_pipe_fixture() {
        if std::env::var_os("WG_TEST_ESCAPE_PIPE").is_none() {
            return;
        }
        // Fork after exec: doing this in pre_exec would also inherit Rust's
        // exec-error pipe and make Command::spawn itself wait for the sleeper.
        unsafe {
            let pid = libc::fork();
            assert!(pid >= 0);
            if pid == 0 {
                libc::setsid();
                libc::sleep(2);
                libc::_exit(0);
            }
        }
    }

    #[test]
    fn helper_deadlines_cover_blocked_stdin_and_escaped_output_pipes() {
        use std::os::unix::process::CommandExt;
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30"]);
        cmd.process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        let error = helper_output_timeout(
            cmd.spawn().unwrap(),
            Some("x".repeat(1024 * 1024)),
            std::time::Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.contains("timed out"));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        let mut cmd = Command::new(std::env::current_exe().unwrap());
        cmd.args([
            "--exact",
            "backend::tests::helper_escaped_pipe_fixture",
            "--nocapture",
        ])
        .env("WG_TEST_ESCAPE_PIPE", "1");
        cmd.process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let started = std::time::Instant::now();
        assert!(
            helper_output_timeout(
                cmd.spawn().unwrap(),
                None,
                std::time::Duration::from_millis(100)
            )
            .unwrap_err()
            .contains("timed out")
        );
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    #[test]
    fn helper_discovery_requires_a_trusted_executable() {
        assert!(!trusted_helper(std::path::Path::new("relative-helper")));
        let dir = scratch_dir();
        let helper = dir.join("wg-helper");
        std::fs::write(&helper, b"untrusted file").unwrap();
        assert!(!trusted_helper(&helper));
        std::fs::remove_dir_all(dir).unwrap();
        assert!(trusted_helper(std::path::Path::new("/bin/sh")));
    }

    #[test]
    fn background_reads_never_request_interactive_authentication() {
        for verb in [
            "list",
            "active",
            "read",
            "dump",
            "is-enabled",
            "showconf",
            "log",
            "killswitch-status",
        ] {
            assert!(read_only_helper_verb(verb));
        }
        for verb in [
            "save",
            "up",
            "down",
            "delete",
            "rename",
            "persist",
            "killswitch-enable",
        ] {
            assert!(!read_only_helper_verb(verb));
        }
        let fields = (3..=22)
            .map(|field| if field == 22 { "123456" } else { "0" })
            .collect::<Vec<_>>()
            .join(" ");
        assert_eq!(
            polkit_subject(&format!("42 (name with ) spaces) {fields}"), 42, 1000),
            Some("42,123456,1000".into())
        );
        assert_eq!(polkit_subject("bad proc text", 42, 1000), None);
    }

    fn scratch_dir() -> PathBuf {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let template = std::env::temp_dir().join("wireguard-backend-test-XXXXXX");
        let mut bytes = std::ffi::CString::new(template.as_os_str().as_bytes())
            .unwrap()
            .into_bytes_with_nul();
        assert!(!unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null());
        bytes.pop();
        PathBuf::from(std::ffi::OsString::from_vec(bytes))
            .canonicalize()
            .unwrap()
    }

    #[test]
    fn atomic_exports_preserve_old_data_and_other_hardlinks_on_failure() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch_dir();
        let dest = dir.join("export.zip");
        let other = dir.join("other-file");
        std::fs::write(&dest, b"old archive").unwrap();
        std::fs::hard_link(&dest, &other).unwrap();
        let failed: Result<(), String> = atomic_private_write(&dest, true, |file| {
            file.write_all(b"partial secret").unwrap();
            Err("simulated full disk".into())
        });
        assert!(failed.is_err());
        assert_eq!(std::fs::read(&dest).unwrap(), b"old archive");
        write_private(&dest, b"new secret").unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"new secret");
        assert_eq!(std::fs::read(&other).unwrap(), b"old archive");
        assert_eq!(dest.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        assert!(
            atomic_private_write(&dest, false, |f| f
                .write_all(b"overwrite")
                .map_err(|e| e.to_string()))
            .is_err()
        );
        assert_eq!(std::fs::read(&dest).unwrap(), b"new secret");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn export_refuses_symlinks_and_never_silently_omits_failed_reads() {
        use std::os::unix::fs::symlink;
        let dir = scratch_dir();
        let real = dir.join("real");
        let link = dir.join("link");
        std::fs::write(&real, b"do not touch").unwrap();
        symlink(&real, &link).unwrap();
        assert!(write_private(&link, b"secret").is_err());
        assert_eq!(std::fs::read(&real).unwrap(), b"do not touch");
        let result = collect_configs(["wg0".to_string(), "wg1".to_string()], |name| {
            if name == "wg0" {
                Ok("valid content".into())
            } else {
                Err("permission denied".into())
            }
        });
        assert_eq!(result.unwrap_err(), "permission denied");
        assert!(unique_name_with("wg0", |_| true).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn backup_storage_rejects_symlink_directories_and_linked_archives() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let base = scratch_dir();
        let outside = scratch_dir();
        symlink(&outside, base.join("wireguard")).unwrap();
        assert!(ensure_backup_dir(&base).is_err());
        assert!(!outside.join("backups").exists());
        std::fs::remove_file(base.join("wireguard")).unwrap();
        let dir = ensure_backup_dir(&base).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_backup_dir(&base).unwrap();
        assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
        let real = dir.join("real.zip");
        let linked = dir.join("linked.zip");
        std::fs::write(&real, b"archive").unwrap();
        assert!(open_backup_in(&dir, &real).is_ok());
        symlink(&real, &linked).unwrap();
        assert!(open_backup_in(&dir, &linked).is_err());
        std::fs::remove_file(&linked).unwrap();
        std::fs::hard_link(&real, &linked).unwrap();
        assert!(open_backup_in(&dir, &real).is_err());
        std::fs::remove_dir_all(base).unwrap();
        std::fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn duplicate_central_directory_names_are_not_hidden_by_zip_indexing() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        let mut archive =
            backup_archive(&[("wg0.conf", &valid), ("wg1.conf", &valid)]).into_inner();
        for index in 0..archive.len().saturating_sub(8) {
            if &archive[index..index + 8] == b"wg1.conf" {
                archive[index + 2] = b'0';
            }
        }
        assert!(
            backup_configs(std::io::Cursor::new(archive))
                .unwrap_err()
                .contains("duplicate")
        );
    }

    #[test]
    fn oversized_zip_is_rejected_before_parsing_metadata() {
        let dir = scratch_dir();
        let path = dir.join("large.zip");
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(MAX_BACKUP_BYTES + 1).unwrap();
        drop(file);
        assert!(
            backup_configs(std::fs::File::open(path).unwrap())
                .unwrap_err()
                .contains("64 MiB")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

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
        let peers = merge_peers(vec![saved], Some(&dump));
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].allowed_ips, "10.0.0.0/24");
        assert_eq!(peers[0].keepalive, "off");
        assert_eq!(peers[0].endpoint, "saved:1");
        assert!(peers[1].preshared);
        assert_eq!(live_totals(Some(&dump), 300), (60, 80, Some(100)));
        assert_eq!(live_totals(Some(&dump), 150).2, None);
        let mut dump = dump;
        dump.peers[0].rx = u64::MAX;
        assert_eq!(live_totals(Some(&dump), 300).0, u64::MAX);
        assert!(parse_dump("invalid\tdump\t0").is_none());
    }

    #[test]
    fn qr_selection_skips_bad_grids_and_refuses_ambiguous_tunnels() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        assert_eq!(
            select_qr_config([
                Err("decode failed".into()),
                Ok("unrelated QR".into()),
                Ok(valid.clone())
            ])
            .unwrap(),
            valid
        );
        assert!(
            select_qr_config([
                Ok(valid.clone()),
                Ok(format!("{valid}Address = 10.0.0.1/24\n"))
            ])
            .is_err()
        );
        assert_eq!(
            select_qr_config([Ok(valid.clone()), Ok(valid.clone())]).unwrap(),
            valid
        );
    }

    #[test]
    fn qr_decoder_rejects_oversized_dimensions() {
        let dir = scratch_dir();
        let path = dir.join("wide.png");
        image::GrayImage::new(4097, 1).save(&path).unwrap();
        assert!(decode_qr(&path).unwrap_err().contains("bounded QR image"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn backup_archive(files: &[(&str, &str)]) -> std::io::Cursor<Vec<u8>> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        for (name, content) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        zip.finish().unwrap()
    }

    #[test]
    fn backup_restore_only_reads_tunnel_configs() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        let configs = backup_configs(backup_archive(&[
            ("README.txt", "not a config"),
            ("wg0.conf", &valid),
        ]))
        .unwrap();
        assert_eq!(configs, vec![("wg0".to_string(), valid)]);
        assert!(backup_configs(backup_archive(&[("notes.txt", "text")])).is_err());
    }

    #[test]
    fn backup_restore_preflights_names_and_all_content() {
        let valid = format!("[Interface]\nPrivateKey = {KEY}\n");
        for name in ["../wg0.conf", "nested/wg0.conf", "wg 0.conf", " wg0.conf"] {
            assert!(
                backup_configs(backup_archive(&[(name, &valid)])).is_err(),
                "{name}"
            );
        }
        assert!(
            backup_configs(backup_archive(&[
                ("wg0.conf", &valid),
                ("wg1.conf", "invalid config"),
            ]))
            .is_err()
        );
        let scripted = format!("{valid}PostUp = id\n");
        assert!(backup_configs(backup_archive(&[("wg0.conf", &scripted)])).is_err());
    }

    #[test]
    fn backup_restore_rejects_oversized_configs() {
        let oversized = "x".repeat(1024 * 1024 + 1);
        assert!(backup_configs(backup_archive(&[("wg0.conf", &oversized)])).is_err());
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
        assert_eq!(name.unwrap(), "vpn_config-3");
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
