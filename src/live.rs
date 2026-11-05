//! Live status: a background poller gathers data (all blocking helper calls
//! happen there), a UI-thread timer applies it, and kill switch losses are
//! repaired automatically.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use slint::{ComponentHandle, Model};

use crate::actions::{self, RepairDecision};
use crate::backend::{self, Detail, HealthTracker, KillSwitch};
use crate::ops::{Outcome, TaskOutcome};
use crate::state::{self, KILLSWITCH, LIVE, Live, REPAIR, SELECTED, TRAY_INFO, TRAY_TUNNELS};
use crate::ui_bridge::{refresh_list, run_ui_task, set_status, show_detail};
use crate::{MainWindow, theme};

/// Seconds between kill switch checks of every active tunnel.
const KILLSWITCH_EVERY: u64 = 5;

fn now() -> Duration {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    ORIGIN.get_or_init(Instant::now).elapsed()
}

/// Start the background poller thread.
pub fn start_poller() {
    std::thread::spawn(move || {
        // Previous (name, rx, tx, when) sample for the selected tunnel, to derive
        // a live throughput rate.
        let mut prev: Option<(String, u64, u64, Instant)> = None;
        let mut tracker = HealthTracker::new();
        let mut tick = 0u64;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            tick += 1;
            let name = SELECTED.lock().unwrap().clone();
            let tunnels = match backend::try_list_tunnels() {
                Ok(tunnels) => tunnels,
                Err(error) => {
                    *TRAY_INFO.lock().unwrap() = format!("Tunnel status unavailable: {error}");
                    prev = None;
                    continue;
                }
            };
            *TRAY_TUNNELS.lock().unwrap() =
                tunnels.iter().map(|t| (t.name.clone(), t.active)).collect();
            let actives: Vec<String> = tunnels
                .iter()
                .filter(|t| t.active)
                .map(|t| t.name.clone())
                .collect();
            let detail = name
                .as_ref()
                .filter(|n| tunnels.iter().any(|t| &t.name == *n))
                .map(|n| backend::get_detail(n));
            // Record up/down transitions for every tunnel so health knows
            // how long a tunnel has been up; full peer health for the
            // selected one.
            for t in &tunnels {
                if detail.as_ref().is_none_or(|d| d.name != t.name) {
                    tracker.observe(&t.name, t.active, &[], now());
                }
            }
            let health = detail
                .as_ref()
                .map(|d| tracker.observe(&d.name, d.active, &d.peers, now()))
                .unwrap_or_default();
            if let Some(d) = &detail {
                state::store_health(&d.name, health.clone());
            }

            // Throughput for the selected tunnel from successive samples.
            let mut speed = String::new();
            match (name.as_ref(), detail.as_ref()) {
                (Some(n), Some(d)) => {
                    let at = Instant::now();
                    if let Some((pn, prx, ptx, pt)) = prev.as_ref()
                        && pn == n
                    {
                        let dt = at.duration_since(*pt).as_secs_f64();
                        if dt >= 0.5 {
                            let rrx = (d.rx_bytes.saturating_sub(*prx) as f64 / dt) as u64;
                            let rtx = (d.tx_bytes.saturating_sub(*ptx) as f64 / dt) as u64;
                            speed = format!(
                                "↓ {}/s   ↑ {}/s",
                                backend::fmt_bytes(rrx),
                                backend::fmt_bytes(rtx)
                            );
                        }
                    }
                    prev = Some((n.clone(), d.rx_bytes, d.tx_bytes, at));
                }
                _ => prev = None,
            }

            let killswitch = tick.is_multiple_of(KILLSWITCH_EVERY).then(|| {
                actives
                    .iter()
                    .map(|n| (n.clone(), backend::killswitch_status(n)))
                    .collect::<Vec<_>>()
            });

            // Tray tooltip summary.
            let tray = if actives.is_empty() {
                "No active tunnel".to_string()
            } else if speed.is_empty() {
                format!("Active: {}", actives.join(", "))
            } else {
                format!("Active: {} · {}", actives.join(", "), speed)
            };
            *TRAY_INFO.lock().unwrap() = theme::text(&tray);

            let mut slot = LIVE.lock().unwrap();
            // Keep an unapplied kill switch sample rather than overwrite it.
            let killswitch = killswitch.or_else(|| slot.take().and_then(|old| old.killswitch));
            *slot = Some(Live {
                name,
                detail,
                health,
                actives,
                speed,
                killswitch,
            });
        }
    });
}

/// Start the UI-thread timer that applies the poller's data. The returned
/// timer must be kept alive.
pub fn start_apply_timer(ui: &MainWindow) -> slint::Timer {
    let timer = slint::Timer::default();
    let w = ui.as_weak();
    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_secs(1),
        move || {
            let Some(ui) = w.upgrade() else { return };
            let Some(live) = LIVE.lock().unwrap().take() else {
                return;
            };
            if let Some(statuses) = &live.killswitch {
                apply_killswitch_statuses(&ui, statuses);
            }
            // Live status only matters on the Tunnels tab. Skip the apply +
            // repaint on the Log/Backup tabs so the per-second redraw never
            // churns the Log view — that churn made dragging a text
            // selection there feel laggy/janky.
            if ui.get_active_tab() != 0 {
                return;
            }
            // Only apply detail if it still matches the current selection,
            // so a payload computed for a previously-selected tunnel can't
            // overwrite a selection the user just changed.
            let current = SELECTED.lock().unwrap().clone();
            if let Some(d) = &live.detail
                && live.name == current
            {
                show_detail(&ui, d, &live.health, &live.speed);
            }
            let model = ui.get_tunnels();
            for i in 0..model.row_count() {
                if let Some(mut row) = model.row_data(i) {
                    let want = live.actives.iter().any(|a| a == row.name.as_str());
                    if row.active != want {
                        row.active = want;
                        model.set_row_data(i, row);
                    }
                }
            }
            ui.window().request_redraw();
        },
    );
    timer
}

/// Record the latest kill switch statuses and repair any lost ones.
fn apply_killswitch_statuses(ui: &MainWindow, statuses: &[(String, KillSwitch)]) {
    let previous: Vec<String> = KILLSWITCH.with(|map| map.borrow().keys().cloned().collect());
    for name in previous {
        if !statuses.iter().any(|(n, _)| *n == name) {
            consider_repair(ui, &name, false, KillSwitch::Disabled);
        }
    }
    KILLSWITCH.with(|map| *map.borrow_mut() = statuses.iter().cloned().collect());
    for (name, status) in statuses {
        consider_repair(ui, name, true, *status);
    }
}

/// Run `killswitch-repair` when the planner says so (once per loss, with
/// backoff), then re-check and report the result.
pub fn consider_repair(ui: &MainWindow, name: &str, active: bool, status: KillSwitch) {
    let decision = REPAIR.with(|p| p.borrow_mut().decide(name, active, status, now()));
    if decision != RepairDecision::Repair {
        return;
    }
    let target = name.to_string();
    let started = run_ui_task(
        ui,
        "Kill switch protection lost — repairing…",
        move || {
            let report = actions::repair_killswitch(&target);
            (target, report)
        },
        |ui, (name, report)| {
            REPAIR.with(|p| {
                p.borrow_mut()
                    .finished(&name, report.as_ref().err().cloned())
            });
            set_status(ui, report.unwrap_or_else(|e| e));
            refresh_list(ui);
        },
    );
    // Busy with another operation: try again on a later tick (not counted).
    if started {
        REPAIR.with(|p| p.borrow_mut().started(name, now()));
    }
}

impl TaskOutcome for (String, Result<String, String>) {
    fn outcome(&self) -> Outcome {
        self.1.outcome()
    }
    fn reconciled(&self) -> bool {
        true
    }
}

/// Banner text for the kill switch of the shown tunnel ("" when fine).
pub fn killswitch_banner(
    status: KillSwitch,
    active: bool,
    attempts: usize,
    last_error: Option<&str>,
) -> String {
    let problem = status.problem(active);
    if problem.is_empty() {
        return String::new();
    }
    if !(active && status.is_broken()) {
        return problem.to_string();
    }
    if attempts >= actions::REPAIR_BACKOFF.len() {
        let why = last_error.map(|e| format!(" ({e})")).unwrap_or_default();
        format!(
            "{problem} Automatic repair failed{why}. Use Repair now, or turn the kill switch off and on."
        )
    } else if attempts > 0 {
        format!("{problem} Repairing automatically (attempt {attempts})…")
    } else {
        format!("{problem} Repairing automatically…")
    }
}

pub fn refresh_killswitch_banner(ui: &MainWindow, d: &Detail) {
    consider_repair(ui, &d.name, d.active, d.killswitch);
    let (attempts, last_error) = REPAIR.with(|p| {
        let p = p.borrow();
        (
            p.attempts(&d.name),
            p.last_error(&d.name).map(str::to_string),
        )
    });
    ui.set_ks_warning(
        theme::text(&killswitch_banner(
            d.killswitch,
            d.active,
            attempts,
            last_error.as_deref(),
        ))
        .into(),
    );
    ui.set_ks_repairable(d.active && d.killswitch.is_broken());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kill_switch_loss_is_surfaced_clearly() {
        assert_eq!(killswitch_banner(KillSwitch::Enabled, true, 0, None), "");
        assert_eq!(killswitch_banner(KillSwitch::Disabled, true, 0, None), "");
        let b = killswitch_banner(KillSwitch::Missing, true, 0, None);
        assert!(
            b.contains("NOT blocked") && b.contains("Repairing automatically"),
            "{b}"
        );
        let b = killswitch_banner(KillSwitch::Mismatch, true, 1, None);
        assert!(b.contains("attempt 1"), "{b}");
        let b = killswitch_banner(KillSwitch::Missing, true, 3, Some("nft: no such table"));
        assert!(
            b.contains("Automatic repair failed (nft: no such table)"),
            "{b}"
        );
        let b = killswitch_banner(KillSwitch::Unknown, true, 0, None);
        assert!(b.contains("could not be read"), "{b}");
        let b = killswitch_banner(KillSwitch::Missing, false, 0, None);
        assert!(
            b.contains("not in force") && !b.contains("Repairing"),
            "{b}"
        );
    }
}
