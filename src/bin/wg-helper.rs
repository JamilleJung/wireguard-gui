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
    command_output("wg-quick", &["down", name], None, Duration::from_secs(30)).map(|_| ())
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
            "300",
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
            "200",
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
            "kill switch needs an active wg-quick tunnel with fwmark: {name}"
        ))
    } else {
        Ok(mark.to_string())
    }
}

fn kill_comment(name: &str) -> String {
    format!("wg-helper-killswitch:{name}")
}

fn iptables_rule_args<'a>(name: &'a str, mark: &'a str, comment: &'a str) -> Vec<&'a str> {
    vec![
        "OUTPUT",
        "!",
        "-o",
        name,
        "-m",
        "mark",
        "!",
        "--mark",
        mark,
        "-m",
        "addrtype",
        "!",
        "--dst-type",
        "LOCAL",
        "-m",
        "comment",
        "--comment",
        comment,
        "-j",
        "REJECT",
    ]
}

fn iptables_has_comment(tool_name: &str, comment: &str) -> bool {
    command_output(tool_name, &["-S", "OUTPUT"], None, Duration::from_secs(5))
        .map(|out| out.lines().any(|line| line.contains(comment)))
        .unwrap_or(false)
}

fn killswitch_status(name: &str) -> Result<(), String> {
    let enabled = iptables_has_comment("iptables", &kill_comment(name))
        || iptables_has_comment("ip6tables", &kill_comment(name));
    println!("{}", if enabled { "enabled" } else { "disabled" });
    Ok(())
}

fn killswitch_enable(name: &str) -> Result<(), String> {
    let mark = fwmark(name)?;
    let comment = kill_comment(name);
    if !have_tool("iptables") {
        return Err("kill switch needs iptables".into());
    }
    for tool_name in ["iptables", "ip6tables"] {
        if !have_tool(tool_name) || iptables_has_comment(tool_name, &comment) {
            continue;
        }
        let mut args = vec!["-I"];
        let rule = iptables_rule_args(name, &mark, &comment);
        args.extend(rule);
        command_output(tool_name, &args, None, Duration::from_secs(10))?;
    }
    log_action(&format!("killswitch-enable {name}"));
    Ok(())
}

fn killswitch_disable(name: &str) -> Result<(), String> {
    let comment = kill_comment(name);
    for tool_name in ["iptables", "ip6tables"] {
        if !have_tool(tool_name) {
            continue;
        }
        let Ok(out) = command_output(tool_name, &["-S", "OUTPUT"], None, Duration::from_secs(5))
        else {
            continue;
        };
        for line in out.lines().filter(|line| line.contains(&comment)) {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() < 3 || parts[0] != "-A" || parts[1] != "OUTPUT" {
                continue;
            }
            let mut args = vec!["-D", "OUTPUT"];
            args.extend(parts.into_iter().skip(2));
            let _ = command_output(tool_name, &args, None, Duration::from_secs(10));
        }
    }
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
}
