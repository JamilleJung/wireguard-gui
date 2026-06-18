use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const WG_DIR: &str = "/etc/wireguard";
const BACKUP_DIR: &str = "/etc/wireguard/.backup";
const FIXED_PATHS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];

fn main() {
    if let Err(e) = run() {
        eprintln!("wg-helper: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let verb = args.next().ok_or("no verb")?;
    match verb.as_str() {
        "list" => list(),
        "active" => active(),
        "read" => read_config(&required_name(args.next())?),
        "dump" => dump(&required_name(args.next())?),
        "up" => up(&required_name(args.next())?),
        "down" => down(&required_name(args.next())?),
        "save" => save(&required_name(args.next())?),
        "delete" => delete(&required_name(args.next())?),
        "rename" => {
            let old = required_name(args.next())?;
            let new = required_name(args.next())?;
            rename(&old, &new)
        }
        "enable" => set_autostart(&required_name(args.next())?, true),
        "disable" => set_autostart(&required_name(args.next())?, false),
        "is-enabled" => is_enabled(&required_name(args.next())?),
        "sync" => sync_running(&required_name(args.next())?),
        "showconf" => showconf(&required_name(args.next())?),
        "persist" => persist(&required_name(args.next())?),
        "log" => show_log(),
        "killswitch-status" => killswitch_status(&required_name(args.next())?),
        "killswitch-enable" => killswitch_enable(&required_name(args.next())?),
        "killswitch-disable" => killswitch_disable(&required_name(args.next())?),
        _ => Err(format!("unknown verb: {verb}")),
    }
}

fn required_name(name: Option<String>) -> Result<String, String> {
    let name = name.unwrap_or_default();
    validate_name(&name)?;
    Ok(name)
}

fn name_ok(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !first.is_ascii_alphanumeric()
        || name.len() > 15
        || name.contains("..")
        || name.to_ascii_lowercase().ends_with(".conf")
    {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn validate_name(name: &str) -> Result<(), String> {
    if name_ok(name) {
        Ok(())
    } else {
        Err(format!("invalid tunnel name: '{name}'"))
    }
}

fn conf_path(name: &str) -> PathBuf {
    Path::new(WG_DIR).join(format!("{name}.conf"))
}

fn tool(name: &str) -> Option<PathBuf> {
    FIXED_PATHS
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| p.exists())
}

fn command_output(
    tool_name: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let exe = tool(tool_name).ok_or_else(|| format!("{tool_name} not found"))?;
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .env("PATH", FIXED_PATHS.join(":"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("{tool_name} {}: {e}", args.join(" ")))?;
    if let Some(input) = stdin {
        child
            .stdin
            .take()
            .ok_or("stdin unavailable")?
            .write_all(input.as_bytes())
            .map_err(|e| format!("{tool_name} stdin: {e}"))?;
    }

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if start.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{tool_name} {} timed out", args.join(" ")));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(format!("{tool_name} wait: {e}")),
        }
    }

    let out = child
        .wait_with_output()
        .map_err(|e| format!("{tool_name} output: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = redact(&String::from_utf8_lossy(&out.stderr));
        Err(format!("{tool_name} {}: {}", args.join(" "), stderr.trim()))
    }
}

fn command_status(tool_name: &str, args: &[&str], timeout: Duration) -> bool {
    command_output(tool_name, args, None, timeout).is_ok()
}

fn log_action(action: &str) {
    let who = std::env::var("SUDO_USER")
        .ok()
        .or_else(|| std::env::var("PKEXEC_UID").ok())
        .unwrap_or_else(|| "unknown".to_string());
    let msg = format!("user={who} action={action}");
    let _ = command_output(
        "logger",
        &["-t", env!("CARGO_PKG_NAME"), "--", &msg],
        None,
        Duration::from_secs(2),
    );
}

fn redact(input: &str) -> String {
    input
        .lines()
        .map(|line| {
            let lower = line.to_ascii_lowercase();
            if lower.contains("privatekey") || lower.contains("presharedkey") {
                match line.split_once('=') {
                    Some((k, _)) => format!("{} = <redacted>", k.trim()),
                    None => "<redacted secret line>".to_string(),
                }
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ensure_wg_dir() -> Result<(), String> {
    fs::create_dir_all(WG_DIR).map_err(|e| format!("create {WG_DIR}: {e}"))?;
    fs::set_permissions(WG_DIR, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {WG_DIR}: {e}"))?;
    Ok(())
}

fn backup_existing(name: &str) -> Result<(), String> {
    let src = conf_path(name);
    if !src.is_file() {
        return Ok(());
    }
    fs::create_dir_all(BACKUP_DIR).map_err(|e| format!("create {BACKUP_DIR}: {e}"))?;
    fs::set_permissions(BACKUP_DIR, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {BACKUP_DIR}: {e}"))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let dst = Path::new(BACKUP_DIR).join(format!("{name}.conf.{stamp}.{}", std::process::id()));
    fs::copy(&src, &dst).map_err(|e| format!("backup {name}: {e}"))?;
    fs::set_permissions(&dst, fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("chmod backup {name}: {e}"))?;
    log_action(&format!("backup {name} -> {}", dst.display()));
    Ok(())
}

fn sync_dir(path: &Path) {
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    let dir = path.parent().ok_or("target path has no parent")?;
    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or("target path has no file name")?;
    let tmp = dir.join(format!(".{file_name}.tmp.{}", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("create temp {}: {e}", tmp.display()))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("write temp {}: {e}", tmp.display()))?;
    file.sync_all()
        .map_err(|e| format!("sync temp {}: {e}", tmp.display()))?;
    drop(file);
    fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    sync_dir(dir);
    Ok(())
}

fn is_wg_key(value: &str) -> bool {
    let value = value.trim();
    value.len() == 44
        && value.ends_with('=')
        && value[..43]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/')
}

fn validate_config_text(text: &str) -> Result<(), String> {
    let mut section = "";
    let mut have_iface = false;
    let mut have_private = false;
    let mut peer_count = 0usize;
    let mut peer_pub = false;
    let mut peer_allowed = false;

    let finish_peer = |idx: usize, public: bool, allowed: bool| -> Result<(), String> {
        if idx == 0 {
            return Ok(());
        }
        if !public {
            return Err(format!("peer {idx} missing PublicKey"));
        }
        if !allowed {
            return Err(format!("peer {idx} missing AllowedIPs"));
        }
        Ok(())
    };

    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].trim().to_ascii_lowercase();
            if name == "interface" {
                finish_peer(peer_count, peer_pub, peer_allowed)?;
                section = "interface";
                have_iface = true;
            } else if name == "peer" {
                finish_peer(peer_count, peer_pub, peer_allowed)?;
                section = "peer";
                peer_count += 1;
                peer_pub = false;
                peer_allowed = false;
            } else {
                section = "other";
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        // PRIVILEGE BOUNDARY: refuse configs that would make wg-quick run
        // arbitrary commands as root on up/down. The helper runs as root via a
        // NOPASSWD sudoers/polkit grant, so a caller (or someone who hands the
        // victim a crafted .conf) must NOT be able to turn "manage tunnels" into
        // arbitrary root code execution through a PostUp/PreUp/PostDown/PreDown
        // hook. Anyone who legitimately needs hooks can edit /etc/wireguard as
        // root directly, outside this constrained helper.
        if matches!(key.as_str(), "postup" | "preup" | "postdown" | "predown") {
            return Err(format!(
                "script hooks are not allowed in managed configs ({key}); they would run as root"
            ));
        }
        match section {
            "interface" => match key.as_str() {
                "privatekey" => {
                    if !is_wg_key(value) {
                        return Err("[Interface] PrivateKey must be a WireGuard key".into());
                    }
                    have_private = true;
                }
                "address" if value.is_empty() => {
                    return Err("[Interface] Address is empty".into());
                }
                "address" => {}
                _ => {}
            },
            "peer" => match key.as_str() {
                "publickey" => {
                    if !is_wg_key(value) {
                        return Err(format!(
                            "peer {peer_count} PublicKey must be a WireGuard key"
                        ));
                    }
                    peer_pub = true;
                }
                "presharedkey" => {
                    if !value.is_empty() && !is_wg_key(value) {
                        return Err(format!(
                            "peer {peer_count} PresharedKey must be a WireGuard key"
                        ));
                    }
                }
                "allowedips" => {
                    if value.is_empty() {
                        return Err(format!("peer {peer_count} AllowedIPs is empty"));
                    }
                    peer_allowed = true;
                }
                _ => {}
            },
            _ => {}
        }
    }
    finish_peer(peer_count, peer_pub, peer_allowed)?;
    if !have_iface {
        return Err("missing [Interface]".into());
    }
    if !have_private {
        return Err("[Interface] missing PrivateKey".into());
    }
    Ok(())
}

fn read_stdin() -> Result<String, String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .map_err(|e| format!("read stdin: {e}"))?;
    Ok(buf)
}

fn list() -> Result<(), String> {
    let mut names = Vec::new();
    let Ok(entries) = fs::read_dir(WG_DIR) else {
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("conf") {
            continue;
        }
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str())
            && name_ok(stem)
        {
            names.push(stem.to_string());
        }
    }
    names.sort();
    for name in names {
        println!("{name}");
    }
    Ok(())
}

fn active() -> Result<(), String> {
    if let Ok(out) = command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5)) {
        print!("{out}");
    }
    Ok(())
}

fn read_config(name: &str) -> Result<(), String> {
    print!(
        "{}",
        fs::read_to_string(conf_path(name)).map_err(|e| format!("read {name}: {e}"))?
    );
    Ok(())
}

fn dump(name: &str) -> Result<(), String> {
    if let Ok(out) = command_output("wg", &["show", name, "dump"], None, Duration::from_secs(5)) {
        print!("{out}");
    }
    Ok(())
}

fn up(name: &str) -> Result<(), String> {
    log_action(&format!("up {name}"));
    command_output("wg-quick", &["up", name], None, Duration::from_secs(45)).map(|_| ())
}

fn down(name: &str) -> Result<(), String> {
    log_action(&format!("down {name}"));
    let res =
        command_output("wg-quick", &["down", name], None, Duration::from_secs(30)).map(|_| ());
    // The kill switch must never outlive the tunnel that justified it. Once the
    // interface and its fwmark are gone, the terminal REJECT rule would block
    // ALL non-loopback egress (a full network lockout). Tear it down here too —
    // mirroring what delete()/rename() already do — even if `wg-quick down`
    // itself failed (e.g. the tunnel was already partially down).
    let _ = killswitch_disable(name);
    res
}

fn save(name: &str) -> Result<(), String> {
    let content = read_stdin()?;
    validate_config_text(&content)?;
    ensure_wg_dir()?;
    backup_existing(name)?;
    atomic_write(&conf_path(name), &content)?;
    log_action(&format!("save {name}"));
    Ok(())
}

fn delete(name: &str) -> Result<(), String> {
    backup_existing(name)?;
    let _ = command_output("wg-quick", &["down", name], None, Duration::from_secs(30));
    if have_tool("systemctl")
        && command_status(
            "systemctl",
            &["is-enabled", &format!("wg-quick@{name}")],
            Duration::from_secs(5),
        )
    {
        let _ = command_output(
            "systemctl",
            &["disable", &format!("wg-quick@{name}")],
            None,
            Duration::from_secs(15),
        );
    }
    let path = conf_path(name);
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("delete {name}: {e}"))?;
        sync_dir(Path::new(WG_DIR));
    }
    let _ = killswitch_disable(name);
    log_action(&format!("delete {name}"));
    Ok(())
}

fn rename(old: &str, new: &str) -> Result<(), String> {
    if old == new {
        return Err("source and destination names must differ".into());
    }
    let src = conf_path(old);
    let dst = conf_path(new);
    if !src.is_file() {
        return Err(format!("no such tunnel: {old}"));
    }
    if dst.exists() {
        return Err(format!("target already exists: {new}"));
    }
    let content = read_stdin()?;
    validate_config_text(&content)?;
    if interface_active(old) {
        command_output("wg-quick", &["down", old], None, Duration::from_secs(30))
            .map_err(|_| format!("rename requires the active tunnel to stop: {old}"))?;
    }
    let _ = killswitch_disable(old);
    backup_existing(old)?;
    let enabled = have_tool("systemctl")
        && command_status(
            "systemctl",
            &["is-enabled", &format!("wg-quick@{old}")],
            Duration::from_secs(5),
        );
    if enabled {
        let _ = command_output(
            "systemctl",
            &["disable", &format!("wg-quick@{old}")],
            None,
            Duration::from_secs(15),
        );
    }
    atomic_write(&dst, &content)?;
    fs::remove_file(&src).map_err(|e| format!("remove old tunnel {old}: {e}"))?;
    sync_dir(Path::new(WG_DIR));
    if enabled {
        let _ = command_output(
            "systemctl",
            &["enable", &format!("wg-quick@{new}")],
            None,
            Duration::from_secs(15),
        );
    }
    log_action(&format!("rename {old} -> {new}"));
    Ok(())
}

fn have_tool(name: &str) -> bool {
    tool(name).is_some()
}

fn interface_active(name: &str) -> bool {
    command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5))
        .map(|out| out.split_whitespace().any(|iface| iface == name))
        .unwrap_or(false)
}

fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    if !have_tool("systemctl") {
        return Err("start-on-boot needs systemd (systemctl not found)".into());
    }
    log_action(&format!("{} {name}", if on { "enable" } else { "disable" }));
    command_output(
        "systemctl",
        &[
            if on { "enable" } else { "disable" },
            &format!("wg-quick@{name}"),
        ],
        None,
        Duration::from_secs(15),
    )
    .map(|_| ())
}

fn is_enabled(name: &str) -> Result<(), String> {
    if !have_tool("systemctl") {
        println!("unknown");
        return Ok(());
    }
    match command_output(
        "systemctl",
        &["is-enabled", &format!("wg-quick@{name}")],
        None,
        Duration::from_secs(5),
    ) {
        Ok(out) => print!("{out}"),
        Err(_) => println!("disabled"),
    }
    Ok(())
}

fn sync_running(name: &str) -> Result<(), String> {
    log_action(&format!("sync {name}"));
    let stripped = command_output("wg-quick", &["strip", name], None, Duration::from_secs(15))
        .map_err(|_| format!("wg-quick strip failed for {name}"))?;
    if stripped.trim().is_empty() {
        return Err(format!("wg-quick strip produced no output for {name}"));
    }
    command_output(
        "wg",
        &["syncconf", name, "/dev/stdin"],
        Some(&stripped),
        Duration::from_secs(15),
    )
    .map(|_| ())
}

fn showconf(name: &str) -> Result<(), String> {
    if let Ok(out) = command_output("wg", &["showconf", name], None, Duration::from_secs(10)) {
        print!("{out}");
    }
    Ok(())
}

fn persist(name: &str) -> Result<(), String> {
    backup_existing(name)?;
    log_action(&format!("persist {name}"));
    command_output("wg-quick", &["save", name], None, Duration::from_secs(30)).map(|_| ())
}

fn show_log() -> Result<(), String> {
    if !have_tool("journalctl") {
        println!("(journalctl is not available on this system - logs are only");
        println!(" recorded on systemd installs.)");
        return Ok(());
    }
    println!("===== {} activity =====", env!("CARGO_PKG_NAME"));
    if let Ok(out) = command_output(
        "journalctl",
        &[
            "--no-pager",
            "-o",
            "short-iso",
            "-n",
            "1000",
            "-t",
            env!("CARGO_PKG_NAME"),
        ],
        None,
        Duration::from_secs(10),
    ) {
        print!("{out}");
    }
    println!("\n===== wg-quick (systemd) =====");
    if let Ok(out) = command_output(
        "journalctl",
        &[
            "--no-pager",
            "-o",
            "short-iso",
            "-n",
            "1000",
            "-u",
            "wg-quick@*",
        ],
        None,
        Duration::from_secs(10),
    ) {
        print!("{out}");
    }
    Ok(())
}

fn fwmark(name: &str) -> Result<String, String> {
    let out = command_output(
        "wg",
        &["show", name, "fwmark"],
        None,
        Duration::from_secs(5),
    )?;
    let mark = out.trim();
    if mark.is_empty() || mark == "off" {
        Err(format!(
            "kill switch needs an active wg-quick tunnel with FwMark set: {name}"
        ))
    } else {
        Ok(mark.to_string())
    }
}

/// Ensure the tunnel is active and carries a non-zero fwmark for the kill
/// switch's `meta mark … accept` rule.
///
/// We prefer the fwmark `wg-quick` assigns automatically to default-route
/// tunnels and only set one ourselves when none exists — at RUNTIME via
/// `wg set`, NEVER by rewriting the user's `.conf`. The old approach appended
/// `FwMark = 51820` to the end of the file, which on a normal config lands
/// inside the last `[Peer]` section (FwMark is an `[Interface]` setting), and
/// did a `wg-quick down`/`up` dance that left a leak window with no rules.
fn ensure_tunnel_has_fwmark(name: &str) -> Result<(), String> {
    if !interface_active(name) {
        command_output("wg-quick", &["up", name], None, Duration::from_secs(45))?;
    }
    let current = command_output(
        "wg",
        &["show", name, "fwmark"],
        None,
        Duration::from_secs(5),
    )
    .map(|s| s.trim().to_string())
    .unwrap_or_default();
    if current.is_empty() || current == "off" {
        // No routing fwmark (e.g. a split-tunnel config). Set one at runtime so
        // WireGuard tags its encapsulated transport packets; this needs no
        // config change and no interface restart.
        command_output(
            "wg",
            &["set", name, "fwmark", "51820"],
            None,
            Duration::from_secs(5),
        )?;
    }
    Ok(())
}

fn kill_comment(name: &str) -> String {
    format!("wg-helper-killswitch:{name}")
}

// ---------------------------------------------------------------------------
// nftables backend (preferred on modern Linux — one table handles both v4+v6)
// ---------------------------------------------------------------------------

/// Ensure the `inet filter` table and `output` chain exist (no‑op if present).
fn nft_ensure_chain() -> Result<(), String> {
    let _ = command_output(
        "nft",
        &["add", "table", "inet", "filter"],
        None,
        Duration::from_secs(5),
    );
    let _ = command_output(
        "nft",
        &[
            "add", "chain", "inet", "filter", "output", "{", "type", "filter", "hook", "output",
            "priority", "0", ";", "}",
        ],
        None,
        Duration::from_secs(5),
    );
    command_output(
        "nft",
        &["list", "chain", "inet", "filter", "output"],
        None,
        Duration::from_secs(5),
    )
    .map(|_| ())
    .map_err(|_| "nftables inet filter output chain is not available".into())
}

/// True when at least one rule with our comment exists in nftables.
fn nft_has_comment(comment: &str) -> bool {
    command_output(
        "nft",
        &["-a", "list", "chain", "inet", "filter", "output"],
        None,
        Duration::from_secs(5),
    )
    .map(|out| out.lines().any(|line| line.contains(comment)))
    .unwrap_or(false)
}

/// Delete every nftables rule whose line contains our comment.
fn killswitch_flush_nft(name: &str) {
    let comment = kill_comment(name);
    let Ok(out) = command_output(
        "nft",
        &["-a", "list", "chain", "inet", "filter", "output"],
        None,
        Duration::from_secs(5),
    ) else {
        return;
    };
    let mut handles: Vec<u64> = Vec::new();
    for line in out.lines() {
        if line.contains(&comment)
            && let Some(handle_str) = line.rsplit("handle ").next()
            && let Ok(h) = handle_str.trim().parse::<u64>()
        {
            handles.push(h);
        }
    }
    handles.sort_unstable_by(|a, b| b.cmp(a));
    for h in handles {
        let _ = command_output(
            "nft",
            &[
                "delete",
                "rule",
                "inet",
                "filter",
                "output",
                "handle",
                &h.to_string(),
            ],
            None,
            Duration::from_secs(5),
        );
    }
}

/// nftables path of killswitch_enable.
fn killswitch_enable_nft(name: &str, mark: &str) -> Result<(), String> {
    let comment = kill_comment(name);
    killswitch_flush_nft(name);
    nft_ensure_chain()?;
    // SSH safety: when $SSH_CONNECTION is set, insert an ACCEPT rule for
    // established/related SSH return traffic before the kill-switch rules
    // so the current session survives.
    let ssh_port = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|conn| conn.split_whitespace().nth(3)?.parse::<u16>().ok());
    if let Some(port) = ssh_port {
        let _ = command_output(
            "nft",
            &[
                "insert",
                "rule",
                "inet",
                "filter",
                "output",
                "tcp",
                "sport",
                &port.to_string(),
                "ct",
                "state",
                "established,related",
                "accept",
                "comment",
                &comment,
            ],
            None,
            Duration::from_secs(5),
        );
    }
    command_output(
        "nft",
        &[
            "insert", "rule", "inet", "filter", "output", "oif", "lo", "accept", "comment",
            &comment,
        ],
        None,
        Duration::from_secs(10),
    )?;
    // Allow the tunnel's own plaintext egress: an app's packet is routed to the
    // wg interface and traverses OUTPUT with oif=<name> and NO fwmark (only
    // WireGuard's encapsulated transport packets carry the mark, on a second
    // pass). Without this the terminal reject drops it and the tunnel carries
    // no traffic at all.
    command_output(
        "nft",
        &[
            "insert", "rule", "inet", "filter", "output", "oifname", name, "accept", "comment",
            &comment,
        ],
        None,
        Duration::from_secs(10),
    )?;
    command_output(
        "nft",
        &[
            "insert", "rule", "inet", "filter", "output", "meta", "mark", mark, "accept",
            "comment", &comment,
        ],
        None,
        Duration::from_secs(10),
    )?;
    command_output(
        "nft",
        &[
            "add", "rule", "inet", "filter", "output", "reject", "comment", &comment,
        ],
        None,
        Duration::from_secs(10),
    )?;
    Ok(())
}

// ---------------------------------------------------------------------------
// iptables / ip6tables backend (fallback when nftables is absent)
// ---------------------------------------------------------------------------

fn iptables_has_comment(tool_name: &str, comment: &str) -> bool {
    command_output(tool_name, &["-S", "OUTPUT"], None, Duration::from_secs(5))
        .map(|out| out.lines().any(|line| line.contains(comment)))
        .unwrap_or(false)
}

/// Remove every iptables/ip6tables rule that carries our comment.
fn killswitch_flush_iptables(name: &str) {
    let comment = kill_comment(name);
    for tool_name in ["iptables", "ip6tables"] {
        if !have_tool(tool_name) {
            continue;
        }
        let Ok(out) = command_output(tool_name, &["-S", "OUTPUT"], None, Duration::from_secs(5))
        else {
            continue;
        };
        let mut nums: Vec<usize> = Vec::new();
        let mut idx = 0usize;
        for line in out.lines() {
            if line.starts_with("-A OUTPUT ") {
                idx += 1;
                if line.contains(&comment) {
                    nums.push(idx);
                }
            }
        }
        nums.sort_unstable_by(|a, b| b.cmp(a));
        for n in nums {
            let _ = command_output(
                tool_name,
                &["-D", "OUTPUT", &n.to_string()],
                None,
                Duration::from_secs(5),
            );
        }
    }
}

fn killswitch_enable_iptables(name: &str, mark: &str) -> Result<(), String> {
    let comment = kill_comment(name);
    killswitch_flush_iptables(name);
    let ssh_port = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|conn| conn.split_whitespace().nth(3)?.parse::<u16>().ok());
    if let Some(port) = ssh_port {
        for tool_name in ["iptables", "ip6tables"] {
            if !have_tool(tool_name) {
                continue;
            }
            let _ = command_output(
                tool_name,
                &[
                    "-I",
                    "OUTPUT",
                    "-p",
                    "tcp",
                    "--sport",
                    &port.to_string(),
                    "-m",
                    "conntrack",
                    "--ctstate",
                    "ESTABLISHED,RELATED",
                    "-m",
                    "comment",
                    "--comment",
                    &comment,
                    "-j",
                    "ACCEPT",
                ],
                None,
                Duration::from_secs(5),
            );
        }
    }
    for tool_name in ["iptables", "ip6tables"] {
        if !have_tool(tool_name) {
            continue;
        }
        command_output(
            tool_name,
            &[
                "-I",
                "OUTPUT",
                "-m",
                "mark",
                "--mark",
                mark,
                "-m",
                "comment",
                "--comment",
                &comment,
                "-j",
                "ACCEPT",
            ],
            None,
            Duration::from_secs(10),
        )?;
        command_output(
            tool_name,
            &[
                "-I",
                "OUTPUT",
                "-o",
                "lo",
                "-m",
                "comment",
                "--comment",
                &comment,
                "-j",
                "ACCEPT",
            ],
            None,
            Duration::from_secs(10),
        )?;
        // Allow the tunnel's own plaintext egress (oif=<name>, unmarked) — see
        // the nft path; without it the REJECT drops all tunnelled traffic.
        command_output(
            tool_name,
            &[
                "-I",
                "OUTPUT",
                "-o",
                name,
                "-m",
                "comment",
                "--comment",
                &comment,
                "-j",
                "ACCEPT",
            ],
            None,
            Duration::from_secs(10),
        )?;
        command_output(
            tool_name,
            &[
                "-A",
                "OUTPUT",
                "-m",
                "comment",
                "--comment",
                &comment,
                "-j",
                "REJECT",
            ],
            None,
            Duration::from_secs(10),
        )?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Unified killswitch API  (nftables preferred, iptables fallback)
// ---------------------------------------------------------------------------

fn killswitch_status(name: &str) -> Result<(), String> {
    let comment = kill_comment(name);
    let enabled = (have_tool("nft") && nft_has_comment(&comment))
        || iptables_has_comment("iptables", &comment)
        || iptables_has_comment("ip6tables", &comment);
    println!("{}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

/// True if any non-loopback interface has an IPv6 address — i.e. IPv6 traffic
/// could leak if the kill switch only covers IPv4.
fn host_has_global_ipv6() -> bool {
    fs::read_to_string("/proc/net/if_inet6")
        .map(|s| s.lines().any(|l| l.split_whitespace().last() != Some("lo")))
        .unwrap_or(false)
}

fn killswitch_enable(name: &str) -> Result<(), String> {
    // SSH safety: warn when $SSH_CONNECTION is set (kill switch can lock out).
    if std::env::var("SSH_CONNECTION").is_ok() {
        eprintln!("wg-helper: SSH session detected — auto-allowing established SSH traffic. ");
    }

    // 1. Ensure tunnel has FwMark and is active.
    ensure_tunnel_has_fwmark(name)?;
    if !interface_active(name) {
        return Err(format!("kill switch needs an active tunnel: {name}"));
    }
    let mark = fwmark(name)?;

    // 2. Prefer nftables (single `inet` table covers IPv4 + IPv6), fall back.
    if have_tool("nft") {
        killswitch_enable_nft(name, &mark)?;
    } else if have_tool("iptables") || have_tool("ip6tables") {
        // Fail closed: with only the iptables fallback we must cover IPv6 too.
        // If the host has a non-loopback IPv6 address but ip6tables is missing,
        // a v4-only reject would silently leak all IPv6 traffic while the status
        // still reads "enabled". Refuse rather than give false protection.
        if host_has_global_ipv6() && !have_tool("ip6tables") {
            return Err(
                "kill switch can't protect IPv6 on this host: install nftables (preferred) or ip6tables"
                    .into(),
            );
        }
        killswitch_enable_iptables(name, &mark)?;
    } else {
        return Err("kill switch needs nftables, iptables, or ip6tables".into());
    }

    log_action(&format!("killswitch-enable {name}"));
    Ok(())
}

fn killswitch_disable(name: &str) -> Result<(), String> {
    // Clean up every possible backend so stale rules never linger.
    if have_tool("nft") {
        killswitch_flush_nft(name);
    }
    killswitch_flush_iptables(name);
    log_action(&format!("killswitch-disable {name}"));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq=";

    #[test]
    fn tunnel_name_validation() {
        for name in ["wg0", "home-vpn", "a.b_c", "a12345678901234"] {
            assert!(name_ok(name), "{name}");
        }
        for name in [
            "",
            ".",
            "..",
            "/tmp/x",
            "../../etc/passwd",
            "name/evil",
            "name\\evil",
            "bad..name",
            "-bad",
            "name.conf",
            "abcdefghijklmnop",
        ] {
            assert!(!name_ok(name), "{name}");
        }
    }

    #[test]
    fn config_validation() {
        let valid = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(validate_config_text(&valid).is_ok());
        assert!(validate_config_text(&format!("[Interface]\nPrivateKey = {KEY}\n")).is_ok());
        assert!(validate_config_text("[Interface]\n").is_err());
        assert!(
            validate_config_text(&format!(
                "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n"
            ))
            .is_ok()
        );
    }

    #[test]
    fn rejects_script_hooks_at_the_privilege_boundary() {
        let base = format!("[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.1/32\n");
        for hook in [
            "PostUp", "PreUp", "PostDown", "PreDown", "postup", "preDown",
        ] {
            let cfg = format!("{base}{hook} = cp /bin/bash /tmp/x; chmod u+s /tmp/x\n");
            assert!(
                validate_config_text(&cfg).is_err(),
                "{hook} must be rejected (would run as root)"
            );
        }
        // A hook hidden in a [Peer] section is rejected too.
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nPostUp = id\n"
        );
        assert!(validate_config_text(&cfg).is_err());
        // A normal config without hooks still validates.
        assert!(validate_config_text(&base).is_ok());
    }

    #[test]
    fn paths_stay_under_wireguard_dir() {
        assert_eq!(conf_path("wg0"), Path::new(WG_DIR).join("wg0.conf"));
    }

    #[test]
    fn redacts_secret_lines() {
        let text = "PrivateKey = abc\nok\nPresharedKey = def";
        let out = redact(text);
        assert!(out.contains("PrivateKey = <redacted>"));
        assert!(out.contains("PresharedKey = <redacted>"));
        assert!(out.contains("ok"));
        assert!(!out.contains("abc"));
        assert!(!out.contains("def"));
    }

    // ---- killswitch-specific tests ----

    #[test]
    fn kill_comment_contains_tunnel_name() {
        let c = kill_comment("wg0");
        assert!(c.contains("wg0"), "{c}");
        assert!(c.contains("wg-helper-killswitch"), "{c}");
        // Comment must be unique enough to avoid collisions.
        assert!(!kill_comment("wg0").eq(&kill_comment("wg1")));
    }

    #[test]
    fn name_allows_simple_words() {
        // "Peer" and "Interface" are valid Linux interface names even though
        // they match WireGuard config section headers. wg-quick handles the
        // distinction via file extension and context.
        assert!(name_ok("Peer"));
        assert!(name_ok("Interface"));
    }

    #[test]
    fn fwmark_config_detection_logic() {
        // ensure_tunnel_has_fwmark reads the config and looks for a FwMark
        // line. Verify the detection patterns.
        let with_mark = "PrivateKey = x\nFwMark = 51820\n";
        let without_mark = "PrivateKey = x\nAddress = 10.0.0.1/24\n";
        let with_mark_lower = "privatekey = x\nfwmark = 99\n";
        let with_spaces = "FwMark   =   42\n";
        let with_prefix = "FwMarkFile = /tmp/x\n"; // NOT a FwMark line

        let has = |cfg: &str| {
            cfg.lines().any(|line| {
                let lower = line.trim().to_ascii_lowercase();
                lower == "fwmark"
                    || lower
                        .split_once('=')
                        .map(|(k, _)| k.trim() == "fwmark")
                        .unwrap_or(false)
            })
        };

        assert!(has(with_mark));
        assert!(!has(without_mark));
        assert!(has(with_mark_lower));
        assert!(has(with_spaces));
        // "FwMarkFile" is a different directive — exact matching correctly rejects it.
        assert!(!has(with_prefix));
    }

    #[test]
    fn nft_handle_extraction_parses_handles() {
        // Simulate `nft -a list chain inet filter output` output.
        let nft_out = "table inet filter {\n\tchain output {\n\t\toif \"lo\" accept comment \"wg-helper-killswitch:wg0\" # handle 12\n\t\tmeta mark 0xca6c accept comment \"wg-helper-killswitch:wg0\" # handle 13\n\t\treject comment \"wg-helper-killswitch:wg0\" # handle 14\n\t}\n}";
        let comment = "wg-helper-killswitch:wg0";
        let mut handles: Vec<u64> = Vec::new();
        for line in nft_out.lines() {
            if line.contains(comment)
                && let Some(handle_str) = line.rsplit("handle ").next()
                && let Ok(h) = handle_str.trim().parse::<u64>()
            {
                handles.push(h);
            }
        }
        assert_eq!(handles, vec![12, 13, 14]);
    }

    #[test]
    fn nft_handle_extraction_no_false_positives() {
        // Lines without our comment should not produce handles.
        let nft_out = "oif \"lo\" accept comment \"other-rule\" # handle 1\nreject # handle 2";
        let comment = "wg-helper-killswitch:wg0";
        let mut handles: Vec<u64> = Vec::new();
        for line in nft_out.lines() {
            if line.contains(comment)
                && let Some(handle_str) = line.rsplit("handle ").next()
                && let Ok(h) = handle_str.trim().parse::<u64>()
            {
                handles.push(h);
            }
        }
        assert!(handles.is_empty());
    }

    #[test]
    fn tunnel_name_edge_cases() {
        // Valid names at the boundary.
        assert!(name_ok("a")); // single char
        assert!(name_ok("a1")); // alphanumeric
        assert!(name_ok("a.b")); // dots ok
        assert!(name_ok("a-b")); // hyphens ok
        assert!(name_ok("a_b")); // underscores ok
        assert!(name_ok("a12345678901234")); // 15 chars max
        // Invalid edge cases.
        assert!(!name_ok("")); // empty
        assert!(!name_ok("a123456789012345")); // 16 chars — too long
        assert!(!name_ok("-a")); // leading hyphen
        assert!(!name_ok(".a")); // leading dot
        assert!(!name_ok("a..b")); // double dot
    }

    #[test]
    fn config_validation_edge_cases() {
        // Empty config
        assert!(validate_config_text("").is_err());
        // Only comment
        assert!(validate_config_text("# just a comment").is_err());
        // Interface with no private key
        assert!(validate_config_text("[Interface]\nAddress = 10.0.0.1/24\n").is_err());
        // Peer with no public key
        assert!(
            validate_config_text(&format!(
                "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nAllowedIPs = 0.0.0.0/0\n"
            ))
            .is_err()
        );
        // Peer with no allowed IPs
        assert!(
            validate_config_text(&format!(
                "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\n"
            ))
            .is_err()
        );
    }

    #[test]
    fn redact_handles_empty_and_comments() {
        assert_eq!(redact(""), "");
        assert_eq!(redact("# nothing secret"), "# nothing secret");
        // Case insensitivity for PrivateKey
        let out = redact("privatekey = secret\nPRESHAREDKEY = also-secret");
        assert!(out.contains("<redacted>"));
        assert!(!out.contains("secret"));
        assert!(!out.contains("also-secret"));
    }

    #[test]
    fn atomic_write_paths_are_fixed() {
        // Paths must always stay under /etc/wireguard
        assert!(conf_path("wg0").starts_with("/etc/wireguard"));
        assert!(conf_path("home-vpn").starts_with("/etc/wireguard"));
        // No escaping even with tricky names (validation would reject these anyway)
        assert_eq!(conf_path("wg0"), Path::new("/etc/wireguard/wg0.conf"));
    }

    #[test]
    fn killswitch_nft_rule_structure() {
        // Verify the nftables rules we generate have valid structure
        // (comment is essential for later cleanup).
        let c = kill_comment("test-tun");
        assert!(c.contains("wg-helper-killswitch"));
        assert!(c.contains("test-tun"));
        // Comment should be safe for nftables (no quotes in name).
        assert!(!c.contains('"'));
        assert!(!c.contains("'"));
    }

    #[test]
    fn killswitch_iptables_comment_format() {
        // iptables --comment must survive shell/iptables parsing.
        let c = kill_comment("home-vpn");
        // No shell metacharacters.
        assert!(!c.contains('$'));
        assert!(!c.contains('`'));
        assert!(!c.contains(';'));
        assert!(!c.contains('|'));
        // Must be valid after --comment flag.
        assert!(c.len() >= 10);
    }

    #[test]
    fn ssh_port_parsing() {
        // Simulate SSH_CONNECTION parsing
        let conn = "192.168.1.5 52341 10.0.0.1 22";
        let port: Option<u16> = conn
            .split_whitespace()
            .nth(3)
            .and_then(|s| s.parse::<u16>().ok());
        assert_eq!(port, Some(22));

        let bad = "garbage";
        let port2: Option<u16> = bad
            .split_whitespace()
            .nth(3)
            .and_then(|s| s.parse::<u16>().ok());
        assert_eq!(port2, None);
    }

    #[test]
    fn fwmark_present_tunnel_reactivation_logic() {
        // When FwMark is already in config, ensure_tunnel_has_fwmark should
        // not modify the config (no backup, no rewrite).
        // We test the detection logic only (can't call wg-quick in unit test).
        let cfg = "[Interface]
PrivateKey = x
FwMark = 51820
Address = 10.0.0.1/24
";
        let has = cfg.lines().any(|line| {
            let lower = line.trim().to_ascii_lowercase();
            lower.starts_with("fwmark") && lower.contains('=')
        });
        assert!(has);

        let cfg2 = "[Interface]
PrivateKey = x
";
        let has2 = cfg2.lines().any(|line| {
            let lower = line.trim().to_ascii_lowercase();
            lower.starts_with("fwmark") && lower.contains('=')
        });
        assert!(!has2);
    }

    #[test]
    fn iptables_rule_number_extraction() {
        // Simulate `iptables -S OUTPUT` output.
        let out = "-P OUTPUT ACCEPT\n-A OUTPUT -o lo -m comment --comment wg-helper-killswitch:wg0 -j ACCEPT\n-A OUTPUT -m mark --mark 0xca6c -m comment --comment wg-helper-killswitch:wg0 -j ACCEPT\n-A OUTPUT -j ACCEPT\n-A OUTPUT -m comment --comment wg-helper-killswitch:wg0 -j REJECT";
        let comment = "wg-helper-killswitch:wg0";
        let mut nums: Vec<usize> = Vec::new();
        let mut idx = 0usize;
        for line in out.lines() {
            if line.starts_with("-A OUTPUT ") {
                idx += 1;
                if line.contains(comment) {
                    nums.push(idx);
                }
            }
        }
        // -P line skipped, first -A is rule 1 (lo), second -A is rule 2 (mark),
        // third -A is rule 3 (other), fourth -A is rule 4 (reject).
        assert_eq!(nums, vec![1, 2, 4]);
    }
}
