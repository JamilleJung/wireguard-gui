//! Bridging between background work and the Slint UI: rendering backend data
//! into Slint models, the status bar, and running work off the UI thread.

pub mod editor;
pub mod editor_form;
pub mod markdown;
pub mod windows;

use std::sync::atomic::Ordering;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use crate::backend::{self, Health};
use crate::ops::{OpGuard, TaskOutcome};
use crate::state::{
    self, DETAIL_REQUEST, LIST_REQUEST, LOG_REQUEST, RAW_LOG, SELECTED, TRAY_TUNNELS,
};
use crate::{BackupRow, MainWindow, PeerInfo, TunnelDetail, TunnelItem, clipboard, layout, theme};

/// Apply the chosen theme to any window's `Theme` global.
#[macro_export]
macro_rules! apply_theme {
    ($win:expr) => {{
        let choice = $crate::state::theme_choice();
        let global = $win.global::<$crate::Theme>();
        global.set_high_contrast(choice.high_contrast);
        global.set_ascii(choice.ascii);
    }};
}

/// Split a document into a `[string]` line model for the virtualized ListView
/// doc views (Log / Diagnostics). One Text row per line keeps scrolling
/// O(visible) and avoids the read-only-TextEdit relayout/blink-Timer.
pub fn lines_model(text: &str) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = text.lines().map(|l| theme::text(l).into()).collect();
    ModelRc::new(VecModel::from(rows))
}

/// Build a single-line model from one placeholder/status string (never-blank
/// states for the Log view).
pub fn one_line_model(text: &str) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(vec![SharedString::from(theme::text(text))]))
}

/// Open a URL in the user's default browser.
pub fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Copy text to the system clipboard. Returns whether it succeeded.
pub fn copy_to_clipboard(text: &str) -> bool {
    // Native clipboard via arboard (the Wayland backend is enabled in Cargo.toml
    // via the `wayland-data-control` feature). Cache the handle so the served
    // selection persists for the app's lifetime.
    let via_arboard = state::CLIPBOARD.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = arboard::Clipboard::new().ok();
        }
        c.as_mut()
            .map(|cb| cb.set_text(text.to_string()).is_ok())
            .unwrap_or(false)
    });
    if via_arboard {
        return true;
    }
    // Fallback for sessions where the native backend can't own the selection:
    // pipe to whichever clipboard CLI is present (wl-copy / xclip / xsel).
    clipboard::copy_via_cli(text)
}

/// Briefly show an inline "Copied!" confirmation in a doc window (Help / Update
/// / Diag), then clear it. These windows have no main status bar, so a tiny
/// transient hint stands in. `set_hint` writes the window's `copied-hint`.
pub fn flash_copied_hint<W, F>(win: &W, set_hint: F, msg: &str)
where
    W: ComponentHandle + 'static,
    F: Fn(&W, SharedString) + Copy + 'static,
{
    set_hint(win, msg.into());
    let weak = win.as_weak();
    slint::Timer::single_shot(Duration::from_millis(1500), move || {
        if let Some(w) = weak.upgrade() {
            set_hint(&w, SharedString::new());
        }
    });
}

/// How long a status message stays: longer messages (failures, rollback
/// reports) get more reading time.
pub fn status_duration(msg: &str) -> Duration {
    let words = msg.split_whitespace().count() as u64;
    Duration::from_secs((4 + words / 4).min(20))
}

pub fn set_status(ui: &MainWindow, msg: impl Into<String>) {
    let msg: SharedString = theme::text(&msg.into()).into();
    ui.set_status(msg.clone());
    // Auto-dismiss (clear only if it hasn't been replaced).
    let w = ui.as_weak();
    slint::Timer::single_shot(status_duration(&msg), move || {
        if let Some(ui) = w.upgrade()
            && ui.get_status() == msg
        {
            ui.set_status(SharedString::new());
        }
    });
}

/// Run `work` on a background thread under the operation state machine, then
/// apply `done` on the UI thread. Returns false (and says so) when another
/// operation is still running.
pub fn run_ui_task<T: TaskOutcome + Send + 'static>(
    ui: &MainWindow,
    message: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&MainWindow, T) + Send + 'static,
) -> bool {
    let Some(guard) = OpGuard::acquire(message) else {
        ui_idle(ui);
        return false;
    };
    ui.set_status(theme::text(message).into());
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = work();
        guard.complete(result.outcome());
        if result.reconciled() {
            guard.reconcile();
        }
        let _ = weak.upgrade_in_event_loop(move |ui| {
            // Retain exclusion until the completion is applied, so an editor
            // open cannot overtake a ready-but-undelivered earlier result.
            guard.reconcile();
            drop(guard);
            done(&ui, result);
        });
    });
    true
}

pub fn ui_idle(ui: &MainWindow) -> bool {
    if crate::ops::current_phase() != crate::ops::Phase::Idle {
        set_status(ui, "Another operation is still running.");
        false
    } else {
        true
    }
}

fn peer_info(p: &backend::Peer, health: Health, number: usize) -> PeerInfo {
    PeerInfo {
        public_key: p.public_key.clone().into(),
        preshared: p.preshared,
        allowed_ips: p.allowed_ips.clone().into(),
        endpoint: p.endpoint.clone().into(),
        keepalive: p.keepalive.clone().into(),
        latest_handshake: p.latest_handshake.clone().into(),
        transfer: p.transfer.clone().into(),
        health: theme::text(&backend::health::peer_summary(health, p.handshake_age)).into(),
        health_level: health.level().into(),
        number: number as i32,
    }
}

/// Render a backend detail plus per-peer health, showing only the current
/// page of (filtered) peers.
pub fn to_slint_detail(d: &backend::Detail, health: &[Health], ui: &MainWindow) -> TunnelDetail {
    let health: Vec<Health> = if health.len() == d.peers.len() {
        health.to_vec()
    } else {
        backend::health::snapshot(d.active, &d.peers)
    };
    let summary_input: Vec<(Health, Option<u64>)> = health
        .iter()
        .copied()
        .zip(d.peers.iter().map(|p| p.handshake_age))
        .collect();
    let overall = backend::health::aggregate(d.active, &health);
    let matched = layout::filter_peers(
        d.peers.iter().map(|p| {
            [
                p.public_key.as_str(),
                p.endpoint.as_str(),
                p.allowed_ips.as_str(),
            ]
        }),
        &ui.get_peer_filter(),
    );
    let page = layout::peer_page(
        matched.len(),
        ui.get_peer_page().max(0) as usize,
        layout::PEERS_PER_PAGE,
    );
    ui.set_peer_page(page.page as i32);
    ui.set_peer_pages(page.pages as i32);
    ui.set_peer_page_label(page.label(d.peers.len(), matched.len()).into());
    let peers: Vec<PeerInfo> = matched[page.start..page.end]
        .iter()
        .map(|&i| peer_info(&d.peers[i], health[i], i + 1))
        .collect();
    TunnelDetail {
        name: d.name.clone().into(),
        active: d.active,
        autostart: d.autostart,
        killswitch: d.killswitch.intended_on(),
        killswitch_state: d.killswitch.as_str().into(),
        public_key: d.public_key.clone().into(),
        listen_port: d.listen_port.clone().into(),
        addresses: d.addresses.clone().into(),
        dns: d.dns.clone().into(),
        peers: ModelRc::new(VecModel::from(peers)),
        peer_count: d.peers.len() as i32,
        health: theme::text(&backend::health::interface_summary(
            d.active,
            &summary_input,
        ))
        .into(),
        health_level: overall.level().into(),
        speed: SharedString::new(),
    }
}

/// Show a detail and the banners derived from it.
pub fn show_detail(ui: &MainWindow, d: &backend::Detail, health: &[Health], speed: &str) {
    let mut det = to_slint_detail(d, health, ui);
    det.speed = theme::text(speed).into();
    ui.set_detail(det);
    ui.set_has_selection(true);
    let states = if health.len() == d.peers.len() {
        health.to_vec()
    } else {
        backend::health::snapshot(d.active, &d.peers)
    };
    let overall = backend::health::aggregate(d.active, &states);
    ui.set_conn_warning(
        theme::text(&connection_warning(overall, crate::doctor::clock_synced())).into(),
    );
    state::LAST_DETAIL.with(|slot| *slot.borrow_mut() = Some((d.clone(), health.to_vec())));
    crate::live::refresh_killswitch_banner(ui, d);
}

/// The amber banner text for a problem connection ("" when fine). The clock
/// is mentioned only as a possible cause, and only when handshakes fail.
pub fn connection_warning(overall: Health, clock_synced: Option<bool>) -> String {
    let base = match overall {
        Health::ConnectionFailed => {
            "Not connected: packets are sent but the peer does not answer. Click Diagnose for why."
        }
        Health::HandshakeStale => {
            "Handshake stale: keepalive is on but no new handshake completed. Click Diagnose for why."
        }
        _ => return String::new(),
    };
    match backend::health::clock_hint(overall, clock_synced) {
        Some(hint) => format!("{base} {hint}"),
        None => base.to_string(),
    }
}

/// Re-render the last detail (after a peer page/filter change).
pub fn rerender_detail(ui: &MainWindow) {
    let last = state::LAST_DETAIL.with(|slot| slot.borrow().clone());
    if let Some((d, health)) = last
        && selected_name(ui).as_deref() == Some(d.name.as_str())
    {
        let speed = ui.get_detail().speed.to_string();
        show_detail(ui, &d, &health, &speed);
    }
}

/// Rebuild the left-hand tunnel list, preserving the selected name if possible.
pub fn refresh_list(ui: &MainWindow) {
    refresh_list_after(ui, None);
}

pub fn refresh_list_after(ui: &MainWindow, preferred: Option<(String, Option<String>)>) {
    let request = LIST_REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let result = backend::try_list_tunnels();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if request != LIST_REQUEST.load(Ordering::SeqCst) {
                return;
            }
            match result {
                Ok(tunnels) => {
                    let preferred = preferred
                        .filter(|(_, expected)| selected_name(&ui) == *expected)
                        .map(|(name, _)| name);
                    apply_tunnel_list(&ui, tunnels, preferred);
                }
                Err(error) => set_status(&ui, format!("Couldn't refresh tunnels: {error}")),
            }
        });
    });
}

pub fn apply_tunnel_list(
    ui: &MainWindow,
    tunnels: Vec<backend::Tunnel>,
    preferred: Option<String>,
) {
    *TRAY_TUNNELS.lock().unwrap() = tunnels.iter().map(|t| (t.name.clone(), t.active)).collect();
    let prev = preferred.or_else(|| selected_name(ui));
    let mut new_index = -1i32;
    let items: Vec<TunnelItem> = tunnels
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if Some(&t.name) == prev.as_ref() {
                new_index = i as i32;
            }
            TunnelItem {
                name: t.name.clone().into(),
                active: t.active,
            }
        })
        .collect();

    // Auto-select the first tunnel when nothing is selected yet (like the
    // Windows client) so live status shows immediately.
    if new_index < 0 && prev.is_none() && !items.is_empty() {
        new_index = 0;
    }

    ui.set_tunnels(ModelRc::new(VecModel::from(items)));
    ui.set_selected_index(new_index);
    ui.set_has_selection(new_index >= 0);
    if new_index >= 0 {
        load_detail(ui, &tunnels[new_index as usize].name);
    } else {
        *SELECTED.lock().unwrap() = None;
    }
}

pub fn load_detail(ui: &MainWindow, name: &str) {
    let name = name.to_string();
    let request = DETAIL_REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
    let changed = SELECTED.lock().unwrap().as_deref() != Some(name.as_str());
    *SELECTED.lock().unwrap() = Some(name.clone());
    if changed {
        ui.set_peer_page(0);
        ui.set_peer_filter(SharedString::new());
    }
    // Never leave the previous tunnel's buttons/keys visible while fetching a
    // newly selected tunnel. Read-only polling can fill this same selection.
    ui.set_has_selection(false);
    let weak = ui.as_weak();
    std::thread::spawn(move || {
        let detail = backend::get_detail(&name);
        let _ = weak.upgrade_in_event_loop(move |ui| {
            if request != DETAIL_REQUEST.load(Ordering::SeqCst)
                || selected_name(&ui).as_deref() != Some(name.as_str())
            {
                return;
            }
            let health = state::known_health(&name).unwrap_or_default();
            show_detail(&ui, &detail, &health, "");
        });
    });
}

/// Selected-tunnel name from the live model + selected index.
pub fn selected_name(ui: &MainWindow) -> Option<String> {
    let idx = ui.get_selected_index();
    if idx < 0 {
        return None;
    }
    ui.get_tunnels()
        .row_data(idx as usize)
        .map(|t| t.name.to_string())
}

/// Re-apply the Log tab's filter + this-tunnel toggle to the stored raw log.
pub fn apply_log_filter(ui: &MainWindow) {
    let raw = RAW_LOG.with(|r| r.borrow().clone());

    // Distinct, never-blank states:
    //  - RAW empty  -> nothing loaded yet (only happens pre-first-load / after Clear)
    //  - get_log() sentinels (error / "no entries") -> show verbatim, unfiltered
    if raw.is_empty() {
        ui.set_log_lines(one_line_model(
            "(loading… press Refresh if this stays empty)",
        ));
        return;
    }
    if raw.starts_with("Could not read the log:") || raw == "(no recent log entries)" {
        ui.set_log_lines(lines_model(&raw));
        return;
    }

    let needle = ui.get_log_filter().to_string().to_ascii_lowercase();
    let only = if ui.get_log_this_tunnel() {
        selected_name(ui)
    } else {
        None
    };
    let filtered: Vec<SharedString> = raw
        .lines()
        .filter(|line| {
            (needle.is_empty() || line.to_ascii_lowercase().contains(&needle))
                && only.as_deref().is_none_or(|n| line.contains(n))
        })
        .map(|l| theme::text(l).into())
        .collect();

    if filtered.is_empty() {
        ui.set_log_lines(one_line_model("(no log lines match the current filter)"));
    } else {
        ui.set_log_lines(ModelRc::new(VecModel::from(filtered)));
    }
}

/// Reload the journal OFF the UI thread (`get_log()` shells out via sudo and can
/// take a moment) and post the result back to the event loop. This is why
/// opening the Log tab and pressing Refresh never stutter the UI.
pub fn refresh_log_async(w: slint::Weak<MainWindow>) {
    let request = LOG_REQUEST.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        let text = backend::get_log();
        let _ = w.upgrade_in_event_loop(move |ui| {
            if request != LOG_REQUEST.load(Ordering::SeqCst) {
                return;
            }
            RAW_LOG.with(|r| *r.borrow_mut() = text);
            apply_log_filter(&ui);
        });
    });
}

/// Rebuild the Backup tab's list model from disk.
pub fn populate_backups(ui: &MainWindow) {
    let rows: Vec<BackupRow> = backend::list_backups()
        .into_iter()
        .map(|b| BackupRow {
            name: b.name.into(),
            date: backend::fmt_time(b.when_secs).into(),
            size: backend::fmt_size(b.size).into(),
            count: b.count.to_string().into(),
        })
        .collect();
    ui.set_backups(ModelRc::new(VecModel::from(rows)));
    ui.set_backup_sel(-1);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_banner_only_for_real_problems_and_clock_is_a_possible_cause() {
        assert_eq!(connection_warning(Health::Idle, Some(false)), "");
        assert_eq!(connection_warning(Health::Healthy, Some(false)), "");
        assert_eq!(connection_warning(Health::NoTraffic, None), "");
        let failed = connection_warning(Health::ConnectionFailed, Some(true));
        assert!(
            failed.contains("Diagnose") && !failed.contains("clock"),
            "{failed}"
        );
        let failed = connection_warning(Health::ConnectionFailed, Some(false));
        assert!(failed.contains("one possible cause"), "{failed}");
    }

    #[test]
    fn longer_status_messages_stay_longer() {
        assert_eq!(status_duration("Saved wg0"), Duration::from_secs(4));
        let long = "word ".repeat(40);
        assert!(status_duration(&long) > Duration::from_secs(10));
        assert!(status_duration(&"w ".repeat(1000)) <= Duration::from_secs(20));
    }
}
