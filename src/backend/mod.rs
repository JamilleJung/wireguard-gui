//! Talks to WireGuard through the privileged `wg-helper` binary.
//!
//! * [`helper`]: locating and invoking the helper (typed [`HelperError`]s).
//! * [`status`]: the tunnel detail model and live `wg show dump` parsing.
//! * [`health`]: per-peer and per-interface connection health.
//! * [`verify`]: checking what a mutation really did.
//! * [`backup`]: backups, exports and restore with rollback.
//! * [`qr`]: QR-code import. [`diagnostics`]: the connection checklist.

pub mod backup;
pub mod diagnostics;
pub mod error;
pub mod health;
pub mod helper;
pub mod qr;
pub mod status;
pub mod verify;

use std::collections::HashSet;

pub use backup::{
    RestoreReport, backup_dir, create_backup, delete_backup, export_backup_to, export_zip,
    fmt_size, fmt_time, list_backups, restore_backup, save_text_to,
};
pub use diagnostics::diagnose_report;
pub use error::HelperError;
pub use health::{Health, HealthTracker};
pub use helper::{helper_path, init};
pub use qr::decode_qr;
pub use status::{Detail, KillSwitch, Peer, fmt_bytes};
pub use verify::{Verdict, Verified};

use helper::{helper, wg_output};
use verify::{ChangeText, verify_change};

use crate::{config, doctor, secrets, validation};

pub struct Tunnel {
    pub name: String,
    pub active: bool,
}

pub(crate) fn saved_tunnel_names() -> Result<Vec<String>, HelperError> {
    let output = helper(&["list"], None)?;
    let mut seen = HashSet::new();
    let mut names = Vec::new();
    for name in output.lines().filter(|name| !name.is_empty()) {
        validate_tunnel_name(name).map_err(HelperError::InvalidOutput)?;
        if !seen.insert(name) {
            return Err(HelperError::InvalidOutput(
                "wg-helper returned duplicate tunnel names.".into(),
            ));
        }
        names.push(name.to_string());
    }
    Ok(names)
}

fn active_names() -> Result<HashSet<String>, HelperError> {
    Ok(helper(&["active"], None)?
        .split_whitespace()
        .map(str::to_string)
        .collect())
}

pub fn try_list_tunnels() -> Result<Vec<Tunnel>, HelperError> {
    let names = saved_tunnel_names()?;
    let active = active_names()?;
    Ok(names
        .into_iter()
        .map(|name| Tunnel {
            active: active.contains(&name),
            name,
        })
        .collect())
}

/// Whether the interface is currently up.
pub fn is_up(name: &str) -> Result<bool, HelperError> {
    Ok(active_names()?.contains(name))
}

/// Whether a saved config with this name exists.
pub fn exists(name: &str) -> Result<bool, HelperError> {
    Ok(saved_tunnel_names()?.iter().any(|n| n == name))
}

/// A collision-free tunnel name based on `base`: returns `base`, else
/// `base-2`, `base-3`, … (kept within the 15-char interface-name limit).
pub fn unique_name(base: &str) -> Result<String, String> {
    let existing: HashSet<_> = saved_tunnel_names()?.into_iter().collect();
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

pub fn read_config(name: &str) -> Result<String, HelperError> {
    helper(&["read", name], None)
}

pub fn validate_tunnel_name(name: &str) -> Result<(), String> {
    validation::validate_tunnel_name(name)
}

pub fn save_config(name: &str, content: &str) -> Result<(), HelperError> {
    helper(&["save", name], Some(content)).map(|_| ())
}

pub fn rename_config(old: &str, new: &str, content: &str) -> Result<(), HelperError> {
    helper(&["rename", old, new], Some(content)).map(|_| ())
}

pub fn activate(name: &str) -> Result<(), HelperError> {
    helper(&["up", name], None).map(|_| ())
}

pub fn deactivate(name: &str) -> Result<(), HelperError> {
    helper(&["down", name], None).map(|_| ())
}

pub fn delete(name: &str) -> Result<(), HelperError> {
    helper(&["delete", name], None).map(|_| ())
}

fn plain(error: &HelperError) -> String {
    error.to_string()
}

/// Bring a tunnel up and confirm the interface really is up.
pub fn activate_verified(name: &str) -> Verified {
    let text = ChangeText {
        done: format!("Activated {name}"),
        attempt: "Activation",
        expected: format!("{name} is up"),
        unexpected: format!("{name} is not up"),
    };
    verify_change(
        &text,
        activate(name),
        |e| doctor::friendly_error(&e.to_string()),
        || is_up(name),
    )
}

/// Take a tunnel down and confirm the interface is gone.
pub fn deactivate_verified(name: &str) -> Verified {
    let text = ChangeText {
        done: format!("Deactivated {name}"),
        attempt: "Deactivation",
        expected: format!("{name} is down"),
        unexpected: format!("{name} is still up"),
    };
    verify_change(&text, deactivate(name), plain, || is_up(name).map(|up| !up))
}

/// Delete a tunnel and confirm its config file is gone.
pub fn delete_verified(name: &str) -> Verified {
    let text = ChangeText {
        done: format!("Removed {name}"),
        attempt: "Removal",
        expected: format!("{name} is removed"),
        unexpected: format!("{name} still exists"),
    };
    verify_change(&text, delete(name), plain, || exists(name).map(|e| !e))
}

/// Rename (writing `content`) and confirm the new file exists and the old
/// name is gone.
pub fn rename_verified(old: &str, new: &str, content: &str) -> Verified {
    let text = ChangeText {
        done: format!("Renamed {old} → {new}"),
        attempt: "Rename",
        expected: format!("{new} exists and {old} is gone"),
        unexpected: format!("the files were not renamed ({old} → {new})"),
    };
    verify_change(&text, rename_config(old, new, content), plain, || {
        let names = saved_tunnel_names()?;
        Ok(names.iter().any(|n| n == new) && !names.iter().any(|n| n == old))
    })
}

/// Save and, when the helper's answer is lost, check whether the file now
/// holds exactly the submitted draft.
pub fn save_verified(name: &str, content: &str) -> Verified {
    let text = ChangeText {
        done: format!("Saved {name}"),
        attempt: "Save",
        expected: format!("the saved {name} matches your draft"),
        unexpected: format!("{name} was not saved"),
    };
    let result = save_config(name, content);
    if result.is_ok() {
        return Verified {
            verdict: Verdict::Confirmed,
            message: text.done,
            outcome_was_unknown: false,
        };
    }
    verify_change(&text, result, plain, || {
        Ok(exists(name)? && read_config(name)? == content)
    })
}

/// Reconnect (down + up) so a saved config fully applies.
pub fn reconnect_verified(name: &str) -> Verified {
    let down = deactivate_verified(name);
    if !down.applied() {
        return down;
    }
    let mut up = activate_verified(name);
    if up.applied() {
        up.message = format!("Reconnected {name}");
    }
    up
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
    let live = status::parse_dump(&dump);

    // Active: use the active interfaces list (fast, reliable), fall back to dump.
    let active = is_up(name).unwrap_or(!dump.trim().is_empty());

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

    let now = status::now_epoch();
    let peers = status::merge_peers(parsed.peers, live.as_ref(), now);
    let (rx_bytes, tx_bytes, _) = status::live_totals(live.as_ref(), now);

    Detail {
        name: name.to_string(),
        active,
        autostart: is_autostart(name),
        killswitch: killswitch_status(name),
        public_key,
        listen_port,
        addresses: parsed.address.unwrap_or_default(),
        dns: parsed.dns.unwrap_or_default(),
        peers,
        rx_bytes,
        tx_bytes,
    }
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
/// (`sync`). Only wg-level fields take effect; Address/DNS/MTU/Table changes
/// still need a full reconnect, and the helper refuses a FwMark change.
pub fn sync_running(name: &str) -> Result<(), HelperError> {
    helper(&["sync", name], None).map(|_| ())
}

/// The helper refused a live sync because the change needs a reconnect
/// (e.g. a different FwMark).
pub fn sync_needs_reconnect(error: &HelperError) -> bool {
    error
        .stderr()
        .is_some_and(|stderr| stderr.to_ascii_lowercase().contains("reconnect"))
}

/// The live running wg-level config (`wg showconf`).
pub fn running_config(name: &str) -> Result<String, HelperError> {
    helper(&["showconf", name], None)
}

/// Save the live running state back to the `.conf` (`wg-quick save`).
pub fn persist_live(name: &str) -> Result<(), HelperError> {
    helper(&["persist", name], None).map(|_| ())
}

/// Whether `wg-quick@<name>` is enabled to start on boot.
pub fn is_autostart(name: &str) -> bool {
    helper(&["is-enabled", name], None)
        .map(|s| s.trim() == "enabled")
        .unwrap_or(false)
}

/// Enable/disable starting the tunnel on boot.
pub fn set_autostart(name: &str, on: bool) -> Result<(), HelperError> {
    helper(&[if on { "enable" } else { "disable" }, name], None).map(|_| ())
}

/// The helper-installed firewall kill switch state for this tunnel.
pub fn killswitch_status(name: &str) -> KillSwitch {
    KillSwitch::parse(helper(&["killswitch-status", name], None))
}

/// Enable/disable the helper-installed firewall kill switch. This is intentionally
/// not persistent: it uses standard firewall rules and no daemon.
pub fn set_killswitch(name: &str, on: bool) -> Result<(), HelperError> {
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

/// Reinstall a lost kill switch from the live interface, then re-check it.
pub fn repair_killswitch(name: &str) -> (Result<(), HelperError>, KillSwitch) {
    let result = helper(&["killswitch-repair", name], None).map(|_| ());
    (result, killswitch_status(name))
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
pub(crate) mod tests {
    use super::helper::{
        ESC, Escalation, HELPER, helper_output, helper_output_timeout, invoke, polkit_subject,
        read_only_helper_verb, trusted_helper,
    };
    use super::*;
    use std::path::PathBuf;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    // A syntactically valid 44-char base64 WireGuard key (43 chars + '=').
    pub(crate) const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

    pub(crate) fn scratch_dir() -> PathBuf {
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

    /// A fake helper script for the invocation-path tests.
    fn fake_helper(dir: &std::path::Path, body: &str, mode: u32) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join("wg-helper");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn offline_saved_configs_do_not_depend_on_live_status_fixture() {
        let Some(path) = std::env::var_os("WG_TEST_SAVED_CONFIG_HELPER") else {
            return;
        };
        HELPER
            .set(Some(PathBuf::from(&path).to_string_lossy().into_owned()))
            .unwrap();
        assert!(ESC.set(Escalation::Direct).is_ok());
        assert!(try_list_tunnels().is_err());
        assert_eq!(unique_name("wg0").unwrap(), "wg0-2");
        assert_eq!(backup::read_all_configs().unwrap().len(), 1);
        let dest = PathBuf::from(path)
            .parent()
            .unwrap()
            .join("offline-backup.zip");
        assert_eq!(export_zip(&dest).unwrap(), 1);
        assert!(dest.is_file());
    }

    #[test]
    fn offline_backup_and_name_checks_survive_a_failed_active_query() {
        use std::os::unix::process::CommandExt;
        let dir = scratch_dir();
        let helper = fake_helper(
            &dir,
            &format!(
                "case \"$1\" in\nlist) printf 'wg0\\n';;\nactive) echo 'active query failed' >&2; exit 2;;\nread) printf '[Interface]\\nPrivateKey = {KEY}\\n';;\n*) exit 2;;\nesac"
            ),
            0o700,
        );
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "backend::tests::offline_saved_configs_do_not_depend_on_live_status_fixture",
                "--nocapture",
            ])
            .env("WG_TEST_SAVED_CONFIG_HELPER", &helper)
            .process_group(0)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output =
            helper_output_timeout(command.spawn().unwrap(), None, Duration::from_secs(5)).unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(dir.join("offline-backup.zip").is_file());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn helper_invocation_reports_timeouts_as_unknown_outcomes() {
        let dir = scratch_dir();
        let helper = fake_helper(&dir, "sleep 30", 0o700);
        let start = Instant::now();
        let error = invoke(
            Escalation::Direct,
            &helper,
            &["up", "wg0"],
            None,
            Duration::from_millis(200),
        )
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(3));
        assert_eq!(error, HelperError::TimedOut);
        assert!(error.outcome_unknown());
        assert_eq!(error.to_string(), "Helper operation timed out.");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn helper_invocation_reports_permission_denied_and_busy_lock() {
        let dir = scratch_dir();
        // Not executable: EACCES even for root.
        let helper = fake_helper(&dir, "exit 0", 0o600);
        let error = invoke(
            Escalation::Direct,
            &helper,
            &["list"],
            None,
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(error, HelperError::Spawn(_)), "{error:?}");
        assert!(error.to_string().contains("ermission denied"), "{error}");
        let helper = fake_helper(
            &dir,
            "echo 'wg-helper: another helper operation is busy; retry when it completes' >&2; exit 2",
            0o700,
        );
        let error = invoke(
            Escalation::Direct,
            &helper,
            &["save", "wg0"],
            Some("[Interface]\n"),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(error.is_busy(), "{error:?}");
        assert!(!error.outcome_unknown());
        let helper = fake_helper(&dir, "echo \"PrivateKey = SECRET\" >&2; exit 3", 0o700);
        let error = invoke(
            Escalation::Direct,
            &helper,
            &["save", "wg0"],
            Some("x"),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(matches!(error, HelperError::Failed { .. }));
        assert!(!error.to_string().contains("SECRET"), "{error}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn helper_invocation_detects_a_helper_that_ignores_its_payload() {
        let dir = scratch_dir();
        // Exits successfully without reading stdin: the save outcome is unknown.
        let helper = fake_helper(&dir, "exec 0<&-; exit 0", 0o700);
        let error = invoke(
            Escalation::Direct,
            &helper,
            &["save", "wg0"],
            Some(&"x".repeat(1024 * 1024)),
            Duration::from_secs(5),
        )
        .unwrap_err();
        assert_eq!(error, HelperError::IncompleteInput);
        assert!(error.outcome_unknown());
        std::fs::remove_dir_all(dir).unwrap();
    }

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
        assert!(
            helper_output(child, None)
                .unwrap_err()
                .to_string()
                .contains("4 MiB")
        );
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
        let started = Instant::now();
        let error = helper_output_timeout(
            cmd.spawn().unwrap(),
            Some("x".repeat(1024 * 1024)),
            Duration::from_millis(100),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
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
        let started = Instant::now();
        assert!(
            helper_output_timeout(cmd.spawn().unwrap(), None, Duration::from_millis(100))
                .unwrap_err()
                .is_timeout()
        );
        assert!(started.elapsed() < Duration::from_secs(2));
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
            "sync",
            "killswitch-enable",
            "killswitch-repair",
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

    #[test]
    fn sync_refusal_that_needs_a_reconnect_is_recognised() {
        let refusal = HelperError::from_failure(
            "wg-helper sync wg0".into(),
            "FwMark changed; reconnect the tunnel to apply it".into(),
        );
        assert!(sync_needs_reconnect(&refusal));
        assert!(!sync_needs_reconnect(&HelperError::TimedOut));
        assert!(!sync_needs_reconnect(&HelperError::from_failure(
            "wg-helper sync wg0".into(),
            "wg syncconf failed".into()
        )));
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
        assert!(unique_name_with("wg0", |_| true).is_err());
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
