//! Background operations: what runs on worker threads for each user action.
//! Every mutation verifies the real result (see `backend::verify`) rather
//! than trusting the helper's exit status.

use std::collections::HashMap;
use std::time::Duration;

use crate::backend::{self, HelperError, KillSwitch, RestoreReport, Verdict, Verified};
use crate::draft::FailureKind;
use crate::ops::{Outcome, TaskOutcome};
use crate::safe_file;

/// User-selected configs follow the helper's 1 MiB limit.
pub const MAX_CONFIG: u64 = safe_file::MIB;

/// Read a user-chosen config without blocking on special files.
pub fn read_config_file(path: &std::path::Path) -> Result<String, String> {
    safe_file::read_text(path, "Config", MAX_CONFIG).map_err(String::from)
}

fn stem_of(path: &std::path::Path) -> String {
    path.file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| "tunnel".into())
}

/// One file for review in the editor: (unique name, content).
pub fn read_import(path: &std::path::Path) -> Result<(String, String), String> {
    let content = read_config_file(path)?;
    backend::unique_name(&stem_of(path)).map(|name| (name, content))
}

/// A QR image for review in the editor: (unique name, decoded config).
pub fn read_qr_import(path: &std::path::Path) -> Result<(String, String), String> {
    let text = backend::decode_qr(path)?;
    backend::unique_name(&stem_of(path)).map(|name| (name, text))
}

pub struct ImportSummary {
    pub imported: usize,
    pub skipped: usize,
    pub last: Option<String>,
    pub first_error: Option<String>,
    pub unknown: bool,
}

impl ImportSummary {
    pub fn message(&self) -> String {
        let mut message = format!(
            "Imported {} tunnel(s), {} skipped",
            self.imported, self.skipped
        );
        if let Some(error) = &self.first_error
            && (self.imported == 0 || self.unknown)
        {
            message.push_str(&format!(": {error}"));
        }
        message
    }
}

impl TaskOutcome for ImportSummary {
    fn outcome(&self) -> Outcome {
        if self.unknown {
            Outcome::Unknown
        } else if self.skipped == 0 {
            Outcome::Succeeded
        } else {
            Outcome::Failed
        }
    }
    fn reconciled(&self) -> bool {
        true
    }
}

/// Import several files directly (invalid ones are skipped).
pub fn import_files(files: Vec<std::path::PathBuf>) -> ImportSummary {
    let mut summary = ImportSummary {
        imported: 0,
        skipped: 0,
        last: None,
        first_error: None,
        unknown: false,
    };
    for path in files {
        let result = (|| {
            let content = read_config_file(&path)?;
            backend::validate_config(&content)?;
            let name = backend::unique_name(&stem_of(&path))?;
            let verified = backend::save_verified(&name, &content);
            if verified.outcome_was_unknown {
                summary.unknown |= !verified.applied();
            }
            verified.into_result().map(|_| name)
        })();
        match result {
            Ok(name) => {
                summary.imported += 1;
                summary.last = Some(name);
            }
            Err(error) => {
                summary.skipped += 1;
                summary.first_error.get_or_insert(error);
            }
        }
    }
    summary
}

#[derive(Clone, Debug)]
pub struct SaveRequest {
    /// Name the editor was opened with ("" for a new tunnel).
    pub original: String,
    pub name: String,
    pub text: String,
    pub activate_after: bool,
}

/// What happened to the running interface after the file was saved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SyncState {
    /// The tunnel is not running (or was just activated/renamed).
    NotNeeded,
    Applied,
    /// The file was saved but the running interface was NOT updated.
    Failed {
        error: String,
        needs_reconnect: bool,
    },
}

#[derive(Debug)]
pub struct SaveSuccess {
    pub name: String,
    pub message: String,
    pub sync: SyncState,
}

#[derive(Debug)]
pub struct SaveError {
    pub kind: FailureKind,
    pub message: String,
}

impl<T> TaskOutcome for Result<T, SaveError> {
    fn outcome(&self) -> Outcome {
        match self {
            Ok(_) => Outcome::Succeeded,
            Err(e) if e.kind == FailureKind::Unknown => Outcome::Unknown,
            Err(_) => Outcome::Failed,
        }
    }
    fn reconciled(&self) -> bool {
        true
    }
}

fn save_error(verified: Verified) -> SaveError {
    SaveError {
        kind: if verified.verdict == Verdict::Unknown {
            FailureKind::Unknown
        } else {
            FailureKind::Save
        },
        message: verified.message,
    }
}

/// Message for a saved file whose running interface could not be updated.
pub fn sync_failure_message(name: &str, error: &str, needs_reconnect: bool) -> String {
    if needs_reconnect {
        format!(
            "Saved {name}, but this change can only take effect after a reconnect; the saved file and the running interface now differ."
        )
    } else {
        format!(
            "Saved {name}, but applying it to the running interface failed ({error}); the saved file and the running interface now differ."
        )
    }
}

/// Apply the saved file to a running tunnel and describe the result.
pub fn sync_after_save(name: &str) -> SyncState {
    match backend::sync_running(name) {
        Ok(()) => SyncState::Applied,
        Err(error) => SyncState::Failed {
            needs_reconnect: backend::sync_needs_reconnect(&error),
            error: error.to_string(),
        },
    }
}

/// Validate-then-save flow of the editor, run off the UI thread.
pub fn save_tunnel(req: &SaveRequest) -> Result<SaveSuccess, SaveError> {
    let fail = |kind, message: String| SaveError { kind, message };
    let tunnels = backend::try_list_tunnels()
        .map_err(|e| fail(FailureKind::Save, format!("couldn't list tunnels: {e}")))?;
    let renaming = !req.original.is_empty() && req.original != req.name;
    if (renaming || req.original.is_empty()) && tunnels.iter().any(|t| t.name == req.name) {
        return Err(fail(
            FailureKind::Validation,
            format!("A tunnel named “{}” already exists.", req.name),
        ));
    }
    let was_active = tunnels.iter().any(|t| t.name == req.name && t.active);
    let name = req.name.clone();
    if renaming {
        let verified = backend::rename_verified(&req.original, &name, &req.text);
        if !verified.applied() {
            return Err(save_error(verified));
        }
        return Ok(SaveSuccess {
            name,
            message: verified.message,
            sync: SyncState::NotNeeded,
        });
    }
    let verified = backend::save_verified(&name, &req.text);
    if !verified.applied() {
        return Err(save_error(verified));
    }
    let saved = if verified.verdict == Verdict::Confirmed {
        format!("Saved {name}")
    } else {
        verified.message
    };
    if req.activate_after {
        let up = backend::activate_verified(&name);
        let message = if up.applied() {
            format!("Created and activated {name}")
        } else {
            format!("Created {name}, but activation failed: {}", up.message)
        };
        return Ok(SaveSuccess {
            name,
            message,
            sync: SyncState::NotNeeded,
        });
    }
    if !was_active {
        return Ok(SaveSuccess {
            name,
            message: saved,
            sync: SyncState::NotNeeded,
        });
    }
    let sync = sync_after_save(&name);
    let message = match &sync {
        SyncState::Applied => format!(
            "{saved} (WireGuard settings applied live; reconnect for Address/DNS/MTU/routes)"
        ),
        SyncState::Failed {
            error,
            needs_reconnect,
        } => sync_failure_message(&name, error, *needs_reconnect),
        SyncState::NotNeeded => saved,
    };
    Ok(SaveSuccess {
        name,
        message,
        sync,
    })
}

/// Restore result wrapper so a rolled-back restore counts as failed.
pub struct RestoreOutcome(pub Result<RestoreReport, String>);

impl TaskOutcome for RestoreOutcome {
    fn outcome(&self) -> Outcome {
        match &self.0 {
            Ok(report) if report.succeeded() => Outcome::Succeeded,
            _ => Outcome::Failed,
        }
    }
    fn reconciled(&self) -> bool {
        true
    }
}

impl RestoreOutcome {
    pub fn message(&self) -> String {
        match &self.0 {
            Ok(report) => report.message(),
            Err(error) => format!("Restore failed: {error}"),
        }
    }
}

/// Toggle a tunnel from the tray, deciding by its real current state.
pub fn toggle_tunnel(name: &str) -> Result<String, String> {
    // Cached tray state can be stale. Read errors must never be interpreted
    // as inactive.
    let tunnels = backend::try_list_tunnels()?;
    let tunnel = tunnels
        .iter()
        .find(|t| t.name == name)
        .ok_or_else(|| format!("Tunnel {name} no longer exists"))?;
    if tunnel.active {
        backend::deactivate_verified(name).into_result()
    } else {
        backend::activate_verified(name).into_result()
    }
}

pub fn deactivate_all() -> Result<String, String> {
    let tunnels = backend::try_list_tunnels()?;
    let failed: Vec<String> = tunnels
        .iter()
        .filter(|t| t.active)
        .map(|t| backend::deactivate_verified(&t.name))
        .filter(|v| !v.applied())
        .map(|v| v.message)
        .collect();
    if failed.is_empty() {
        Ok("Deactivated all active tunnels".to_string())
    } else {
        Err(format!("Couldn't deactivate: {}", failed.join("; ")))
    }
}

// ---------------------------------------------------------------------------
// Kill switch auto-repair
// ---------------------------------------------------------------------------

/// Delay before attempt N+1 of one loss episode; after the last, give up.
pub const REPAIR_BACKOFF: [Duration; 3] = [
    Duration::from_secs(0),
    Duration::from_secs(15),
    Duration::from_secs(90),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RepairDecision {
    Nothing,
    Repair,
    /// Every automatic attempt for this loss failed; only the warning stays.
    GaveUp,
}

#[derive(Clone, Debug, Default)]
struct Episode {
    attempts: usize,
    next_at: Duration,
    last_error: Option<String>,
}

/// Decides when to run `killswitch-repair`: once immediately per detected
/// loss, then with backoff, never more than `REPAIR_BACKOFF.len()` times.
#[derive(Default)]
pub struct RepairPlanner {
    episodes: HashMap<String, Episode>,
}

impl RepairPlanner {
    pub fn decide(
        &mut self,
        name: &str,
        active: bool,
        status: KillSwitch,
        now: Duration,
    ) -> RepairDecision {
        if !active || !status.is_broken() {
            // Protection restored, switched off, or tunnel down: the loss
            // episode is over; a later loss starts a new one.
            self.episodes.remove(name);
            return RepairDecision::Nothing;
        }
        let episode = self.episodes.entry(name.to_string()).or_insert(Episode {
            next_at: now,
            ..Default::default()
        });
        if episode.attempts >= REPAIR_BACKOFF.len() {
            RepairDecision::GaveUp
        } else if now >= episode.next_at {
            RepairDecision::Repair
        } else {
            RepairDecision::Nothing
        }
    }

    /// A repair was started at `now`.
    pub fn started(&mut self, name: &str, now: Duration) {
        if let Some(episode) = self.episodes.get_mut(name) {
            episode.attempts += 1;
            let delay = REPAIR_BACKOFF
                .get(episode.attempts)
                .copied()
                .unwrap_or(Duration::MAX / 4);
            episode.next_at = now + delay;
        }
    }

    pub fn finished(&mut self, name: &str, error: Option<String>) {
        if let Some(episode) = self.episodes.get_mut(name) {
            episode.last_error = error;
        }
    }

    pub fn attempts(&self, name: &str) -> usize {
        self.episodes.get(name).map_or(0, |e| e.attempts)
    }

    pub fn last_error(&self, name: &str) -> Option<&str> {
        self.episodes
            .get(name)
            .and_then(|e| e.last_error.as_deref())
    }
}

/// Worker for one repair: run it, re-check, and report what is true now.
pub fn repair_killswitch(name: &str) -> Result<String, String> {
    let (result, status) = backend::repair_killswitch(name);
    repair_report(name, result, status)
}

pub fn repair_report(
    name: &str,
    result: Result<(), HelperError>,
    status: KillSwitch,
) -> Result<String, String> {
    match (result, status) {
        (_, KillSwitch::Enabled) => Ok(format!("Kill switch for {name} repaired and active again")),
        (Ok(()), status) => Err(format!(
            "Kill switch repair for {name} reported success, but its status is still {}",
            status.as_str()
        )),
        (Err(error), status) => Err(format!(
            "Kill switch repair for {name} failed ({error}); status: {}",
            status.as_str()
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_file_reads_are_bounded_and_never_wait_for_named_pipes() {
        use std::os::unix::ffi::OsStrExt as _;
        let directory = crate::backend::tests::scratch_dir();
        let path = directory.join("example.conf");
        std::fs::write(&path, "example config").unwrap();
        assert_eq!(read_config_file(&path).unwrap(), "example config");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(MAX_CONFIG + 1)
            .unwrap();
        assert!(read_config_file(&path).unwrap_err().contains("1 MiB"));
        std::fs::remove_file(&path).unwrap();
        let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
        let start = std::time::Instant::now();
        assert!(
            read_config_file(&path)
                .unwrap_err()
                .contains("regular file")
        );
        assert!(start.elapsed() < Duration::from_secs(2));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn sync_failures_never_claim_the_running_tunnel_was_updated() {
        let m = sync_failure_message("wg0", "wg syncconf failed", false);
        assert!(
            m.contains("now differ") && m.contains("wg syncconf failed"),
            "{m}"
        );
        let m = sync_failure_message("wg0", "FwMark changed; reconnect", true);
        assert!(m.contains("reconnect") && m.contains("now differ"), "{m}");
        assert!(!m.contains("applied live"));
    }

    #[test]
    fn save_errors_map_to_draft_failure_kinds_and_outcomes() {
        let unknown = save_error(Verified {
            verdict: Verdict::Unknown,
            message: "Save timed out".into(),
            outcome_was_unknown: true,
        });
        assert_eq!(unknown.kind, FailureKind::Unknown);
        assert_eq!(Err::<(), _>(unknown).outcome(), Outcome::Unknown);
        let failed = save_error(Verified {
            verdict: Verdict::Failed,
            message: "Save failed: permission denied".into(),
            outcome_was_unknown: false,
        });
        assert_eq!(failed.kind, FailureKind::Save);
    }

    #[test]
    fn kill_switch_repair_runs_once_per_loss_with_backoff() {
        let s = Duration::from_secs;
        let mut p = RepairPlanner::default();
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Enabled, s(0)),
            RepairDecision::Nothing
        );
        // Loss detected: repair immediately, once.
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(1)),
            RepairDecision::Repair
        );
        p.started("wg0", s(1));
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(2)),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(15)),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Mismatch, s(16)),
            RepairDecision::Repair
        );
        p.started("wg0", s(16));
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(100)),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(106)),
            RepairDecision::Repair
        );
        p.started("wg0", s(106));
        p.finished("wg0", Some("nft failed".into()));
        // Never spins: after the last attempt it gives up for this episode.
        for t in 107..2000 {
            assert_ne!(
                p.decide("wg0", true, KillSwitch::Missing, s(t)),
                RepairDecision::Repair
            );
        }
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(3000)),
            RepairDecision::GaveUp
        );
        assert_eq!(p.attempts("wg0"), 3);
        assert_eq!(p.last_error("wg0"), Some("nft failed"));
        // Restored, then lost again: a new episode gets a fresh repair.
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Enabled, s(3001)),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Missing, s(3002)),
            RepairDecision::Repair
        );
    }

    #[test]
    fn kill_switch_is_not_repaired_when_down_off_or_unknown() {
        let mut p = RepairPlanner::default();
        let now = Duration::from_secs(5);
        assert_eq!(
            p.decide("wg0", false, KillSwitch::Missing, now),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Disabled, now),
            RepairDecision::Nothing
        );
        assert_eq!(
            p.decide("wg0", true, KillSwitch::Unknown, now),
            RepairDecision::Nothing
        );
    }

    #[test]
    fn repair_report_states_the_rechecked_status() {
        assert!(
            repair_report("wg0", Ok(()), KillSwitch::Enabled)
                .unwrap()
                .contains("repaired")
        );
        let e = repair_report("wg0", Ok(()), KillSwitch::Missing).unwrap_err();
        assert!(e.contains("still missing"), "{e}");
        let e = repair_report("wg0", Err(HelperError::TimedOut), KillSwitch::Unknown).unwrap_err();
        assert!(e.contains("timed out") && e.contains("unknown"), "{e}");
        // A timed-out repair that actually worked is reported as repaired.
        assert!(repair_report("wg0", Err(HelperError::TimedOut), KillSwitch::Enabled).is_ok());
    }

    #[test]
    fn import_summary_mentions_errors_when_nothing_was_imported() {
        let summary = ImportSummary {
            imported: 0,
            skipped: 2,
            last: None,
            first_error: Some("Config must be a regular file, not a named pipe".into()),
            unknown: false,
        };
        assert!(summary.message().contains("named pipe"));
        assert_eq!(summary.outcome(), Outcome::Failed);
    }
}
