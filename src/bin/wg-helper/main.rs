//! `wg-helper`: the small privileged program the frontends run through sudo or
//! pkexec. It accepts one verb and validated tunnel names, never a path or a
//! shell fragment, and only ever runs tools from fixed system directories.
//!
//! Modules:
//! * [`process`]  — running system tools safely (environment, limits, deadlines)
//! * [`fsutil`]   — root-owned files: lock, private dirs, backups, atomic writes
//! * [`runtime`]  — the running interface (`wg show`/`wg set`, sync settings)
//! * [`commands`] — tunnel verbs
//! * [`firewall`] — the kill switch (nftables table or iptables chain)
//! * [`logging`]  — structured, redacted audit records

mod commands;
mod firewall;
mod fsutil;
mod logging;
mod process;
mod runtime;

#[allow(dead_code)]
#[path = "../../config.rs"]
mod config;
#[path = "../../secrets.rs"]
mod secrets;

use std::path::{Path, PathBuf};
use std::time::Duration;

pub const WG_DIR: &str = "/etc/wireguard";
pub const BACKUP_DIR: &str = "/etc/wireguard/.backup";

fn main() {
    if let Err(e) = run() {
        eprintln!("wg-helper: {e}");
        std::process::exit(2);
    }
}

/// How many tunnel names a verb takes and whether it changes state.
fn verb_shape(verb: &str) -> Option<(usize, bool)> {
    Some(match verb {
        "list" | "active" | "log" => (0, false),
        "read" | "dump" | "is-enabled" | "showconf" | "killswitch-status" => (1, false),
        "up" | "down" | "save" | "delete" | "enable" | "disable" | "sync" | "persist"
        | "killswitch-enable" | "killswitch-disable" | "killswitch-repair" => (1, true),
        "rename" => (2, true),
        _ => return None,
    })
}

fn run() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let verb = args.next().ok_or("no verb")?;
    let names: Vec<_> = args.collect();
    let (expected, mutating) = verb_shape(&verb).ok_or_else(|| format!("unknown verb: {verb}"))?;
    if names.len() != expected {
        return Err(format!("{verb} requires {expected} tunnel name(s)"));
    }
    for name in &names {
        validate_name(name)?;
    }
    if !mutating {
        return dispatch(&verb, &names);
    }
    // Both frontends share this lock. Check names first, and keep read-only
    // operations independent of directory creation or a long-running mutation.
    let _lock = fsutil::lock_helper_state(Path::new(WG_DIR), Duration::from_secs(2))?;
    let result = dispatch(&verb, &names);
    logging::outcome(&verb, &names, &result);
    result
}

fn dispatch(verb: &str, names: &[String]) -> Result<(), String> {
    let name = names.first().map(String::as_str).unwrap_or_default();
    match verb {
        "list" => commands::list(),
        "active" => commands::active(),
        "log" => commands::show_log(),
        "read" => commands::read_config(name),
        "dump" => commands::dump(name),
        "up" => commands::up(name),
        "down" => commands::down(name),
        "save" => commands::save(name),
        "delete" => commands::delete(name),
        "rename" => commands::rename(name, &names[1]),
        "enable" => commands::set_autostart(name, true),
        "disable" => commands::set_autostart(name, false),
        "is-enabled" => commands::is_enabled(name),
        "sync" => commands::sync_running(name),
        "showconf" => commands::showconf(name),
        "persist" => commands::persist(name),
        "killswitch-status" => firewall::status(name),
        "killswitch-enable" => firewall::enable(name),
        "killswitch-disable" => firewall::disable(name),
        "killswitch-repair" => firewall::repair(name),
        _ => Err(format!("unknown verb: {verb}")),
    }
}

pub fn name_ok(name: &str) -> bool {
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

pub fn conf_path(name: &str) -> PathBuf {
    Path::new(WG_DIR).join(format!("{name}.conf"))
}

#[cfg(test)]
mod testutil {
    use std::path::PathBuf;

    pub struct TempDir(pub PathBuf);
    impl TempDir {
        pub fn new() -> Self {
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
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_verb_has_a_fixed_shape_and_mutations_take_the_lock() {
        for verb in ["list", "active", "log"] {
            assert_eq!(verb_shape(verb), Some((0, false)), "{verb}");
        }
        for verb in [
            "read",
            "dump",
            "is-enabled",
            "showconf",
            "killswitch-status",
        ] {
            assert_eq!(verb_shape(verb), Some((1, false)), "{verb}");
        }
        for verb in [
            "up",
            "down",
            "save",
            "delete",
            "enable",
            "disable",
            "sync",
            "persist",
            "killswitch-enable",
            "killswitch-disable",
            "killswitch-repair",
        ] {
            assert_eq!(verb_shape(verb), Some((1, true)), "{verb}");
        }
        assert_eq!(verb_shape("rename"), Some((2, true)));
        for verb in ["", "sh", "UP", "killswitch", "--help"] {
            assert_eq!(verb_shape(verb), None, "{verb}");
        }
    }

    #[test]
    fn tunnel_name_validation() {
        for name in [
            "wg0",
            "home-vpn",
            "a.b_c",
            "a12345678901234",
            "a",
            "Peer",
            "Interface",
        ] {
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
            ".a",
            "name.conf",
            "abcdefghijklmnop",
        ] {
            assert!(!name_ok(name), "{name}");
        }
    }

    #[test]
    fn paths_stay_under_wireguard_dir() {
        assert_eq!(conf_path("wg0"), Path::new("/etc/wireguard/wg0.conf"));
        assert!(conf_path("home-vpn").starts_with(WG_DIR));
    }

    #[test]
    fn redacts_secret_lines() {
        let text = "PrivateKey = abc\nok\nPresharedKey = def";
        let out = secrets::redact_config(text);
        assert!(out.contains("PrivateKey = <redacted>"));
        assert!(out.contains("PresharedKey = <redacted>"));
        assert!(out.contains("ok"));
        assert!(!out.contains("abc"));
        assert!(!out.contains("def"));
    }
}
