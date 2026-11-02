/// Normalize single-field copy payloads.
///
/// UI labels often wrap or contain incidental spacing. Single values such as
/// public keys, endpoints, addresses, and AllowedIPs should copy cleanly without
/// leading spaces, trailing spaces, or accidental newlines. Raw configs/logs do
/// not use this helper because their newlines are meaningful.
pub fn normalize_single_field_copy_value(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Fallback when the native clipboard cannot own the selection.
pub fn copy_via_cli(text: &str) -> bool {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let candidates: &[(&str, &[&str])] = if wayland {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    } else {
        &[
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
            ("wl-copy", &[]),
        ]
    };
    candidates.iter().any(|(program, args)| {
        let mut command = std::process::Command::new(program);
        command.args(*args);
        pipe_to_command(command, text, std::time::Duration::from_secs(2))
    })
}

/// Bound both stdin delivery and child exit: a clipboard helper can stall
/// before reading a large config, or keep running after EOF. Reap the child in
/// a worker and terminate its process group if it misses the deadline.
fn pipe_to_command(
    mut command: std::process::Command,
    text: &str,
    timeout: std::time::Duration,
) -> bool {
    use std::io::Write as _;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;
    use std::sync::mpsc;

    let Ok(mut child) = command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let pid = child.id();
    let Some(mut stdin) = child.stdin.take() else {
        let _ = child.kill();
        let _ = child.wait();
        return false;
    };
    let input = text.to_string();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
        let status = child.wait();
        let written = matches!(writer.join(), Ok(Ok(())));
        let _ = tx.send(status.is_ok_and(|status| status.success()) && written);
    });
    match rx.recv_timeout(timeout) {
        Ok(success) => success,
        Err(_) => {
            // The spawned child starts a separate process group. This also
            // closes inherited stdin in descendants so the writer can finish.
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
            let _ = rx.recv_timeout(std::time::Duration::from_millis(200));
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clipboard_helper_times_out_while_stdin_is_blocked() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "sleep 30"]);
        let start = std::time::Instant::now();
        assert!(!pipe_to_command(
            command,
            &"x".repeat(262144),
            std::time::Duration::from_millis(100)
        ));
        assert!(start.elapsed() < std::time::Duration::from_secs(3));
    }

    #[test]
    fn clipboard_helper_requires_successful_delivery_and_exit() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "cat >/dev/null"]);
        assert!(pipe_to_command(
            command,
            &"x".repeat(262144),
            std::time::Duration::from_secs(3)
        ));
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "exit 7"]);
        assert!(!pipe_to_command(
            command,
            "secret config",
            std::time::Duration::from_secs(3)
        ));
    }

    #[test]
    fn trims_accidental_outer_whitespace() {
        assert_eq!(normalize_single_field_copy_value(" abc "), "abc");
        assert_eq!(normalize_single_field_copy_value("\nabc\n"), "abc");
        assert_eq!(normalize_single_field_copy_value("abc\n"), "abc");
        assert_eq!(normalize_single_field_copy_value("  abc\n  "), "abc");
        assert_eq!(normalize_single_field_copy_value("abc\r\n"), "abc");
    }

    #[test]
    fn joins_display_wrapped_fields() {
        assert_eq!(
            normalize_single_field_copy_value("  one\n  two  \n"),
            "one two"
        );
    }
}
