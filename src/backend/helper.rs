//! Running the privileged `wg-helper` (and unprivileged `wg`) processes.
//!
//! Everything that needs root (reading /etc/wireguard, `wg show`, `wg-quick`)
//! goes through [`helper`], which runs the helper as:
//!   * nothing             — when we are already root
//!   * `sudo -n wg-helper` — the normal case (NOPASSWD sudoers drop-in)
//!   * `pkexec wg-helper`  — fallback when sudo is not set up (prompts)

use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use super::error::HelperError;
use crate::{logging, secrets};

#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Escalation {
    Direct,
    Sudo,
    Pkexec,
}

pub(crate) static ESC: OnceLock<Escalation> = OnceLock::new();
pub(crate) static HELPER: OnceLock<Option<String>> = OnceLock::new();

/// Upper bound for one helper invocation (wg-quick up can be slow).
pub(crate) const HELPER_TIMEOUT: Duration = Duration::from_secs(90);
pub(crate) const MAX_HELPER_OUTPUT: usize = 4 * 1024 * 1024;

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
        .and_then(|child| helper_output_timeout(child, None, Duration::from_secs(2)).ok())
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

/// Resolve an allowed `$WG_HELPER` override, this application's trusted
/// installed or adjacent helper, or a development helper in debug builds.
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

pub(crate) fn trusted_helper(path: &std::path::Path) -> bool {
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
fn spawn_error(esc: Escalation, helper: &str, e: &std::io::Error) -> HelperError {
    if e.kind() == std::io::ErrorKind::NotFound {
        return HelperError::Spawn(match esc {
            Escalation::Direct => {
                format!("helper not found at {helper} - run ./install.sh (or set it up) first")
            }
            Escalation::Sudo => "'sudo' not found on PATH".to_string(),
            Escalation::Pkexec => "can't gain root: no passwordless sudo, and pkexec (polkit) \
                 isn't installed. Re-run ./install.sh (it now sets up sudo for you), or start \
                 this as root."
                .to_string(),
        });
    }
    HelperError::Spawn(format!("spawn failed: {e}"))
}

/// Run the helper with a verb (+ optional names) and optional stdin payload.
pub(crate) fn helper(args: &[&str], stdin: Option<&str>) -> Result<String, HelperError> {
    let esc = *ESC.get().unwrap_or(&Escalation::Pkexec);
    let helper = helper_path().ok_or(HelperError::NotInstalled)?;

    // Polling must not open authentication dialogs every refresh. pkcheck
    // without --allow-user-interaction only accepts an existing authorization;
    // explicit mutating actions still use pkexec's normal authentication flow.
    if esc == Escalation::Pkexec
        && read_only_helper_verb(args.first().copied().unwrap_or(""))
        && !polkit_authorized(helper)
    {
        return Err(HelperError::AuthorizationRequired);
    }
    invoke(esc, helper, args, stdin, HELPER_TIMEOUT)
}

/// The invocation path behind [`helper`], with the escalation, executable and
/// deadline explicit so it can be exercised against fake helpers in tests.
pub(crate) fn invoke(
    esc: Escalation,
    helper: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, HelperError> {
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
            c.arg("--disable-internal-agent").arg(helper).args(args);
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

    let verb = args.first().copied().unwrap_or("");
    let started = std::time::Instant::now();
    let result = cmd
        .spawn()
        .map_err(|e| spawn_error(esc, helper, &e))
        .and_then(|child| helper_output_timeout(child, stdin.map(str::to_owned), timeout))
        .and_then(|out| {
            if !out.status.success() {
                let err = secrets::redact_config(&String::from_utf8_lossy(&out.stderr));
                return Err(HelperError::from_failure(
                    format!("{} {}", helper, args.join(" ")),
                    err.trim().to_string(),
                ));
            }
            String::from_utf8(out.stdout).map_err(|_| HelperError::InvalidUtf8)
        });
    // Background polling runs every second; record mutations and failures.
    if !read_only_helper_verb(verb) || result.is_err() {
        let elapsed = started.elapsed().as_millis().to_string();
        let target = args.get(1..).map(|rest| rest.join(" ")).unwrap_or_default();
        match &result {
            Ok(_) => logging::log(
                "helper",
                &[
                    ("verb", verb),
                    ("target", &target),
                    ("outcome", "ok"),
                    ("ms", &elapsed),
                ],
            ),
            Err(error) => logging::log(
                "helper",
                &[
                    ("verb", verb),
                    ("target", &target),
                    ("outcome", error.kind()),
                    ("ms", &elapsed),
                    ("error", &error.to_string()),
                ],
            ),
        }
    }
    result
}

fn nonblocking(pipe: &impl std::os::fd::AsRawFd) -> Result<(), HelperError> {
    let fd = pipe.as_raw_fd();
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        Err(HelperError::Io(std::io::Error::last_os_error().to_string()))
    } else {
        Ok(())
    }
}

fn drain_helper_pipe(
    pipe: &mut Option<impl std::io::Read>,
    bytes: &mut Vec<u8>,
) -> Result<(), HelperError> {
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
            Ok(_) => return Err(HelperError::OutputTooLarge),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(e) => return Err(HelperError::Io(e.to_string())),
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn helper_output(
    child: std::process::Child,
    input: Option<String>,
) -> Result<std::process::Output, HelperError> {
    helper_output_timeout(child, input, HELPER_TIMEOUT)
}

pub(crate) fn helper_output_timeout(
    mut child: std::process::Child,
    input: Option<String>,
    timeout: Duration,
) -> Result<std::process::Output, HelperError> {
    use std::io::Write;
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
    let unavailable = |what: &str| HelperError::Io(format!("Helper {what} is unavailable."));
    let result = (|| {
        nonblocking(stdout.as_ref().ok_or_else(|| unavailable("stdout"))?)?;
        nonblocking(stderr.as_ref().ok_or_else(|| unavailable("stderr"))?)?;
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
                status = child
                    .try_wait()
                    .map_err(|e| HelperError::Io(e.to_string()))?;
            }
            if status.is_some() && stdout.is_none() && stderr.is_none() {
                break;
            }
            if started.elapsed() >= timeout {
                return Err(HelperError::TimedOut);
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        let status = status.ok_or_else(|| HelperError::Io("Missing helper exit status.".into()))?;
        if status.success() && (failed_input || offset != input.len()) {
            return Err(HelperError::IncompleteInput);
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
        while deadline.elapsed() < Duration::from_millis(200) {
            if child.try_wait().ok().flatten().is_some() {
                return result;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::spawn(move || {
            let _ = child.wait();
        });
    }
    result
}

/// Verbs that only read state. They never open an interactive authentication
/// dialog under pkexec. Mutating verbs (including `killswitch-repair`) must
/// not be listed here.
pub(crate) fn read_only_helper_verb(verb: &str) -> bool {
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

pub(crate) fn polkit_subject(stat: &str, pid: u32, uid: u32) -> Option<String> {
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
            Ok(None) if start.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(10))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
        }
    }
}

/// Run an unprivileged `wg` subcommand (`genkey`, `pubkey`, `genpsk`).
pub(crate) fn wg_output(
    verb: &str,
    input: Option<String>,
) -> Result<std::process::Output, HelperError> {
    use std::os::unix::process::CommandExt;
    let executable = if unsafe { libc::geteuid() } == 0 && !cfg!(debug_assertions) {
        ["/usr/sbin/wg", "/usr/bin/wg", "/sbin/wg", "/bin/wg"]
            .into_iter()
            .find(|path| trusted_helper(std::path::Path::new(path)))
            .ok_or_else(|| HelperError::Spawn("No trusted wg executable found.".into()))?
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
    let child = command
        .spawn()
        .map_err(|e| HelperError::Spawn(format!("wg {verb}: {e}")))?;
    helper_output_timeout(child, input, Duration::from_secs(10))
}
