//! Tunnel verbs: listing, reading, activation, editing and start-on-boot.
//!
//! Every mutation checks the resulting state instead of trusting an exit
//! status, and a timeout — whose outcome is unknown — is reported together
//! with the state actually observed afterwards.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::fsutil::{
    atomic_write, atomic_write_mode, backup_existing, check_private_dir, ensure_wg_dir,
    open_config, read_config_text, read_stdin, sync_dir,
};
use crate::process::{command_output, command_result, have_tool, is_timeout};
use crate::runtime::{interface_active, preserve_runtime_settings, runtime_fwmark};
use crate::{WG_DIR, conf_path, config, firewall, logging, name_ok, secrets};

pub fn validate_config_text(text: &str) -> Result<(), String> {
    config::validate_basic_wireguard_config(text)?;
    if secrets::config_runs_scripts(text) {
        return Err(
            "script hooks are not allowed in managed configs; they would run as root".into(),
        );
    }
    Ok(())
}

pub fn list() -> Result<(), String> {
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

pub fn active() -> Result<(), String> {
    let out = command_output("wg", &["show", "interfaces"], None, Duration::from_secs(5))?;
    print!("{out}");
    Ok(())
}

pub fn read_config(name: &str) -> Result<(), String> {
    check_private_dir(Path::new(WG_DIR))?;
    print!("{}", read_config_text(&conf_path(name))?);
    Ok(())
}

pub fn dump(name: &str) -> Result<(), String> {
    let out = command_output("wg", &["show", name, "dump"], None, Duration::from_secs(5))?;
    print!("{out}");
    Ok(())
}

fn describe(active: Result<bool, String>) -> String {
    match active {
        Ok(true) => "the interface is up".into(),
        Ok(false) => "the interface is down".into(),
        Err(error) => format!("its state could not be read: {error}"),
    }
}

/// Combine a command result with the state observed afterwards.
fn settle(
    action: &str,
    name: &str,
    result: Result<(), String>,
    want_active: bool,
    observed: Result<bool, String>,
) -> Result<(), String> {
    match (result, observed) {
        (Ok(()), Ok(active)) if active == want_active => Ok(()),
        (Ok(()), observed) => Err(format!(
            "{action} {name} reported success but {}",
            describe(observed)
        )),
        (Err(error), observed) if is_timeout(&error) => Err(format!(
            "{error}; outcome checked afterwards: {}",
            describe(observed)
        )),
        (Err(error), _) => Err(error),
    }
}

pub fn up(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    logging::event("up", Some(name), &[]);
    let result =
        command_output("wg-quick", &["up", name], None, Duration::from_secs(45)).map(|_| ());
    settle("activate", name, result, true, interface_active(name))
}

pub fn finish_down(
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

pub fn down(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    logging::event("down", Some(name), &[]);
    let stopped =
        command_output("wg-quick", &["down", name], None, Duration::from_secs(30)).map(|_| ());
    let stopped = match stopped {
        Err(error) if is_timeout(&error) => Err(format!(
            "{error}; outcome checked afterwards: {}",
            describe(interface_active(name))
        )),
        other => other,
    };
    finish_down(stopped, interface_active(name), || firewall::disable(name))
}

fn stop_for_mutation(name: &str) -> Result<(), String> {
    if interface_active(name)? {
        let _ = open_config(&conf_path(name))?;
        command_output("wg-quick", &["down", name], None, Duration::from_secs(30))?;
        if interface_active(name)? {
            return Err(format!("interface {name} is still active"));
        }
    }
    firewall::disable(name)
}

pub fn save(name: &str) -> Result<(), String> {
    let content = read_stdin()?;
    validate_config_text(&content)?;
    ensure_wg_dir()?;
    backup_existing(name)?;
    atomic_write(&conf_path(name), &content)?;
    // Confirm what landed on disk is what was sent.
    if read_config_text(&conf_path(name))? != content {
        return Err(format!(
            "saved config for {name} does not match what was written"
        ));
    }
    Ok(())
}

pub fn autostart_unit(name: &str) -> String {
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
            secrets::redact_config(&String::from_utf8_lossy(&out.stderr))
        )),
    }
}

pub fn delete(name: &str) -> Result<(), String> {
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
    if path.try_exists().map_err(|e| e.to_string())? {
        return Err(format!("{} still exists after delete", path.display()));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RenameStep {
    StopAndCleanup,
    EnableNew,
    DisableOld,
    EnableOld,
    DisableNew,
}

pub fn rename_files(
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
    if src.try_exists().unwrap_or(true) || !dst.try_exists().unwrap_or(false) {
        return Err(format!(
            "rename did not complete: {} and {} are not in the expected state",
            src.display(),
            dst.display()
        ));
    }
    Ok(())
}

pub fn rename(old: &str, new: &str) -> Result<(), String> {
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
    })
}

pub fn set_autostart(name: &str, on: bool) -> Result<(), String> {
    if on {
        let _ = open_config(&conf_path(name))?;
    }
    if !have_tool("systemctl") {
        return Err("start-on-boot needs systemd (systemctl not found)".into());
    }
    command_output(
        "systemctl",
        &[if on { "enable" } else { "disable" }, &autostart_unit(name)],
        None,
        Duration::from_secs(15),
    )?;
    if autostart_enabled(name)? != on {
        return Err(format!(
            "start-on-boot for {name} is still {} after the change",
            if on { "off" } else { "on" }
        ));
    }
    Ok(())
}

pub fn is_enabled(name: &str) -> Result<(), String> {
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

/// Apply the saved config to the running interface without dropping it.
///
/// `wg syncconf` resets interface fields that the file omits; see
/// [`preserve_runtime_settings`]. The fwmark is re-read afterwards and the kill
/// switch re-checked, so a sync can never leave the transport unmarked while
/// the firewall still expects the old mark.
pub fn sync_running(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    if !interface_active(name)? {
        return Err(format!("{name} is not running; nothing to sync"));
    }
    let stripped = command_output("wg-quick", &["strip", name], None, Duration::from_secs(15))
        .map_err(|_| format!("wg-quick strip failed for {name}"))?;
    if stripped.trim().is_empty() {
        return Err(format!("wg-quick strip produced no output for {name}"));
    }
    let mark = runtime_fwmark(name)?;
    let port = crate::runtime::runtime_listen_port(name)?;
    let merged = preserve_runtime_settings(&stripped, mark, port)?;
    let expected = crate::runtime::interface_settings(&merged)?
        .fwmark
        .unwrap_or(mark);
    command_output(
        "wg",
        &["syncconf", name, "/dev/stdin"],
        Some(&merged),
        Duration::from_secs(15),
    )?;
    let after = runtime_fwmark(name)?;
    if after != expected {
        return Err(format!(
            "fwmark of {name} changed during sync (expected {}, found {}); reconnect the tunnel",
            expected.map_or("off".into(), |m| m.to_string()),
            after.map_or("off".into(), |m| m.to_string())
        ));
    }
    firewall::reconcile_after_sync(name)
}

pub fn showconf(name: &str) -> Result<(), String> {
    let out = command_output("wg", &["showconf", name], None, Duration::from_secs(10))?;
    print!("{out}");
    Ok(())
}

pub fn persist(name: &str) -> Result<(), String> {
    let _ = open_config(&conf_path(name))?;
    backup_existing(name)?;
    command_output("wg-quick", &["save", name], None, Duration::from_secs(30)).map(|_| ())
}

pub fn log_report_with(
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
        secrets::redact_config(&activity),
        secrets::redact_config(&quick)
    );
    if text.len() as u64 > crate::process::MAX_COMMAND_OUTPUT {
        return Err("journal output exceeds the size limit".into());
    }
    Ok(text)
}

pub fn show_log() -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    const KEY: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopo=";

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
    fn actions_are_judged_by_the_observed_state() {
        assert!(settle("activate", "wg0", Ok(()), true, Ok(true)).is_ok());
        let error = settle("activate", "wg0", Ok(()), true, Ok(false)).unwrap_err();
        assert!(
            error.contains("reported success but the interface is down"),
            "{error}"
        );
        let error = settle(
            "activate",
            "wg0",
            Err("wg-quick up wg0: timed out".into()),
            true,
            Ok(true),
        )
        .unwrap_err();
        assert!(
            error.contains("outcome checked afterwards: the interface is up"),
            "{error}"
        );
        assert_eq!(
            settle("activate", "wg0", Err("bad key".into()), true, Ok(false)),
            Err("bad key".into())
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
    fn config_validation() {
        let valid = format!(
            "[Interface]\nPrivateKey = {KEY}\nAddress = 10.0.0.2/32\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\n"
        );
        assert!(validate_config_text(&valid).is_ok());
        assert!(validate_config_text(&format!("[Interface]\nPrivateKey = {KEY}\n")).is_ok());
        assert!(validate_config_text("[Interface]\n").is_err());
    }

    #[test]
    fn config_validation_edge_cases() {
        assert!(validate_config_text("").is_err());
        assert!(validate_config_text("# just a comment").is_err());
        assert!(validate_config_text("[Interface]\nAddress = 10.0.0.1/24\n").is_err());
        assert!(
            validate_config_text(&format!(
                "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nAllowedIPs = 0.0.0.0/0\n"
            ))
            .is_err()
        );
        assert!(
            validate_config_text(&format!(
                "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\n"
            ))
            .is_err()
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
        let cfg = format!(
            "[Interface]\nPrivateKey = {KEY}\n\n[Peer]\nPublicKey = {KEY}\nAllowedIPs = 0.0.0.0/0\nPostUp = id\n"
        );
        assert!(validate_config_text(&cfg).is_err());
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
}
