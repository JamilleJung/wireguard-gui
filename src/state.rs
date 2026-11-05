//! Application state shared between the UI thread, the live poller and the
//! tray: selection, latest live data, request counters, window handles and
//! saved preferences.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Mutex, OnceLock};

use crate::actions::RepairPlanner;
use crate::backend::{Detail, Health, KillSwitch};
use crate::draft::Draft;
use crate::theme::ThemeChoice;
use crate::{AboutWindow, DiagWindow, EditWindow, HelpWindow, QrWindow, SetupWindow, UpdateWindow};

/// Set once the tray registers with a StatusNotifierItem host. If it never does
/// (e.g. GNOME without the AppIndicator extension), closing the window must quit
/// rather than hide into a tray that will never appear.
pub static TRAY_ONLINE: AtomicBool = AtomicBool::new(false);

/// The currently-selected tunnel name, shared with the background poll thread
/// so it can fetch live status off the UI thread (keeps the UI smooth).
pub static SELECTED: Mutex<Option<String>> = Mutex::new(None);

/// Latest live data computed by the background thread, applied on the UI thread.
pub struct Live {
    /// The tunnel `detail` was computed for, so the UI can drop a payload that
    /// no longer matches the current selection (avoids clobbering a fresh pick).
    pub name: Option<String>,
    pub detail: Option<Detail>,
    /// Per-peer health of `detail`, from the poller's rx/tx history.
    pub health: Vec<Health>,
    pub actives: Vec<String>,
    /// Live throughput for the selected tunnel, e.g. "↓ 1.2 MiB/s   ↑ 340 KiB/s".
    pub speed: String,
    /// Kill switch status of every active tunnel (checked every few seconds).
    pub killswitch: Option<Vec<(String, KillSwitch)>>,
}
pub static LIVE: Mutex<Option<Live>> = Mutex::new(None);

/// Latest per-peer health per tunnel, for diagnostics and detail loads.
pub static HEALTH: Mutex<Option<HashMap<String, Vec<Health>>>> = Mutex::new(None);

pub fn known_health(name: &str) -> Option<Vec<Health>> {
    HEALTH.lock().ok()?.as_ref()?.get(name).cloned()
}

pub fn store_health(name: &str, health: Vec<Health>) {
    if let Ok(mut map) = HEALTH.lock() {
        map.get_or_insert_with(HashMap::new)
            .insert(name.to_string(), health);
    }
}

/// A short live summary for the tray tooltip (active tunnels + throughput),
/// written by the background thread and read by the tray on demand.
pub static TRAY_INFO: Mutex<String> = Mutex::new(String::new());
pub static TRAY_TUNNELS: Mutex<Vec<(String, bool)>> = Mutex::new(Vec::new());

pub static LIST_REQUEST: AtomicU64 = AtomicU64::new(0);
pub static LOG_REQUEST: AtomicU64 = AtomicU64::new(0);
pub static DETAIL_REQUEST: AtomicU64 = AtomicU64::new(0);

static THEME: OnceLock<ThemeChoice> = OnceLock::new();

pub fn set_theme(choice: ThemeChoice) {
    let _ = THEME.set(choice);
    crate::theme::set_ascii(choice.ascii);
}

pub fn theme_choice() -> ThemeChoice {
    THEME.get().copied().unwrap_or_default()
}

thread_local! {
    // Keeps the currently-open editor window (and its draft) alive. Replaced
    // on the next open unless its draft failed; only touched on the UI thread.
    pub static EDITOR: RefCell<Option<(EditWindow, Rc<RefCell<Draft>>)>> = const { RefCell::new(None) };
    // Keeps the currently-open QR window alive.
    pub static QRWIN: RefCell<Option<QrWindow>> = const { RefCell::new(None) };
    // Keeps the About window alive.
    pub static ABOUTWIN: RefCell<Option<AboutWindow>> = const { RefCell::new(None) };
    // Keeps the in-app Help window alive.
    pub static HELPWIN: RefCell<Option<HelpWindow>> = const { RefCell::new(None) };
    // Keeps the Updates window alive.
    pub static UPDATEWIN: RefCell<Option<UpdateWindow>> = const { RefCell::new(None) };
    // Keeps the connection-diagnostics window alive.
    pub static DIAGWIN: RefCell<Option<DiagWindow>> = const { RefCell::new(None) };
    // Keeps the first-run Setup wizard alive while it's shown.
    pub static SETUPWIN: RefCell<Option<SetupWindow>> = const { RefCell::new(None) };
    // Long-lived clipboard handle (kept alive so the copied text persists).
    pub static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    // The unfiltered journal text; the Log tab's filter/this-tunnel toggles are
    // applied to this each time without re-querying the privileged helper.
    pub static RAW_LOG: RefCell<String> = const { RefCell::new(String::new()) };
    // The last rendered detail and its health, re-rendered on peer paging.
    pub static LAST_DETAIL: RefCell<Option<(Detail, Vec<Health>)>> = const { RefCell::new(None) };
    // Kill switch auto-repair bookkeeping (UI thread only).
    pub static REPAIR: RefCell<RepairPlanner> = RefCell::new(RepairPlanner::default());
    // Last known kill switch status per active tunnel (UI thread only).
    pub static KILLSWITCH: RefCell<HashMap<String, KillSwitch>> = RefCell::new(HashMap::new());
}

/// Path of the saved "Advanced" disclosure preference: $XDG_CONFIG_HOME (or
/// ~/.config)/wireguard-gui/mode.
fn mode_state_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wireguard-gui").join("mode"))
}

/// Load whether the per-tunnel "Advanced ▾" section starts expanded. New users
/// default to collapsed. Tolerates the legacy file contents from the old
/// Easy/Advanced toggle: old "advanced" maps to expanded, anything else to
/// collapsed.
pub fn load_show_advanced() -> bool {
    match mode_state_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => matches!(s.trim(), "expanded" | "advanced"),
        None => false,
    }
}

/// Persist the disclosure state so a power user who expands it keeps it.
pub fn save_show_advanced(expanded: bool) {
    if let Some(p) = mode_state_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, if expanded { "expanded" } else { "collapsed" });
    }
}
