//! Running the fixed set of system tools with a clean environment, bounded
//! memory and a deadline that a misbehaving child cannot defeat.

use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::secrets;

pub const FIXED_PATHS: [&str; 4] = ["/usr/sbin", "/usr/bin", "/sbin", "/bin"];
pub const MAX_COMMAND_OUTPUT: u64 = 4 * 1024 * 1024;

/// The first executable regular file named `name` in the fixed system paths.
pub fn tool(name: &str) -> Option<PathBuf> {
    FIXED_PATHS
        .iter()
        .map(|dir| Path::new(dir).join(name))
        .find(|p| {
            p.metadata()
                .is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
        })
}

pub fn have_tool(name: &str) -> bool {
    tool(name).is_some()
}

pub fn command_result(
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

pub fn configure_tool_environment(cmd: &mut Command) {
    cmd.env_clear()
        .env("PATH", FIXED_PATHS.join(":"))
        .env("LC_ALL", "C");
}

pub fn command_output(
    tool_name: &str,
    args: &[&str],
    stdin: Option<&str>,
    timeout: Duration,
) -> Result<String, String> {
    let out = command_result(tool_name, args, stdin, timeout)?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        let stderr = secrets::redact_config(&String::from_utf8_lossy(&out.stderr));
        Err(format!("{tool_name} {}: {}", args.join(" "), stderr.trim()))
    }
}

/// Whether an error string came from a command that hit its deadline. The
/// outcome of such a command is unknown and must be re-checked.
pub fn is_timeout(error: &str) -> bool {
    error.ends_with("timed out")
}

pub fn capture_output(reader: impl Read) -> Result<Vec<u8>, String> {
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
pub fn run_command(
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;
    use std::fs;

    #[test]
    fn oversized_outputs_are_bounded() {
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
        let error = result.unwrap_err();
        assert_eq!(error, "timed out");
        assert!(is_timeout(&error));
        assert!(start.elapsed() < Duration::from_secs(3));
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

    #[test]
    fn oversized_input_is_refused_before_spawning() {
        let text = "x".repeat(MAX_COMMAND_OUTPUT as usize + 1);
        let error = run_command(
            Command::new("/nonexistent"),
            Some(&text),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.contains("size limit"), "{error}");
    }
}
