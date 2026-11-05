//! Opt-in structured logging for the frontend.
//!
//! Set `WG_GUI_LOG=/path/to/file` to append one `key=value` line per event.
//! Every value passes through [`secrets::redact_config`] and key-shaped
//! base64 tokens are masked, so private and preshared keys never reach the
//! file. The file is created 0600 and never followed through a symlink.

use std::io::Write as _;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::secrets;

static SINK: OnceLock<Option<Mutex<std::fs::File>>> = OnceLock::new();

fn open_sink(path: &std::path::Path) -> Option<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .ok()
        .filter(|f| f.metadata().is_ok_and(|m| m.is_file()))
}

fn sink() -> Option<&'static Mutex<std::fs::File>> {
    SINK.get_or_init(|| {
        let path = std::env::var_os("WG_GUI_LOG").filter(|p| !p.is_empty())?;
        open_sink(std::path::Path::new(&path)).map(Mutex::new)
    })
    .as_ref()
}

/// True for a token shaped like a WireGuard key (44 base64 chars ending '=').
fn key_shaped(token: &str) -> bool {
    token.len() == 44
        && token.ends_with('=')
        && token[..43]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'/')
}

/// Redact secrets and make the value safe for a single `key=value` line.
pub fn sanitize(value: &str) -> String {
    let redacted = secrets::redact_config(value);
    // Mask every maximal base64 run shaped like a key, wherever it appears.
    let is_b64 = |c: char| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=');
    let mut masked = String::with_capacity(redacted.len());
    let mut run = String::new();
    for c in redacted.chars().chain(std::iter::once('\n')) {
        if is_b64(c) {
            run.push(c);
            continue;
        }
        // A run may carry a leading "key=" style prefix; check its tail.
        if run.len() >= 44 && key_shaped(&run[run.len() - 44..]) {
            masked.push_str(&run[..run.len() - 44]);
            masked.push_str("<key>");
        } else {
            masked.push_str(&run);
        }
        run.clear();
        masked.push(c);
    }
    masked.pop();
    let joined = masked.split_whitespace().collect::<Vec<_>>().join(" ");
    let needs_quotes = joined.is_empty() || joined.contains([' ', '=', '"']);
    let escaped = joined.replace('\\', "\\\\").replace('"', "\\\"");
    if needs_quotes {
        format!("\"{escaped}\"")
    } else {
        escaped
    }
}

/// Format one log line (without the timestamp prefix).
pub fn format_line(event: &str, fields: &[(&str, &str)]) -> String {
    let mut line = format!("event={}", sanitize(event));
    for (key, value) in fields {
        line.push_str(&format!(" {key}={}", sanitize(value)));
    }
    line
}

/// Append an event when logging is enabled; a no-op otherwise.
pub fn log(event: &str, fields: &[(&str, &str)]) {
    let Some(sink) = sink() else { return };
    write_line(sink, event, fields);
}

fn write_line(sink: &Mutex<std::fs::File>, event: &str, fields: &[(&str, &str)]) {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let line = format!("ts={ts} {}\n", format_line(event, fields));
    if let Ok(mut file) = sink.lock() {
        let _ = file.write_all(line.as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVATE: &str = "yAnz5TF+lXXJte14tji3zlMNq+hd2rYUIgJBgB3fBmk=";
    const PRESHARED: &str = "FpCyhws9cxwWoV4xELtfJvjJN+zQVRPISllRWgeopVE=";

    #[test]
    fn lines_are_key_value_and_quoted_when_needed() {
        assert_eq!(
            format_line("op", &[("phase", "running")]),
            "event=op phase=running"
        );
        assert_eq!(
            format_line("op", &[("label", "Saving tunnel")]),
            "event=op label=\"Saving tunnel\""
        );
        assert_eq!(format_line("op", &[("x", "")]), "event=op x=\"\"");
        assert!(!format_line("op", &[("x", "a\nb")]).contains('\n'));
    }

    #[test]
    fn private_and_preshared_keys_never_appear_in_the_log_file() {
        let dir = crate::backend::tests::scratch_dir();
        let path = dir.join("gui.log");
        let sink = Mutex::new(open_sink(&path).unwrap());
        let config =
            format!("[Interface]\nPrivateKey = {PRIVATE}\n[Peer]\nPresharedKey = {PRESHARED}\n");
        write_line(&sink, "helper", &[("stdin", &config)]);
        write_line(
            &sink,
            "helper",
            &[("error", &format!("Invalid private key: {PRIVATE}"))],
        );
        write_line(
            &sink,
            "helper",
            &[("error", &format!("journal said {PRESHARED} once"))],
        );
        write_line(&sink, "helper", &[("error", &format!("key=\"{PRIVATE}\""))]);
        drop(sink);
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 4, "{text}");
        assert!(!text.contains(PRIVATE), "{text}");
        assert!(!text.contains(PRESHARED), "{text}");
        assert!(text.contains("<redacted"), "{text}");
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(path.metadata().unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn log_file_is_never_opened_through_a_symlink() {
        let dir = crate::backend::tests::scratch_dir();
        let real = dir.join("real");
        std::fs::write(&real, "").unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(open_sink(&link).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
