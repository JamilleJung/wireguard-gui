//! Decode QR images in a short-lived child process.
//!
//! The image and QR decoders parse untrusted files. A bug in them must not take
//! the application down: `rqrr` 0.11 (the newest release) asserts on some
//! crafted grids, and with `panic = "abort"` that would abort the whole app
//! and, in the terminal UI, leave the terminal in raw mode. The app therefore
//! re-runs its own executable with [`CHILD_ARG`], sends the image bytes on
//! stdin and reads the decoded config from stdout. A crash, hang or runaway
//! allocation in the child only fails that one import.

use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::{import_parse, secrets};

/// Hidden first argument that makes the executable act as the QR decoder.
pub const CHILD_ARG: &str = "--internal-decode-qr";

const DEADLINE: Duration = Duration::from_secs(15);
/// Largest image accepted on stdin (the frontends cap files well below this).
const MAX_INPUT: u64 = 64 * 1024 * 1024;
/// A decoded config is at most 1 MiB; anything longer is refused.
const MAX_OUTPUT: u64 = 1024 * 1024 + 1;
const MAX_STDERR: u64 = 64 * 1024;
#[cfg_attr(test, allow(dead_code))]
/// Address-space ceiling for the child: generous for a 4096x4096 image, far
/// below what could hurt the desktop.
const CHILD_MEMORY: u64 = 1024 * 1024 * 1024;

const CRASHED: &str = "The QR decoder stopped while reading this image, so it is not a readable \
WireGuard QR code. Try a clearer, uncropped image of the QR code.";

/// Entry point of the child process. Never returns.
pub fn child_main() -> ! {
    let mut bytes = Vec::new();
    let read = std::io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes);
    let result = match read {
        Ok(_) if bytes.len() as u64 > MAX_INPUT => Err("QR image is too large.".to_string()),
        Ok(_) => import_parse::decode_qr_bytes(&bytes),
        Err(error) => Err(format!("Could not read the QR image: {error}")),
    };
    match result {
        Ok(config) => {
            let mut out = std::io::stdout().lock();
            let ok = out.write_all(config.as_bytes()).and_then(|()| out.flush());
            std::process::exit(if ok.is_ok() { 0 } else { 1 });
        }
        Err(error) => {
            eprint!("{error}");
            std::process::exit(1);
        }
    }
}

/// Decode `bytes` as a QR image without risking this process.
pub fn decode(bytes: &[u8]) -> Result<String, String> {
    #[cfg(test)]
    {
        // The unit-test harness is not the application binary; the child
        // protocol itself is covered by `tests/qr_isolation.rs`.
        import_parse::decode_qr_bytes(bytes)
    }
    #[cfg(not(test))]
    {
        let mut cmd = Command::new(self_exe());
        cmd.arg(CHILD_ARG);
        limit_child_memory(&mut cmd);
        decode_with(cmd, bytes, DEADLINE)
    }
}

#[cfg(not(test))]
fn self_exe() -> std::path::PathBuf {
    // /proc/self/exe keeps working even if an update replaced the file.
    let proc = std::path::PathBuf::from("/proc/self/exe");
    if proc.exists() {
        proc
    } else {
        std::env::current_exe().unwrap_or(proc)
    }
}

#[cfg(not(test))]
fn limit_child_memory(cmd: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setrlimit is async-signal-safe and only touches the child.
    unsafe {
        cmd.pre_exec(|| {
            let limit = libc::rlimit {
                rlim_cur: CHILD_MEMORY as libc::rlim_t,
                rlim_max: CHILD_MEMORY as libc::rlim_t,
            };
            libc::setrlimit(libc::RLIMIT_AS, &limit);
            Ok(())
        });
    }
}

/// Keep at most `limit` bytes but drain the rest, so a child that writes too
/// much still runs to completion instead of blocking on a full pipe.
fn read_bounded(mut reader: impl Read, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut data = Vec::new();
    reader.by_ref().take(limit).read_to_end(&mut data)?;
    std::io::copy(&mut reader, &mut std::io::sink())?;
    Ok(data)
}

/// Run a decoder command: bytes on stdin, config on stdout, error text on
/// stderr. Bounded in time, input and output.
fn decode_with(mut cmd: Command, bytes: &[u8], deadline: Duration) -> Result<String, String> {
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    if bytes.len() as u64 > MAX_INPUT {
        return Err("QR image is too large.".into());
    }
    cmd.process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Could not start the QR decoder: {e}"))?;
    let mut stdin = child.stdin.take().ok_or("QR decoder stdin unavailable")?;
    let input = bytes.to_vec();
    let writer = std::thread::spawn(move || {
        // A child that exits early closes the pipe; that is not an error here.
        let _ = stdin.write_all(&input);
    });
    let stdout = child.stdout.take().ok_or("QR decoder stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("QR decoder stderr unavailable")?;
    let out = std::thread::spawn(move || read_bounded(stdout, MAX_OUTPUT));
    let err = std::thread::spawn(move || read_bounded(stderr, MAX_STDERR));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if start.elapsed() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) => {
                unsafe {
                    libc::kill(-(child.id() as i32), libc::SIGKILL);
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err(
                    "Reading the QR image took too long and was stopped. Try a smaller, clearer image."
                        .into(),
                );
            }
            Err(error) => return Err(format!("QR decoder failed: {error}")),
        }
    };
    let _ = writer.join();
    let stdout = out.join().ok().and_then(Result::ok).unwrap_or_default();
    let stderr = err.join().ok().and_then(Result::ok).unwrap_or_default();
    if status.signal().is_some() {
        return Err(CRASHED.into());
    }
    match status.code() {
        Some(0) if stdout.len() as u64 >= MAX_OUTPUT => {
            Err("The QR code holds more text than a tunnel config can have.".into())
        }
        Some(0) => String::from_utf8(stdout).map_err(|_| "The QR code is not valid text.".into()),
        Some(1) if !stderr.is_empty() => Err(secrets::redact_config(
            String::from_utf8_lossy(&stderr).trim(),
        )),
        _ => Err(CRASHED.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", script]);
        cmd
    }

    #[test]
    fn a_crashing_decoder_only_fails_the_import() {
        let error = decode_with(sh("cat >/dev/null; kill -ABRT $$"), b"img", DEADLINE).unwrap_err();
        assert_eq!(error, CRASHED);
        let error = decode_with(sh("exit 101"), b"img", DEADLINE).unwrap_err();
        assert_eq!(error, CRASHED);
    }

    #[test]
    fn a_hanging_decoder_is_stopped_at_the_deadline() {
        let start = Instant::now();
        let error = decode_with(sh("sleep 30"), b"img", Duration::from_millis(200)).unwrap_err();
        assert!(error.contains("too long"), "{error}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn decoder_output_and_errors_are_passed_through_bounded_and_redacted() {
        assert_eq!(
            decode_with(sh("cat >/dev/null; printf 'config text'"), b"img", DEADLINE).unwrap(),
            "config text"
        );
        let error = decode_with(
            sh("cat >/dev/null; printf 'PrivateKey = SECRET' >&2; exit 1"),
            b"img",
            DEADLINE,
        )
        .unwrap_err();
        assert!(!error.contains("SECRET"), "{error}");
        let error = decode_with(
            sh("cat >/dev/null; head -c 2000000 /dev/zero"),
            b"x",
            DEADLINE,
        )
        .unwrap_err();
        assert!(error.contains("more text"), "{error}");
        // A decoder that ignores stdin cannot block the writer.
        assert!(decode_with(sh("exit 0"), &vec![0u8; 4 << 20], DEADLINE).is_ok());
    }
}
