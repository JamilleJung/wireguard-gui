use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const WG_DIR: &str = "/etc/wireguard";
const BACKUP_DIR: &str = "/etc/wireguard/.backup";
const FIXED_PATHS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];
const MAX_CONFIG_BYTES: u64 = 1024 * 1024;

#[allow(dead_code)]
#[path = "../config.rs"]
mod config;
#[path = "../secrets.rs"]
mod secrets;

fn main() {
    if let Err(e) = run() {
        eprintln!("wg-helper: {e}");
        std::process::exit(2);
    }
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let verb = args.next().ok_or("no verb")?;
    let names: Vec<_> = args.collect();
    let expected = match verb.as_str() {
        "list" | "active" | "log" => 0,
        "rename" => 2,
        "read" | "dump" | "up" | "down" | "save" | "delete" | "enable" | "disable"
        | "is-enabled" | "sync" | "showconf" | "persist" | "killswitch-status"
        | "killswitch-enable" | "killswitch-disable" => 1,
        _ => return Err(format!("unknown verb: {verb}")),
    };
    if names.len() != expected {
        return Err(format!("{verb} requires {expected} tunnel name(s)"));
    }
    for name in &names {
        validate_name(name)?;
    }
    // Both frontends share this lock. Check names first, and keep read-only
    // operations independent of directory creation or a long-running mutation.
    let _lock = if matches!(
        verb.as_str(),
        "up" | "down"
            | "save"
            | "delete"
            | "rename"
            | "enable"
            | "disable"
            | "sync"
            | "persist"
            | "killswitch-enable"
            | "killswitch-disable"
    ) {
        Some(lock_helper_state(
            Path::new(WG_DIR),
            Duration::from_secs(2),
        )?)
    } else {
        None
    };
    let mut args = names.into_iter();
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
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
        })
}

fn command_result(
    tool_name: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    let exe = tool(tool_name).ok_or_else(|| format!("{tool_name} not found"))?;
    let mut cmd = Command::new(exe);
    configure_tool_environment(&mut cmd);
    cmd.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }

    run_command(cmd, stdin, timeout).map_err(|e| format!("{tool_name} {}: {e}", args.join(" ")))
}

fn configure_tool_environment(cmd: &mut Command) {
    cmd.env_clear()
        .env("PATH", FIXED_PATHS.join(":"))
        .env("LC_ALL", "C");
}

fn command_output(
    tool_name: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let out = command_result(tool_name, args, stdin, timeout)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = redact(&String::from_utf8_lossy(&out.stderr));
        Err(format!("{tool_name} {}: {}", args.join(" "), stderr.trim()))
    }
}

const MAX_COMMAND_OUTPUT: u64 = 4 * 1024 * 1024;

fn capture_output(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut data = Vec::new();
    reader
        .take(MAX_COMMAND_OUTPUT + 1)
        .read_to_end(&mut data)
        .map_err(|e| format!("read command output: {e}"))?;
    if data.len() as u64 > MAX_COMMAND_OUTPUT {
        return Err("command output exceeds the 4 MiB limit".into());
    }
    Ok(data)
}

fn terminate_command(mut child: std::process::Child, pid: u32) {
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = child.kill();
    // Reap separately: a descendant may have escaped the process group and
    // retained a pipe, and an OS-level wait must not defeat our deadline.
    std::thread::spawn(move || {
        let _ = child.wait();
    });
}

/// Bound stdin/output memory and keep the deadline in the parent. Neither a
/// full pipe nor a daemon escaping with setsid can turn it into an endless wait.
fn run_command(
    mut cmd: Command,
    input: Option<&str>,
    timeout: Duration,
) -> Result<std::process::Output, String> {
    use std::os::unix::process::CommandExt;
    use std::sync::mpsc;
    enum Captured {
        Out(Result<Vec<u8>, String>),
        Err(Result<Vec<u8>, String>),
        Input(Result<(), String>),
    }
    if input.is_some_and(|text| text.len() as u64 > MAX_COMMAND_OUTPUT) {
        return Err("command input exceeds the size limit".into());
    }
    cmd.process_group(0)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("spawn: {e}"))?;
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    let out = child.stdout.take().ok_or("stdout unavailable")?;
    let send = tx.clone();
    std::thread::spawn(move || {
        let _ = send.send(Captured::Out(capture_output(out)));
    });
    let err = child.stderr.take().ok_or("stderr unavailable")?;
    let send = tx.clone();
    std::thread::spawn(move || {
        let _ = send.send(Captured::Err(capture_output(err)));
    });
    let mut input_done = input.is_none();
    if let Some(text) = input {
        let mut pipe = child.stdin.take().ok_or("stdin unavailable")?;
        let text = text.to_owned();
        std::thread::spawn(move || {
            let result = pipe
                .write_all(text.as_bytes())
                .map_err(|e| format!("stdin: {e}"));
            let _ = tx.send(Captured::Input(result));
        });
    } else {
        drop(tx);
    }
    let start = std::time::Instant::now();
    let (mut stdout, mut stderr, mut input_error) = (None, None, None);
    loop {
        // Keep an exited leader unreaped while a descendant retains a pipe.
        // Its reserved PID then cannot be recycled before a timeout kills the
        // original process group. Reap only once no pipe can keep us waiting.
        if input_done && let (Some(stdout), Some(stderr)) = (&mut stdout, &mut stderr) {
            match child.try_wait() {
                Ok(Some(status)) => {
                    if status.success()
                        && let Some(error) = input_error
                    {
                        return Err(error);
                    }
                    return Ok(std::process::Output {
                        status,
                        stdout: std::mem::take(stdout),
                        stderr: std::mem::take(stderr),
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    terminate_command(child, pid);
                    return Err(format!("wait: {error}"));
                }
            }
        }
        let Some(remaining) = timeout.checked_sub(start.elapsed()) else {
            terminate_command(child, pid);
            return Err("timed out".into());
        };
        match rx.recv_timeout(remaining.min(Duration::from_millis(25))) {
            Ok(Captured::Out(Ok(data))) => stdout = Some(data),
            Ok(Captured::Err(Ok(data))) => stderr = Some(data),
            Ok(Captured::Input(result)) => {
                input_done = true;
                input_error = result.err();
            }
            Ok(Captured::Out(Err(error)) | Captured::Err(Err(error))) => {
                terminate_command(child, pid);
                return Err(error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // The readers can finish before the process exits; keep
                // polling its status without spinning or blocking forever.
                std::thread::sleep(remaining.min(Duration::from_millis(10)));
            }
        }
    }
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
    secrets::redact_config(input)
}

fn ensure_wg_dir() -> Result<(), String> {
    ensure_private_dir(Path::new(WG_DIR))
}

fn lock_helper_state(dir: &Path, timeout: Duration) -> Result<File, String> {
    use std::os::fd::AsRawFd;
    ensure_private_dir(dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(dir.join(".helper.lock"))
        .map_err(|e| format!("open helper lock: {e}"))?;
    let metadata = lock.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file() || metadata.nlink() != 1 || metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err("untrusted helper lock file".into());
    }
    lock.set_permissions(fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let start = std::time::Instant::now();
    loop {
        if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(lock);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::WouldBlock {
            return Err(format!("lock helper state: {error}"));
        }
        if start.elapsed() >= timeout {
            return Err("another helper operation is busy; retry when it completes".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn ensure_private_dir(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::DirBuilderExt;
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_dir() || metadata.uid() != unsafe { libc::geteuid() } => {
            return Err(format!("untrusted directory: {}", path.display()));
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            fs::DirBuilder::new()
                .mode(0o700)
                .create(path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
        }
        Err(e) => return Err(format!("inspect {}: {e}", path.display())),
    }
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|e| format!("chmod {}: {e}", path.display()))
}

fn check_private_dir(path: &Path) -> Result<(), String> {
    let metadata =
        fs::symlink_metadata(path).map_err(|e| format!("inspect {}: {e}", path.display()))?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!("untrusted directory: {}", path.display()));
    }
    Ok(())
}

fn open_config(path: &Path) -> Result<File, String> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let metadata = file.metadata().map_err(|e| e.to_string())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o022 != 0
    {
        return Err(format!(
            "config must be an owned regular file with one link and no group/other write permission: {}",
            path.display()
        ));
    }
    Ok(file)
}

fn backup_existing(name: &str) -> Result<(), String> {
    let src = conf_path(name);
    if let Err(error) = fs::symlink_metadata(&src) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(format!("inspect {}: {error}", src.display()));
    }
    let input = open_config(&src)?;
    ensure_private_dir(Path::new(BACKUP_DIR))?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dst = Path::new(BACKUP_DIR).join(format!("{name}.conf.{stamp}.{}", std::process::id()));
    backup_file(input, &dst)?;
    log_action(&format!("backup {name} -> {}", dst.display()));
    Ok(())
}

fn backup_file(mut input: File, dst: &Path) -> Result<(), String> {
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dst)
        .map_err(|e| format!("create backup: {e}"))?;
    let result = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .map_err(|e| format!("backup: {e}"));
    if result.is_err() {
        let _ = fs::remove_file(dst);
    }
    result
}

fn sync_dir(path: &Path) {
    if let Ok(file) = File::open(path) {
        let _ = file.sync_all();
    }
}

fn atomic_write(path: &Path, content: &str) -> Result<(), String> {
    atomic_write_mode(path, content, true)
}

fn atomic_write_mode(path: &Path, content: &str, replace: bool) -> Result<(), String> {
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
    let result = (|| {
        file.write_all(content.as_bytes())
            .map_err(|e| format!("write temp {}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("sync temp {}: {e}", tmp.display()))?;
        if replace {
            fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
        } else {
            // An atomic link fails if another writer has created the target;
            // never overwrite an existing tunnel during rename.
            fs::hard_link(&tmp, path).map_err(|e| format!("create {}: {e}", path.display()))?;
            fs::remove_file(&tmp).map_err(|e| format!("remove staging link: {e}"))?;
        }
        sync_dir(dir);
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

fn validate_config_text(text: &str) -> Result<(), String> {
    config::validate_basic_wireguard_config(text)?;
    if secrets::config_runs_scripts(text) {
        return Err(
            "script hooks are not allowed in managed configs; they would run as root".into(),
        );
    }
    Ok(())
}

fn read_config_text(path: &Path) -> Result<String, String> {
    read_config_input(open_config(path)?)
}

fn read_config_input(reader: impl Read) -> Result<String, String> {
    let mut text = String::new();
    reader
        .take(MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("read config: {e}"))?;
    if text.len() as u64 > MAX_CONFIG_BYTES {
        return Err("config exceeds the 1 MiB size limit".into());
    }
    Ok(text)
}

fn read_stdin() -> Result<String, String> {
    read_config_input(std::io::stdin())
}

fn list() -> Result<(), String> {
    let mut names = Vec::new();
    if let Err(error) = fs::symlink_metadata(WG_DIR) {
        if error.kind() == std::io::ErrorKind::NotFound {
            return Ok(());
        }
        return Err(format!("inspect {WG_DIR}: {error}"));
    }
    check_private_dir(Path::new(WG_DIR))?;
    let entries = fs::read_dir(WG_DIR).map_err(|e| format!("list {WG_DIR}: {e}"))?;
    for entry in entries {
        let entry = entry.map_err(|e| format!("list {WG_DIR}: {e}"))?;
        if !entry.file_type().map_err(|e| e.to_string())?.is_file() {
            continue;
        }
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
    let out = command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5))?;
    print!("{out}");
    Ok(())
}

fn read_config(name: &str) -> Result<(), String> {
    check_private_dir(Path::new(WG_DIR))?;
    print!("{}", read_config_text(&conf_path(name))?);
    Ok(())
}

fn dump(name: &str) -> Result<(), String> {
    let out = command_output("wg", &["show", name, "dump"], None, Duration::from_secs(5))?;
    print!("{out}");
    Ok(())
}

fn up(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    log_action(&format!("up {name}"));
    command_output("wg-quick", &["up", name], None, Duration::from_secs(45)).map(|_| ())
}

fn finish_down(
    stopped: Result<(), String>,
    active: Result<bool, String>,
    cleanup: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    if active? {
        return Err(stopped
            .err()
            .unwrap_or_else(|| "interface is still active; kill switch was preserved".into()));
    }
    cleanup()?;
    stopped
}

fn down(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    log_action(&format!("down {name}"));
    let stopped =
        command_output("wg-quick", &["down", name], None, Duration::from_secs(30)).map(|_| ());
    finish_down(stopped, interface_active(name), || killswitch_disable(name))
}

fn stop_for_mutation(name: &str) -> Result<(), String> {
    if interface_active(name)? {
        let _ = open_config(&conf_path(name))?;
        command_output("wg-quick", &["down", name], None, Duration::from_secs(30))?;
        if interface_active(name)? {
            return Err(format!("interface {name} is still active"));
        }
    }
    killswitch_disable(name)
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

fn autostart_unit(name: &str) -> String {
    format!("wg-quick@{name}.service")
}

fn autostart_enabled(name: &str) -> Result<bool, String> {
    if !have_tool("systemctl") {
        return Ok(false);
    }
    let out = command_result(
        "systemctl",
        &["is-enabled", &autostart_unit(name)],
        None,
        Duration::from_secs(5),
    )?;
    let state = String::from_utf8_lossy(&out.stdout);
    match state.trim() {
        "enabled" if out.status.success() => Ok(true),
        "disabled" | "enabled-runtime" | "static" | "indirect" | "masked" | "masked-runtime"
        | "not-found" | "generated" | "transient" | "linked" | "linked-runtime" | "alias" => {
            Ok(false)
        }
        _ => Err(format!(
            "cannot determine start-on-boot for {name}: {}",
            redact(&String::from_utf8_lossy(&out.stderr))
        )),
    }
}

fn delete(name: &str) -> Result<(), String> {
    backup_existing(name)?;
    // A failed stop, unknown interface state, or failed cleanup must not
    // discard the only config needed to recover the running interface.
    stop_for_mutation(name)?;
    if autostart_enabled(name)? {
        set_autostart(name, false)?;
    }
    let path = conf_path(name);
    if path.try_exists().map_err(|e| e.to_string())? {
        fs::remove_file(&path).map_err(|e| format!("delete {name}: {e}"))?;
        sync_dir(Path::new(WG_DIR));
    }
    log_action(&format!("delete {name}"));
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RenameStep {
    StopAndCleanup,
    EnableNew,
    DisableOld,
    EnableOld,
    DisableNew,
}

fn rename_files(
    src: &Path,
    dst: &Path,
    content: &str,
    enabled: bool,
    mut step: impl FnMut(RenameStep) -> Result<(), String>,
) -> Result<(), String> {
    atomic_write_mode(dst, content, false)?;
    let mut boot_touched = false;
    let result = (|| {
        step(RenameStep::StopAndCleanup)?;
        if enabled {
            boot_touched = true;
            step(RenameStep::EnableNew)?;
            step(RenameStep::DisableOld)?;
        }
        fs::remove_file(src).map_err(|e| format!("remove old tunnel: {e}"))?;
        if let Some(dir) = src.parent() {
            sync_dir(dir);
        }
        Ok(())
    })();
    if let Err(error) = result {
        let mut errors = vec![error];
        if src.try_exists().unwrap_or(false) {
            if boot_touched {
                for action in [RenameStep::EnableOld, RenameStep::DisableNew] {
                    if let Err(error) = step(action) {
                        errors.push(format!("rollback: {error}"));
                    }
                }
            }
            if let Err(error) = fs::remove_file(dst) {
                errors.push(format!("remove new tunnel during rollback: {error}"));
            }
        }
        return Err(errors.join("; "));
    }
    Ok(())
}

fn rename(old: &str, new: &str) -> Result<(), String> {
    if old == new {
        return Err("source and destination names must differ".into());
    }
    let src = conf_path(old);
    let dst = conf_path(new);
    let _ = open_config(&src)?;
    let content = read_stdin()?;
    validate_config_text(&content)?;
    let enabled = autostart_enabled(old)?;
    backup_existing(old)?;
    rename_files(&src, &dst, &content, enabled, |action| match action {
        RenameStep::StopAndCleanup => stop_for_mutation(old),
        RenameStep::EnableNew => set_autostart(new, true),
        RenameStep::DisableOld => set_autostart(old, false),
        RenameStep::EnableOld => set_autostart(old, true),
        RenameStep::DisableNew => set_autostart(new, false),
    })?;
    log_action(&format!("rename {old} -> {new}"));
    Ok(())
}

fn have_tool(name: &str) -> bool {
    tool(name).is_some()
}

fn interface_active(name: &str) -> Result<bool, String> {
    command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5))
        .map(|out| out.split_whitespace().any(|iface| iface == name))
}

fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    if on {
        let _ = open_config(&conf_path(name))?;
    }
    if !have_tool("systemctl") {
        return Err("start-on-boot needs systemd (systemctl not found)".into());
    }
    log_action(&format!("{} {name}", if on { "enable" } else { "disable" }));
    command_output(
        "systemctl",
        &[if on { "enable" } else { "disable" }, &autostart_unit(name)],
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
    println!(
        "{}",
        if autostart_enabled(name)? {
            "enabled"
        } else {
            "disabled"
        }
    );
    Ok(())
}

fn sync_running(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
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
    let out = command_output("wg", &["showconf", name], None, Duration::from_secs(10))?;
    print!("{out}");
    Ok(())
}

fn persist(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    backup_existing(name)?;
    log_action(&format!("persist {name}"));
    command_output("wg-quick", &["save", name], None, Duration::from_secs(30)).map(|_| ())
}

fn log_report_with(
    mut query: impl FnMut(&[&str]) -> Result<String, String>,
) -> Result<String, String> {
    let activity = query(&[
        "--no-pager",
        "-o",
        "short-iso",
        "-n",
        "1000",
        "-t",
        env!("CARGO_PKG_NAME"),
    ])?;
    let quick = query(&[
        "--no-pager",
        "-o",
        "short-iso",
        "-n",
        "1000",
        "-u",
        "wg-quick@*",
    ])?;
    let text = format!(
        "===== {} activity =====\n{}\n===== wg-quick (systemd) =====\n{}",
        env!("CARGO_PKG_NAME"),
        redact(&activity),
        redact(&quick)
    );
    if text.len() as u64 > MAX_COMMAND_OUTPUT {
        return Err("journal output exceeds the size limit".into());
    }
    Ok(text)
}

fn show_log() -> Result<(), String> {
    if !have_tool("journalctl") {
        println!(
            "(journalctl is not available on this system - logs are only recorded on systemd installs.)"
        );
        return Ok(());
    }
    let text =
        log_report_with(|args| command_output("journalctl", args, None, Duration::from_secs(10)))?;
    print!("{text}");
    Ok(())
}

fn parse_fwmark(value: &str) -> Result<u32, String> {
    let value = value.trim();
    let mark = if let Some(hex) = value.strip_prefix("0x") {
        (!hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| u32::from_str_radix(hex, 16).ok())
            .flatten()
    } else {
        (!value.is_empty() && value.bytes().all(|b| b.is_ascii_digit()))
            .then(|| value.parse::<u32>().ok())
            .flatten()
    };
    mark.filter(|n| *n != 0)
        .ok_or_else(|| "invalid or missing WireGuard fwmark".into())
}

fn fwmark(name: &str) -> Result<String, String> {
    let out = command_output(
        "wg",
        &["show", name, "fwmark"],
        None,
        Duration::from_secs(5),
    )?;
    Ok(parse_fwmark(&out)?.to_string())
}

fn unused_fwmark(all: &str) -> Result<u32, String> {
    let mut used = std::collections::HashSet::new();
    for line in all.lines().filter(|l| !l.trim().is_empty()) {
        let value = line
            .split_whitespace()
            .nth(1)
            .ok_or("invalid WireGuard fwmark listing")?;
        if value != "off" {
            used.insert(parse_fwmark(value)?);
        }
    }
    let mut mark = 51820u32;
    while used.contains(&mark) {
        mark = mark.checked_add(1).ok_or("no unused WireGuard fwmark")?;
    }
    Ok(mark)
}

fn ensure_tunnel_has_fwmark(name: &str) -> Result<(), String> {
    if !interface_active(name)? {
        return Err(format!("kill switch needs an active tunnel: {name}"));
    }
    let current = command_output(
        "wg",
        &["show", name, "fwmark"],
        None,
        Duration::from_secs(5),
    )?;
    if current.trim() == "off" {
        let all = command_output(
            "wg",
            &["show", "all", "fwmark"],
            None,
            Duration::from_secs(5),
        )?;
        let mark = unused_fwmark(&all)?.to_string();
        command_output(
            "wg",
            &["set", name, "fwmark", &mark],
            None,
            Duration::from_secs(5),
        )?;
    } else {
        parse_fwmark(&current)?;
    }
    Ok(())
}

fn kill_comment(name: &str) -> String {
    format!("wg-helper-killswitch:{name}")
}

/// Parse the quoting emitted by nft/iptables listing commands; never interpret
/// quoted comment contents as more rule syntax.
fn rule_tokens(line: &str) -> Result<Vec<(String, bool)>, String> {
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

fn rule_words(line: &str) -> Result<Vec<String>, String> {
    Ok(rule_tokens(line)?
        .into_iter()
        .map(|(word, _)| word)
        .collect())
}

fn rule_comment(line: &str) -> Option<String> {
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

fn rule_has_comment(line: &str, expected: &str) -> bool {
    rule_comment(line).as_deref() == Some(expected)
}

fn has_reject(text: &str, comment: &str, nft: bool) -> bool {
    text.lines().any(|line| {
        rule_has_comment(line, comment)
            && rule_tokens(line).is_ok_and(|tokens| {
                if nft {
                    tokens
                        .iter()
                        .any(|(word, quoted)| !quoted && word == "reject")
                } else {
                    tokens
                        .windows(2)
                        .any(|w| w[0].0 == "-j" && w[1].0 == "REJECT")
                }
            })
    })
}

fn complete_switch_rules(text: &str, name: &str, nft: bool) -> bool {
    let comment = kill_comment(name);
    let (mut loopback, mut tunnel, mut mark) = (false, false, false);
    for line in text.lines().filter(|line| rule_has_comment(line, &comment)) {
        let Ok(tokens) = rule_tokens(line) else {
            return false;
        };
        let words: Vec<_> = tokens.iter().map(|(word, _)| word.as_str()).collect();
        let pairs = |a: &str, b: &str| words.windows(2).any(|w| w[0] == a && w[1] == b);
        let accept = if nft {
            tokens
                .iter()
                .any(|(word, quoted)| !quoted && word == "accept")
        } else {
            pairs("-j", "ACCEPT")
        };
        if !accept {
            continue;
        }
        if nft {
            loopback |= pairs("oif", "lo") || pairs("oifname", "lo");
            tunnel |= pairs("oifname", name);
            mark |= words
                .windows(3)
                .any(|w| w[0] == "meta" && w[1] == "mark" && parse_fwmark(w[2]).is_ok());
        } else {
            loopback |= pairs("-o", "lo");
            tunnel |= pairs("-o", name);
            mark |= words.windows(2).any(|w| {
                w[0] == "--mark" && parse_fwmark(w[1].split('/').next().unwrap_or("")).is_ok()
            });
        }
    }
    loopback && tunnel && mark && has_reject(text, &comment, nft)
}

fn switch_is_protected(snapshots: &[(&str, String)], name: &str, ipv6: bool) -> bool {
    let nft = snapshots.iter().any(|(tool, text)| {
        *tool == "nft" && nft_is_output_base_chain(text) && complete_switch_rules(text, name, true)
    });
    let has = |tool: &str| {
        snapshots
            .iter()
            .any(|(t, text)| *t == tool && complete_switch_rules(text, name, false))
    };
    nft || (has("iptables") && (!ipv6 || has("ip6tables")))
}

fn reject_other_switch(name: &str, snapshots: &[(&str, String)]) -> Result<(), String> {
    for (_, text) in snapshots {
        for line in text.lines() {
            if let Some(comment) = rule_comment(line)
                && let Some(other) = comment.strip_prefix("wg-helper-killswitch:")
                && name_ok(other)
                && other != name
            {
                return Err(format!(
                    "kill switch for {other} is already installed; disable it before enabling {name}"
                ));
            }
        }
    }
    Ok(())
}

fn ipv6_available() -> bool {
    Path::new("/proc/net/if_inet6").exists()
}

fn firewall_snapshots() -> Result<Vec<(&'static str, String)>, String> {
    let mut out = Vec::new();
    if have_tool("nft")
        && let Some(text) = nft_snapshot()?
    {
        out.push(("nft", text));
    }
    for tool in ["iptables", "ip6tables"] {
        if have_tool(tool) && (tool != "ip6tables" || ipv6_available()) {
            out.push((
                tool,
                command_output(tool, &["-S", "OUTPUT"], None, Duration::from_secs(5))?,
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// nftables backend (preferred on modern Linux — one table handles both v4+v6)
// ---------------------------------------------------------------------------

fn nft_snapshot() -> Result<Option<String>, String> {
    match command_output(
        "nft",
        &["-a", "list", "chain", "inet", "filter", "output"],
        None,
        Duration::from_secs(5),
    ) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.contains("No such file or directory") => Ok(None),
        Err(error) => Err(error),
    }
}

fn nft_is_output_base_chain(text: &str) -> bool {
    text.lines()
        .any(|line| line.trim().starts_with("type filter hook output priority "))
}

fn nft_ensure_chain() -> Result<String, String> {
    if let Some(text) = nft_snapshot()? {
        if !nft_is_output_base_chain(&text) {
            return Err(
                "inet filter output exists but is not a filter base chain hooked to OUTPUT".into(),
            );
        }
        return Ok(text);
    }
    command_output(
        "nft",
        &["-f", "-"],
        Some(
            "add table inet filter\nadd chain inet filter output { type filter hook output priority 0; policy accept; }\n",
        ),
        Duration::from_secs(5),
    )?;
    let text = nft_snapshot()?.ok_or("nftables output chain was not created")?;
    if !nft_is_output_base_chain(&text) {
        return Err("nftables OUTPUT hook is missing".into());
    }
    Ok(text)
}

fn nft_delete_script(text: &str, name: &str) -> Result<String, String> {
    let comment = kill_comment(name);
    let mut script = String::new();
    for line in text.lines().filter(|line| rule_has_comment(line, &comment)) {
        let handle = line
            .rsplit_once("# handle ")
            .and_then(|(_, h)| h.trim().parse::<u64>().ok())
            .ok_or("managed nft rule is missing its handle")?;
        script.push_str(&format!("delete rule inet filter output handle {handle}\n"));
    }
    Ok(script)
}

fn killswitch_flush_nft(name: &str) -> Result<(), String> {
    if let Some(text) = nft_snapshot()? {
        let script = nft_delete_script(&text, name)?;
        if !script.is_empty() {
            command_output("nft", &["-f", "-"], Some(&script), Duration::from_secs(10))?;
        }
    }
    Ok(())
}

fn nft_update_script(
    text: &str,
    name: &str,
    mark: &str,
    ssh: Option<u16>,
) -> Result<String, String> {
    let mut script = nft_delete_script(text, name)?;
    for rule in nft_killswitch_rules(name, mark, ssh) {
        script.push_str(&rule.join(" "));
        script.push('\n');
    }
    Ok(script)
}

fn killswitch_enable_nft(name: &str, mark: &str) -> Result<(), String> {
    let text = nft_ensure_chain()?;
    reject_other_switch(name, &[("nft", text.clone())])?;
    let ssh = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|s| s.split_whitespace().nth(3)?.parse::<u16>().ok());
    let script = nft_update_script(&text, name, mark, ssh)?;
    // One kernel transaction replaces old rules and adds all exceptions. A
    // failed rule cannot leave a reject-only policy or remove old protection.
    command_output("nft", &["-f", "-"], Some(&script), Duration::from_secs(10)).map(|_| ())
}

fn nft_killswitch_rules(name: &str, mark: &str, ssh_port: Option<u16>) -> Vec<Vec<String>> {
    // Insert the rejection BEFORE existing firewall rules. Appending it would
    // let an existing ACCEPT bypass the kill switch. Exceptions are inserted
    // ahead of it; a later failure leaves the rejection in place (fail closed).
    // nft parses its arguments as a command, so quote the comment explicitly.
    let comment = format!("\"{}\"", kill_comment(name));
    let iface = format!("\"{name}\"");
    let mut rules = Vec::new();
    let mut push = |args: &[&str]| rules.push(args.iter().map(|s| (*s).to_string()).collect());
    push(&[
        "insert", "rule", "inet", "filter", "output", "reject", "comment", &comment,
    ]);
    if let Some(port) = ssh_port {
        push(&[
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
        ]);
    }
    push(&[
        "insert", "rule", "inet", "filter", "output", "oifname", "\"lo\"", "accept", "comment",
        &comment,
    ]);
    // Allow the tunnel's own plaintext egress: an app's packet is routed to the
    // wg interface and traverses OUTPUT with oif=<name> and NO fwmark (only
    // WireGuard's encapsulated transport packets carry the mark, on a second
    // pass). Without this the terminal reject drops it and the tunnel carries
    // no traffic at all.
    push(&[
        "insert", "rule", "inet", "filter", "output", "oifname", &iface, "accept", "comment",
        &comment,
    ]);
    push(&[
        "insert", "rule", "inet", "filter", "output", "meta", "mark", mark, "accept", "comment",
        &comment,
    ]);
    rules
}

// ---------------------------------------------------------------------------
// iptables / ip6tables backend (fallback when nftables is absent)
// ---------------------------------------------------------------------------

fn iptables_delete_lines(text: &str, name: &str) -> Vec<String> {
    let comment = kill_comment(name);
    text.lines()
        .filter(|line| line.starts_with("-A OUTPUT ") && rule_has_comment(line, &comment))
        .map(|line| line.replacen("-A OUTPUT ", "-D OUTPUT ", 1))
        .collect()
}

fn iptables_update_script(text: &str, name: &str, mark: Option<&str>, ssh: Option<u16>) -> String {
    let mut lines = vec!["*filter".to_string()];
    lines.extend(iptables_delete_lines(text, name));
    if let Some(mark) = mark {
        lines.extend(
            iptables_killswitch_rules(name, mark, ssh)
                .into_iter()
                .map(|rule| rule.join(" ")),
        );
    }
    lines.push("COMMIT".into());
    format!("{}\n", lines.join("\n"))
}

fn apply_iptables(tool: &str, script: &str) -> Result<(), String> {
    let restore = format!("{tool}-restore");
    command_output(
        &restore,
        &["--noflush", "--wait", "5"],
        Some(script),
        Duration::from_secs(10),
    )
    .map(|_| ())
}

fn killswitch_flush_iptables(name: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    for tool in ["iptables", "ip6tables"] {
        if !have_tool(tool) || (tool == "ip6tables" && !ipv6_available()) {
            continue;
        }
        let result = (|| {
            let text = command_output(tool, &["-S", "OUTPUT"], None, Duration::from_secs(5))?;
            let lines = iptables_delete_lines(&text, name);
            if lines.is_empty() {
                return Ok(());
            }
            if have_tool(&format!("{tool}-restore")) {
                apply_iptables(tool, &iptables_update_script(&text, name, None, None))
            } else {
                // Recovery remains available for rules installed by older
                // versions even if the restore utility has been removed.
                for line in lines {
                    let words = rule_words(&line)?;
                    let args: Vec<&str> = words.iter().map(String::as_str).collect();
                    command_output(tool, &args, None, Duration::from_secs(5))?;
                }
                Ok(())
            }
        })();
        if let Err(error) = result {
            errors.push(error);
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn require_iptables_backends(
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

fn killswitch_enable_iptables(name: &str, mark: &str) -> Result<(), String> {
    require_iptables_backends(
        have_tool("iptables"),
        have_tool("iptables-restore"),
        have_tool("ip6tables"),
        have_tool("ip6tables-restore"),
        ipv6_available(),
    )?;
    let ssh = std::env::var("SSH_CONNECTION")
        .ok()
        .and_then(|s| s.split_whitespace().nth(3)?.parse::<u16>().ok());
    let mut updates = Vec::new();
    for tool in ["iptables", "ip6tables"] {
        if tool == "ip6tables" && !ipv6_available() {
            continue;
        }
        let text = command_output(tool, &["-S", "OUTPUT"], None, Duration::from_secs(5))?;
        reject_other_switch(name, &[(tool, text.clone())])?;
        let script = iptables_update_script(&text, name, Some(mark), ssh);
        // Parse and check BOTH families before the first commit. The kernel
        // cannot transact across families, so a later operational failure is
        // still reported and status requires both complete rule sets.
        command_output(
            &format!("{tool}-restore"),
            &["--noflush", "--wait", "5", "--test"],
            Some(&script),
            Duration::from_secs(10),
        )?;
        updates.push((tool, script));
    }
    for (tool, script) in updates {
        apply_iptables(tool, &script)?;
    }
    Ok(())
}

fn iptables_killswitch_rules(name: &str, mark: &str, ssh_port: Option<u16>) -> Vec<Vec<String>> {
    let comment = kill_comment(name);
    let mut rules = Vec::new();
    let mut push = |args: &[&str]| rules.push(args.iter().map(|s| (*s).to_string()).collect());
    // As with nftables, reject before the old OUTPUT rules, then insert only
    // the intended exceptions ahead of that rejection.
    push(&[
        "-I",
        "OUTPUT",
        "-m",
        "comment",
        "--comment",
        &comment,
        "-j",
        "REJECT",
    ]);
    if let Some(port) = ssh_port {
        push(&[
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
        ]);
    }
    push(&[
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
    ]);
    push(&[
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
    ]);
    // Allow the tunnel's own plaintext egress (oif=<name>, unmarked) — see
    // the nft path; without it the REJECT drops all tunnelled traffic.
    push(&[
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
    ]);
    rules
}

// ---------------------------------------------------------------------------
// Unified killswitch API  (nftables preferred, iptables fallback)
// ---------------------------------------------------------------------------

fn killswitch_status(name: &str) -> Result<(), String> {
    let snapshots = firewall_snapshots()?;
    println!(
        "{}",
        if switch_is_protected(&snapshots, name, ipv6_available()) {
            "enabled"
        } else {
            "disabled"
        }
    );
    Ok(())
}

fn killswitch_enable(name: &str) -> Result<(), String> {
    // Inspect every installed backend BEFORE changing marks or any rule. Two
    // independent terminal rejects cannot safely compose; keep the first
    // switch intact and require an explicit disable before switching tunnels.
    let snapshots = firewall_snapshots()?;
    reject_other_switch(name, &snapshots)?;
    if have_tool("nft") {
        if let Some((_, text)) = snapshots.iter().find(|(tool, _)| *tool == "nft")
            && !nft_is_output_base_chain(text)
        {
            return Err("inet filter output is not a base chain hooked to OUTPUT".into());
        }
    } else {
        require_iptables_backends(
            have_tool("iptables"),
            have_tool("iptables-restore"),
            have_tool("ip6tables"),
            have_tool("ip6tables-restore"),
            ipv6_available(),
        )?;
    }
    ensure_tunnel_has_fwmark(name)?;
    let mark = fwmark(name)?;
    if have_tool("nft") {
        killswitch_enable_nft(name, &mark)?;
    } else {
        killswitch_enable_iptables(name, &mark)?;
    }
    log_action(&format!("killswitch-enable {name}"));
    Ok(())
}

fn killswitch_disable(name: &str) -> Result<(), String> {
    let mut errors = Vec::new();
    if have_tool("nft")
        && let Err(error) = killswitch_flush_nft(name)
    {
        errors.push(error);
    }
    if let Err(error) = killswitch_flush_iptables(name) {
        errors.push(error);
    }
    if !errors.is_empty() {
        return Err(format!("kill switch cleanup failed: {}", errors.join("; ")));
    }
    log_action(&format!("killswitch-disable {name}"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the built test binary with `sudo unshare -n ... --ignored --exact
    /// tests::firewall_batches_in_isolated_network_namespace --nocapture`.
    /// This guard must precede every firewall command: the test is deliberately
    /// destructive inside its disposable namespace and must never use the host.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "requires root, nftables, iptables and a disposable network namespace"]
    fn firewall_batches_in_isolated_network_namespace() {
        let own_ns = fs::read_link("/proc/self/ns/net").expect("read own network namespace");
        let init_ns = fs::read_link("/proc/1/ns/net").expect("read PID1 network namespace");
        assert_ne!(
            own_ns, init_ns,
            "refusing to alter the PID1 network namespace; run under unshare -n"
        );
        assert_eq!(
            unsafe { libc::geteuid() },
            0,
            "test requires root in the isolated namespace"
        );
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
        nft_run("add table inet filter\nadd chain inet filter output\n").unwrap();
        assert!(!nft_is_output_base_chain(&nft_snapshot().unwrap().unwrap()));
        assert!(
            nft_ensure_chain().is_err(),
            "a regular chain cannot protect OUTPUT"
        );
        nft_run("delete chain inet filter output\nadd chain inet filter output { type filter hook output priority 0; policy accept; }\nadd rule inet filter output accept comment \"integration-unrelated-accept\"\nadd rule inet filter output accept comment \"wg-helper-killswitch:wg01\"\n").unwrap();
        let initial = nft_ensure_chain().unwrap();
        assert!(reject_other_switch("wg0", &[("nft", initial.clone())]).is_err());
        nft_run(&nft_update_script(&initial, "wg0", "51820", Some(22)).unwrap()).unwrap();
        let installed = nft_snapshot().unwrap().unwrap();
        assert!(complete_switch_rules(&installed, "wg0", true));
        assert_eq!(
            installed
                .lines()
                .filter(|l| rule_has_comment(l, &kill_comment("wg0")))
                .count(),
            5
        );
        let reject = installed
            .lines()
            .position(|l| {
                rule_has_comment(l, &kill_comment("wg0"))
                    && l.split_whitespace().any(|w| w == "reject")
            })
            .unwrap();
        let unrelated = installed
            .lines()
            .position(|l| rule_has_comment(l, "integration-unrelated-accept"))
            .unwrap();
        assert!(
            reject < unrelated,
            "an existing ACCEPT must not bypass the reject"
        );
        let broken = format!(
            "{}add rule inet filter output this_is_invalid\n",
            nft_update_script(&installed, "wg0", "51821", None).unwrap()
        );
        assert!(nft_run(&broken).is_err());
        assert_eq!(
            nft_snapshot().unwrap().unwrap(),
            installed,
            "failed nft batch must preserve all old rules"
        );
        nft_run(&nft_update_script(&installed, "wg0", "51821", Some(22)).unwrap()).unwrap();
        let reapplied = nft_snapshot().unwrap().unwrap();
        assert_eq!(
            reapplied
                .lines()
                .filter(|l| rule_has_comment(l, &kill_comment("wg0")))
                .count(),
            5
        );
        assert!(complete_switch_rules(&reapplied, "wg0", true));
        nft_run(&nft_delete_script(&reapplied, "wg0").unwrap()).unwrap();
        let cleaned = nft_snapshot().unwrap().unwrap();
        assert!(
            !cleaned
                .lines()
                .any(|l| rule_has_comment(l, &kill_comment("wg0")))
        );
        assert!(
            cleaned
                .lines()
                .any(|l| rule_has_comment(l, &kill_comment("wg01")))
        );
        assert!(
            cleaned
                .lines()
                .any(|l| rule_has_comment(l, "integration-unrelated-accept"))
        );

        for backend in ["iptables", "ip6tables"] {
            for comment in ["integration-unrelated-accept", "wg-helper-killswitch:wg01"] {
                command_output(
                    backend,
                    &[
                        "-A",
                        "OUTPUT",
                        "-m",
                        "comment",
                        "--comment",
                        comment,
                        "-j",
                        "ACCEPT",
                    ],
                    None,
                    Duration::from_secs(5),
                )
                .unwrap();
            }
            let snapshot = || {
                command_output(backend, &["-S", "OUTPUT"], None, Duration::from_secs(5)).unwrap()
            };
            let initial = snapshot();
            assert!(reject_other_switch("wg0", &[(backend, initial.clone())]).is_err());
            let update = iptables_update_script(&initial, "wg0", Some("51820"), Some(22));
            command_output(
                &format!("{backend}-restore"),
                &["--noflush", "--wait", "5", "--test"],
                Some(&update),
                Duration::from_secs(10),
            )
            .unwrap();
            assert_eq!(snapshot(), initial, "--test must not commit any change");
            apply_iptables(backend, &update).unwrap();
            let installed = snapshot();
            assert!(complete_switch_rules(&installed, "wg0", false));
            assert_eq!(
                installed
                    .lines()
                    .filter(|l| rule_has_comment(l, &kill_comment("wg0")))
                    .count(),
                5
            );
            let reject = installed
                .lines()
                .position(|l| rule_has_comment(l, &kill_comment("wg0")) && l.contains("-j REJECT"))
                .unwrap();
            let unrelated = installed
                .lines()
                .position(|l| rule_has_comment(l, "integration-unrelated-accept"))
                .unwrap();
            assert!(
                reject < unrelated,
                "an existing ACCEPT must not bypass the reject"
            );
            let broken = iptables_update_script(&installed, "wg0", Some("51821"), None)
                .replace("COMMIT\n", "-A OUTPUT --this-is-invalid\nCOMMIT\n");
            assert!(apply_iptables(backend, &broken).is_err());
            assert_eq!(
                snapshot(),
                installed,
                "a failed restore must preserve all old rules"
            );
            apply_iptables(
                backend,
                &iptables_update_script(&installed, "wg0", Some("51821"), Some(22)),
            )
            .unwrap();
            let reapplied = snapshot();
            assert_eq!(
                reapplied
                    .lines()
                    .filter(|l| rule_has_comment(l, &kill_comment("wg0")))
                    .count(),
                5
            );
            assert!(complete_switch_rules(&reapplied, "wg0", false));
            apply_iptables(
                backend,
                &iptables_update_script(&reapplied, "wg0", None, None),
            )
            .unwrap();
            let cleaned = snapshot();
            assert!(
                !cleaned
                    .lines()
                    .any(|l| rule_has_comment(l, &kill_comment("wg0")))
            );
            assert!(
                cleaned
                    .lines()
                    .any(|l| rule_has_comment(l, &kill_comment("wg01")))
            );
            assert!(
                cleaned
                    .lines()
                    .any(|l| rule_has_comment(l, "integration-unrelated-accept"))
            );
        }
    }

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            use std::os::unix::ffi::{OsStrExt, OsStringExt};
            let template = std::env::temp_dir().join("wg-helper-test-XXXXXX");
            let mut bytes = std::ffi::CString::new(template.as_os_str().as_bytes())
                .unwrap()
                .into_bytes_with_nul();
            assert!(!unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null());
            bytes.pop();
            Self(PathBuf::from(std::ffi::OsString::from_vec(bytes)))
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn config_reads_reject_symlinks_hardlinks_directories_and_fifos() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let source = dir.0.join("secret");
        fs::write(&source, "private contents").unwrap();
        let link = dir.0.join("wg0.conf");
        symlink(&source, &link).unwrap();
        assert!(open_config(&link).is_err());
        fs::remove_file(&link).unwrap();
        fs::hard_link(&source, &link).unwrap();
        assert!(open_config(&link).is_err());
        fs::remove_file(&link).unwrap();
        assert!(open_config(&source).is_ok());
        for mode in [0o620, 0o602, 0o666] {
            fs::set_permissions(&source, fs::Permissions::from_mode(mode)).unwrap();
            assert!(
                open_config(&source).is_err(),
                "untrusted config mode {mode:o}"
            );
        }
        fs::set_permissions(&source, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(open_config(&source).is_ok());
        assert!(open_config(&dir.0).is_err());
        let fifo = std::ffi::CString::new(link.to_str().unwrap()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
        assert!(open_config(&link).is_err());
    }

    #[test]
    fn private_directory_checks_do_not_follow_symlinks() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let target = dir.0.join("target");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let link = dir.0.join("link");
        symlink(&target, &link).unwrap();
        assert!(ensure_private_dir(&link).is_err());
        assert!(check_private_dir(&link).is_err());
        assert_eq!(fs::metadata(target).unwrap().mode() & 0o777, 0o755);
    }

    #[test]
    fn shared_mutation_lock_has_a_bounded_wait_and_rejects_links() {
        use std::os::unix::fs::symlink;
        let dir = TempDir::new();
        let first = lock_helper_state(&dir.0, Duration::from_millis(20)).unwrap();
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_err());
        drop(first);
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_ok());
        let path = dir.0.join(".helper.lock");
        fs::remove_file(&path).unwrap();
        let other = dir.0.join("other");
        fs::write(&other, "unchanged").unwrap();
        symlink(&other, &path).unwrap();
        assert!(lock_helper_state(&dir.0, Duration::from_millis(20)).is_err());
        assert_eq!(fs::read_to_string(other).unwrap(), "unchanged");
    }

    #[test]
    fn backups_are_exclusive_private_and_do_not_clobber_existing_files() {
        let dir = TempDir::new();
        let source = dir.0.join("source");
        fs::write(&source, "original").unwrap();
        let backup = dir.0.join("backup");
        backup_file(File::open(&source).unwrap(), &backup).unwrap();
        assert_eq!(fs::metadata(&backup).unwrap().mode() & 0o777, 0o600);
        fs::write(&source, "new value").unwrap();
        assert!(backup_file(File::open(source).unwrap(), &backup).is_err());
        assert_eq!(fs::read_to_string(backup).unwrap(), "original");
    }

    #[test]
    fn failed_atomic_write_cleans_staging_and_no_replace_preserves_target() {
        let dir = TempDir::new();
        let path = dir.0.join("wg0.conf");
        fs::create_dir(&path).unwrap();
        assert!(atomic_write(&path, "new").is_err());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
        fs::remove_dir(&path).unwrap();
        atomic_write(&path, "original").unwrap();
        assert!(atomic_write_mode(&path, "replacement", false).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "original");
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    }

    #[test]
    fn rename_failure_preserves_source_and_rolls_back_boot_changes() {
        let dir = TempDir::new();
        let src = dir.0.join("old.conf");
        let dst = dir.0.join("new.conf");
        fs::write(&src, "original").unwrap();
        let mut steps = Vec::new();
        let result = rename_files(&src, &dst, "edited", true, |step| {
            steps.push(step);
            if step == RenameStep::DisableOld {
                Err("injected systemd error".into())
            } else {
                Ok(())
            }
        });
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&src).unwrap(), "original");
        assert!(!dst.exists());
        assert_eq!(
            steps,
            vec![
                RenameStep::StopAndCleanup,
                RenameStep::EnableNew,
                RenameStep::DisableOld,
                RenameStep::EnableOld,
                RenameStep::DisableNew
            ]
        );
        fs::write(&dst, "existing tunnel").unwrap();
        assert!(
            rename_files(&src, &dst, "edited", false, |_| panic!(
                "must not stop before staging succeeds"
            ))
            .is_err()
        );
        assert_eq!(fs::read_to_string(dst).unwrap(), "existing tunnel");
    }

    #[test]
    fn successful_rename_stages_then_removes_source() {
        let dir = TempDir::new();
        let src = dir.0.join("old.conf");
        let dst = dir.0.join("new.conf");
        fs::write(&src, "original").unwrap();
        rename_files(&src, &dst, "edited", false, |_| {
            assert_eq!(fs::read_to_string(&src).unwrap(), "original");
            assert_eq!(fs::read_to_string(&dst).unwrap(), "edited");
            Ok(())
        })
        .unwrap();
        assert!(!src.exists());
        assert_eq!(fs::read_to_string(dst).unwrap(), "edited");
    }

    #[test]
    fn stop_failure_never_removes_protection_from_an_active_or_unknown_interface() {
        assert!(
            finish_down(Err("stop failed".into()), Ok(true), || panic!(
                "must preserve protection"
            ))
            .is_err()
        );
        assert!(
            finish_down(Ok(()), Err("status failed".into()), || panic!(
                "must preserve protection"
            ))
            .is_err()
        );
        assert_eq!(
            finish_down(Ok(()), Ok(false), || Err("cleanup failed".into())),
            Err("cleanup failed".into())
        );
        let mut cleaned = false;
        assert!(
            finish_down(Err("already down".into()), Ok(false), || {
                cleaned = true;
                Ok(())
            })
            .is_err()
        );
        assert!(cleaned);
    }

    #[test]
    fn oversized_inputs_and_outputs_are_bounded() {
        assert!(read_config_input(std::io::repeat(b'x').take(MAX_CONFIG_BYTES + 2)).is_err());
        assert!(capture_output(std::io::repeat(b'x').take(MAX_COMMAND_OUTPUT + 2)).is_err());
        let mut command = Command::new("sh");
        command.args(["-c", "yes x"]);
        let start = std::time::Instant::now();
        assert!(
            run_command(command, None, Duration::from_secs(5))
                .unwrap_err()
                .contains("4 MiB")
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn timeout_survives_a_setsid_child_holding_output_pipes() {
        let dir = TempDir::new();
        let pidfile = dir.0.join("pid");
        let script = "import os,sys,time\npid=os.fork()\nif pid == 0:\n os.setsid()\n open(sys.argv[1],'w').write(str(os.getpid()))\n time.sleep(30)\nelse:\n time.sleep(30)\n";
        let mut command = Command::new("python3");
        command.args(["-c", script]).arg(&pidfile);
        let start = std::time::Instant::now();
        let result = run_command(command, None, Duration::from_millis(300));
        if let Ok(pid) = fs::read_to_string(&pidfile) {
            unsafe {
                libc::kill(pid.parse::<i32>().unwrap(), libc::SIGKILL);
            }
        }
        assert_eq!(result.unwrap_err(), "timed out");
        assert!(start.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn firewall_fallback_requires_every_supported_address_family() {
        assert!(require_iptables_backends(false, false, true, true, true).is_err());
        assert!(require_iptables_backends(true, true, false, false, true).is_err());
        assert!(require_iptables_backends(true, false, true, true, true).is_err());
        assert!(require_iptables_backends(true, true, false, false, false).is_ok());
        assert!(require_iptables_backends(true, true, true, true, true).is_ok());
    }

    #[test]
    fn regular_nft_chain_and_partial_firewall_installations_are_not_protected() {
        let nft = nft_killswitch_rules("wg0", "51820", None)
            .into_iter()
            .map(|r| r[5..].join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!switch_is_protected(&[("nft", nft.clone())], "wg0", true));
        let hooked = format!("type filter hook output priority filter; policy accept;\n{nft}");
        assert!(switch_is_protected(&[("nft", hooked)], "wg0", true));
        let legacy = iptables_killswitch_rules("wg0", "51820", None)
            .into_iter()
            .map(|r| r.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(!switch_is_protected(
            &[("iptables", legacy.clone())],
            "wg0",
            true
        ));
        assert!(switch_is_protected(
            &[("iptables", legacy.clone()), ("ip6tables", legacy)],
            "wg0",
            true
        ));
        assert!(!switch_is_protected(
            &[(
                "nft",
                "type filter hook output priority 0;\nreject comment \"wg-helper-killswitch:wg0\""
                    .into()
            )],
            "wg0",
            true
        ));
    }

    #[test]
    fn single_switch_check_keeps_other_tunnel_rules_intact() {
        let rules = "reject comment \"wg-helper-killswitch:wg01\" # handle 4".to_string();
        assert!(reject_other_switch("wg0", &[("nft", rules.clone())]).is_err());
        assert!(reject_other_switch("wg01", &[("nft", rules)]).is_ok());
        assert!(
            reject_other_switch(
                "wg0",
                &[(
                    "nft",
                    "accept comment \"unrelated wg-helper-killswitch:wg01\"".into()
                )]
            )
            .is_ok()
        );
    }

    #[test]
    fn firewall_updates_are_one_transaction_and_keep_unrelated_rules() {
        let nft = "reject comment \"wg-helper-killswitch:wg0\" # handle 5\naccept comment \"unrelated\" # handle 6";
        let script = nft_update_script(nft, "wg0", "51820", None).unwrap();
        assert!(script.starts_with("delete rule inet filter output handle 5\n"));
        assert!(!script.contains("handle 6"));
        assert!(
            nft_update_script(
                "reject comment \"wg-helper-killswitch:wg0\"",
                "wg0",
                "51820",
                None
            )
            .is_err()
        );
        let old = "-A OUTPUT -m comment --comment \"wg-helper-killswitch:wg0\" -j REJECT\n-A OUTPUT -j ACCEPT";
        let script = iptables_update_script(old, "wg0", Some("51820"), None);
        assert!(script.starts_with("*filter\n-D OUTPUT -m comment --comment \"wg-helper-killswitch:wg0\" -j REJECT\n-I OUTPUT"));
        assert!(script.ends_with("COMMIT\n"));
        assert!(!script.contains("-D OUTPUT -j ACCEPT"));
        assert!(!script.contains(":OUTPUT"));
    }

    #[test]
    fn runtime_marks_do_not_collide_and_numeric_interface_names_are_quoted() {
        assert_eq!(
            unused_fwmark("wg0\t0xca6c\nwg1\t51821\nwg2\toff\n").unwrap(),
            51822
        );
        for bad in ["off", "0", "+1", "1; accept", "0x", "4294967296"] {
            assert!(parse_fwmark(bad).is_err());
        }
        let rules = nft_killswitch_rules("123", "51820", None);
        assert!(
            rules
                .iter()
                .any(|r| r.windows(2).any(|w| w == ["oifname", "\"123\""]))
        );
    }

    #[test]
    fn boot_units_always_include_the_service_suffix() {
        assert_eq!(autostart_unit("wg0"), "wg-quick@wg0.service");
        assert_eq!(
            autostart_unit("home.service"),
            "wg-quick@home.service.service"
        );
        assert_eq!(autostart_unit("vpn.socket"), "wg-quick@vpn.socket.service");
    }

    #[test]
    fn journal_failures_are_reported_and_successful_output_is_redacted() {
        let mut count = 0;
        let result = log_report_with(|_| {
            count += 1;
            if count == 2 {
                Err("journal unavailable".into())
            } else {
                Ok("first query".into())
            }
        });
        assert_eq!(result, Err("journal unavailable".into()));
        let report =
            log_report_with(|_| Ok("PrivateKey = secret-value\nordinary log line".into())).unwrap();
        assert!(!report.contains("secret-value"));
        assert!(report.contains("ordinary log line"));
    }

    #[test]
    fn tool_environment_excludes_caller_loader_and_service_overrides() {
        let mut cmd = Command::new("/bin/sh");
        for name in [
            "XTABLES_LIBDIR",
            "IPTABLES_LIB_DIR",
            "WG_QUICK_USERSPACE_IMPLEMENTATION",
            "BASH_ENV",
            "ENV",
            "SYSTEMD_HOST",
            "DBUS_SYSTEM_BUS_ADDRESS",
            "LD_PRELOAD",
            "SSH_CONNECTION",
            "SUDO_USER",
            "PKEXEC_UID",
            "HOME",
        ] {
            cmd.env(name, "untrusted-value");
        }
        configure_tool_environment(&mut cmd);
        cmd.args(["-c", "printf '%s\\n' \"$PATH\" \"$LC_ALL\" \"${XTABLES_LIBDIR-}${IPTABLES_LIB_DIR-}${WG_QUICK_USERSPACE_IMPLEMENTATION-}${BASH_ENV-}${ENV-}${SYSTEMD_HOST-}${DBUS_SYSTEM_BUS_ADDRESS-}${LD_PRELOAD-}${SSH_CONNECTION-}${SUDO_USER-}${PKEXEC_UID-}${HOME-}\""]);
        let out = run_command(cmd, None, Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            format!("{}\nC\n\n", FIXED_PATHS.join(":"))
        );
    }

    #[test]
    fn command_drains_large_stdout_and_stderr() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "head -c 262144 /dev/zero; head -c 262144 /dev/zero >&2",
        ]);
        let out = run_command(cmd, None, Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert_eq!(out.stdout.len(), 262144);
        assert_eq!(out.stderr.len(), 262144);
    }

    #[test]
    fn command_timeout_stops_children_with_inherited_pipes() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & wait"]);
        let start = std::time::Instant::now();
        let error = run_command(cmd, None, Duration::from_millis(100)).unwrap_err();
        assert_eq!(error, "timed out");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

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
    fn inline_comments_are_valid_and_do_not_hide_hooks() {
        let cfg = format!(
            "[Interface] # home\nPrivateKey = {KEY} # secret\n\
             [Peer] # gateway\nPublicKey = {KEY} # remote\nAllowedIPs = 0.0.0.0/0 # IPv4\n"
        );
        assert!(validate_config_text(&cfg).is_ok());
        for hook in ["PreUp", "PostUp", "PreDown", "PostDown"] {
            assert!(validate_config_text(&format!("{cfg}{hook} = id # hook\n")).is_err());
        }
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
    fn firewall_keyword_interface_names_are_values_not_rule_syntax() {
        for name in ["comment", "reject", "accept"] {
            let nft_rules = nft_killswitch_rules(name, "51820", None);
            let nft = nft_rules
                .iter()
                .enumerate()
                .map(|(i, rule)| format!("{} # handle {}", rule.join(" "), i + 1))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(complete_switch_rules(&nft, name, true), "{name}");
            assert_eq!(
                nft_delete_script(&nft, name).unwrap().lines().count(),
                4,
                "{name}"
            );
            let no_reject = nft.lines().skip(1).collect::<Vec<_>>().join("\n");
            assert!(
                !complete_switch_rules(&no_reject, name, true),
                "a quoted interface named {name} is not a verdict"
            );
            let ipt = iptables_killswitch_rules(name, "51820", None)
                .iter()
                .map(|rule| rule.join(" ").replacen("-I OUTPUT", "-A OUTPUT", 1))
                .collect::<Vec<_>>()
                .join("\n");
            assert!(complete_switch_rules(&ipt, name, false), "{name}");
            assert_eq!(iptables_delete_lines(&ipt, name).len(), 4, "{name}");
        }
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
    }

    #[test]
    fn nft_killswitch_reject_precedes_existing_accept_rules() {
        let mut chain = vec![vec!["existing accept".to_string()]];
        for rule in nft_killswitch_rules("wg0", "0xca6c", Some(22)) {
            assert_eq!(rule[0], "insert");
            chain.insert(0, rule);
        }
        let reject = chain
            .iter()
            .position(|r| r.iter().any(|s| s == "reject"))
            .unwrap();
        let existing = chain
            .iter()
            .position(|r| r[0] == "existing accept")
            .unwrap();
        assert!(reject < existing);
        assert_eq!(reject, 4); // SSH, loopback, tunnel egress, and transport exceptions.
        assert!(
            chain[..reject]
                .iter()
                .all(|r| r.iter().any(|s| s == "accept"))
        );
    }

    #[test]
    fn iptables_killswitch_reject_precedes_existing_accept_rules() {
        let mut chain = vec![vec!["existing accept".to_string()]];
        for rule in iptables_killswitch_rules("wg0", "0xca6c", Some(22)) {
            assert_eq!(rule[0], "-I");
            chain.insert(0, rule);
        }
        let reject = chain
            .iter()
            .position(|r| r.iter().any(|s| s == "REJECT"))
            .unwrap();
        let existing = chain
            .iter()
            .position(|r| r[0] == "existing accept")
            .unwrap();
        assert!(reject < existing);
        assert_eq!(reject, 4);
        assert!(
            chain[..reject]
                .iter()
                .all(|r| r.iter().any(|s| s == "ACCEPT"))
        );
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
}
