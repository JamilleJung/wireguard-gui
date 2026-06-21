// Hide the console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod clipboard;
mod config;
mod create;
mod doctor;
mod secrets;
mod ui_bridge;
mod update;
mod validation;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use ui_bridge::editor_form::{
    Fields, PeerFields, config_to_fields, fields_to_config, form_representable,
};

slint::include_modules!();

/// Set once the tray registers with a StatusNotifierItem host. If it never does
/// (e.g. GNOME without the AppIndicator extension), closing the window must quit
/// rather than hide into a tray that will never appear.
static TRAY_ONLINE: AtomicBool = AtomicBool::new(false);

/// The currently-selected tunnel name, shared with the background poll thread
/// so it can fetch live status off the UI thread (keeps the UI smooth).
static SELECTED: Mutex<Option<String>> = Mutex::new(None);

/// Latest live data computed by the background thread, applied on the UI thread.
struct Live {
    /// The tunnel `detail` was computed for, so the UI can drop a payload that
    /// no longer matches the current selection (avoids clobbering a fresh pick).
    name: Option<String>,
    detail: Option<backend::Detail>,
    actives: Vec<String>,
    /// Live throughput for the selected tunnel, e.g. "↓ 1.2 MiB/s   ↑ 340 KiB/s".
    speed: String,
}
static LIVE: Mutex<Option<Live>> = Mutex::new(None);

/// A short live summary for the tray tooltip (active tunnels + throughput),
/// written by the background thread and read by the tray on demand.
static TRAY_INFO: Mutex<String> = Mutex::new(String::new());

thread_local! {
    // Keeps the currently-open editor window alive. Replaced (and the old one
    // dropped) on the next open; only ever touched on the UI thread.
    static EDITOR: RefCell<Option<EditWindow>> = const { RefCell::new(None) };
    // Keeps the currently-open QR window alive.
    static QRWIN: RefCell<Option<QrWindow>> = const { RefCell::new(None) };
    // Keeps the About window alive.
    static ABOUTWIN: RefCell<Option<AboutWindow>> = const { RefCell::new(None) };
    // Keeps the in-app Help window alive.
    static HELPWIN: RefCell<Option<HelpWindow>> = const { RefCell::new(None) };
    // Keeps the Updates window alive.
    static UPDATEWIN: RefCell<Option<UpdateWindow>> = const { RefCell::new(None) };
    // Keeps the connection-diagnostics window alive.
    static DIAGWIN: RefCell<Option<DiagWindow>> = const { RefCell::new(None) };
    // Keeps the first-run Setup wizard alive while it's shown.
    static SETUPWIN: RefCell<Option<SetupWindow>> = const { RefCell::new(None) };
    // Long-lived clipboard handle (kept alive so the copied text persists).
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
    // The unfiltered journal text; the Log tab's filter/this-tunnel toggles are
    // applied to this each time without re-querying the privileged helper.
    static RAW_LOG: RefCell<String> = const { RefCell::new(String::new()) };
}

/// System-tray icon (StatusNotifierItem). Shows on KDE, and on GNOME with the
/// AppIndicator extension. Menu: Show the window, or Quit.
struct Tray {
    window: slint::Weak<MainWindow>,
}

impl ksni::Tray for Tray {
    fn id(&self) -> String {
        "wireguard-gui".into()
    }
    fn title(&self) -> String {
        "WireGuard".into()
    }
    fn icon_name(&self) -> String {
        "wireguard-gui".into()
    }
    fn status(&self) -> ksni::Status {
        // Always visible.
        ksni::Status::Active
    }
    fn watcher_online(&self) {
        TRAY_ONLINE.store(true, Ordering::Relaxed);
    }
    fn watcher_offline(&self, _reason: ksni::OfflineReason) -> bool {
        // The SNI host went away — closing the window should now quit, not hide.
        TRAY_ONLINE.store(false, Ordering::Relaxed);
        true
    }
    fn tool_tip(&self) -> ksni::ToolTip {
        // Live summary (active tunnels + throughput) kept fresh by the poll
        // thread; falls back to a direct query if it hasn't run yet.
        let description = {
            let info = TRAY_INFO.lock().unwrap().clone();
            if info.is_empty() {
                let actives: Vec<String> = backend::list_tunnels()
                    .into_iter()
                    .filter(|t| t.active)
                    .map(|t| t.name)
                    .collect();
                if actives.is_empty() {
                    "No active tunnel".to_string()
                } else {
                    format!("Active: {}", actives.join(", "))
                }
            } else {
                info
            }
        };
        ksni::ToolTip {
            title: "WireGuard".into(),
            description,
            icon_name: "wireguard-gui".into(),
            icon_pixmap: Vec::new(),
        }
    }
    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::{CheckmarkItem, MenuItem, StandardItem, SubMenu};
        let tunnels = backend::list_tunnels();
        let any_active = tunnels.iter().any(|t| t.active);
        let active_names: Vec<String> = tunnels
            .iter()
            .filter(|t| t.active)
            .map(|t| t.name.clone())
            .collect();

        let mut items: Vec<ksni::MenuItem<Self>> = vec![
            // Status header (disabled label).
            StandardItem {
                label: if any_active {
                    format!("Connected: {}", active_names.join(", "))
                } else {
                    "Not connected".to_string()
                },
                enabled: false,
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Show WireGuard".into(),
                activate: Box::new(|this: &mut Self| {
                    let _ = this.window.upgrade_in_event_loop(|ui| {
                        let _ = ui.show();
                        ui.window().set_minimized(false);
                    });
                }),
                ..Default::default()
            }
            .into(),
        ];

        // A "Tunnels" submenu: one checkable toggle per tunnel.
        let toggles: Vec<ksni::MenuItem<Self>> = tunnels
            .iter()
            .map(|t| {
                let name = t.name.clone();
                CheckmarkItem {
                    label: name.clone(),
                    checked: t.active,
                    activate: Box::new(move |_: &mut Self| {
                        // The tray label can be up to a refresh-interval stale;
                        // re-check live state at click time so we never run the
                        // wrong action (or double-activate) on a captured flag.
                        let up = backend::list_tunnels()
                            .iter()
                            .any(|x| x.name == name && x.active);
                        let _ = if up {
                            backend::deactivate(&name)
                        } else {
                            backend::activate(&name)
                        };
                    }),
                    ..Default::default()
                }
                .into()
            })
            .collect();
        items.push(
            SubMenu {
                label: "Tunnels".into(),
                submenu: toggles,
                ..Default::default()
            }
            .into(),
        );

        // Deactivate everything that's up.
        items.push(
            StandardItem {
                label: "Deactivate all".into(),
                enabled: any_active,
                activate: Box::new(move |_: &mut Self| {
                    for n in &active_names {
                        let _ = backend::deactivate(n);
                    }
                }),
                ..Default::default()
            }
            .into(),
        );

        items.push(MenuItem::Separator);
        items.push(
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|_: &mut Self| {
                    let _ = slint::invoke_from_event_loop(|| {
                        let _ = slint::quit_event_loop();
                    });
                }),
                ..Default::default()
            }
            .into(),
        );
        items
    }
}

/// Open a URL in the user's default browser.
fn open_url(url: &str) {
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// The in-app guide, embedded at build time. Slint can't render markdown, so the
/// Help window shows this raw text in a monospace view (still readable).
const HELP_TEXT: &str = include_str!("../docs/help.md");

/// The changelog, embedded at build time and shown in the Updates window.
const CHANGELOG_TEXT: &str = include_str!("../CHANGELOG.md");

/// Split a document into a `[string]` line model for the virtualized ListView
/// doc views (Log / changelog / Help / Diagnostics). One Text row per line keeps
/// scrolling O(visible) and avoids the read-only-TextEdit relayout/blink-Timer.
fn lines_model(text: &str) -> ModelRc<SharedString> {
    let rows: Vec<SharedString> = text.lines().map(SharedString::from).collect();
    ModelRc::new(VecModel::from(rows))
}

/// Build a single-line model from one placeholder/status string (never-blank
/// states for the Log view).
fn one_line_model(text: &str) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(vec![SharedString::from(text)]))
}

/// Strip the inline markdown markers Slint can't render (`**bold**`, `` `code` ``,
/// `*italic*`, and `[label](url)` links) down to their visible text, so a Help /
/// changelog line reads cleanly. Hand-rolled (no regex crate): we drop `*` and
/// `` ` `` run markers and rewrite links to their label. This is intentionally
/// lenient — it's display-only text, not a parser, so unbalanced markers just
/// lose their stray symbol rather than failing.
fn strip_inline_md(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        match c {
            // Drop emphasis/code markers (`*`, `**`, `` ` ``) — keep the text.
            b'*' | b'`' => {
                i += 1;
                // Collapse a doubled `**` into one skip.
                if i < bytes.len() && bytes[i] == c {
                    i += 1;
                }
            }
            // `[label](url)` -> `label`. Only rewrite a well-formed link; an
            // unmatched `[` is emitted verbatim.
            b'[' => {
                if let Some((label, consumed)) = parse_md_link(&s[i..]) {
                    out.push_str(&label);
                    i += consumed;
                } else {
                    out.push('[');
                    i += 1;
                }
            }
            _ => {
                // Copy this whole UTF-8 char (markers above are all ASCII, so
                // multi-byte chars only ever hit this arm).
                let ch_len = utf8_char_len(c);
                let end = (i + ch_len).min(bytes.len());
                out.push_str(&s[i..end]);
                i = end;
            }
        }
    }
    out
}

/// Byte length of a UTF-8 char from its leading byte.
fn utf8_char_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else if b >> 3 == 0b11110 {
        4
    } else {
        1 // stray continuation byte — advance one to make progress
    }
}

/// Parse a `[label](url)` markdown link at the start of `s`. Returns the label
/// and the number of bytes consumed (the whole `[label](url)`), or None if `s`
/// doesn't start with a complete link.
fn parse_md_link(s: &str) -> Option<(String, usize)> {
    let bytes = s.as_bytes();
    if bytes.first() != Some(&b'[') {
        return None;
    }
    let close = s.find("](")?;
    let label = &s[1..close];
    // Labels don't span lines or contain a nested `[`.
    if label.contains('[') || label.contains('\n') {
        return None;
    }
    let after = &s[close + 2..];
    let paren = after.find(')')?;
    if after[..paren].contains('\n') {
        return None;
    }
    Some((label.to_string(), close + 2 + paren + 1))
}

/// Lightweight per-line markdown for the Help & changelog doc views: tag each
/// line with a `kind` MdRow styles. We don't render full markdown — just enough
/// (headings, bullets, blockquotes, fenced code, rules) to drop the literal
/// `#`/`**`/backtick noise. Inline markers are stripped for every kind except
/// code (which is shown verbatim).
fn md_items(src: &str) -> Vec<DocItem> {
    let mut items = Vec::new();
    let mut in_code = false;
    for raw in src.lines() {
        let trimmed = raw.trim();
        // Fenced code: ``` toggles the block; the fence lines themselves are
        // not emitted.
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            // Verbatim — keep indentation, don't strip inline markers.
            items.push(DocItem {
                text: raw.into(),
                kind: "code".into(),
            });
            continue;
        }
        let (kind, text): (&str, String) =
            if trimmed == "---" || trimmed == "***" || trimmed == "___" {
                ("rule", String::new())
            } else if let Some(rest) = trimmed.strip_prefix("### ") {
                ("h3", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("## ") {
                ("h2", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("# ") {
                ("h1", strip_inline_md(rest))
            } else if let Some(rest) = trimmed.strip_prefix("> ") {
                ("quote", strip_inline_md(rest))
            } else if trimmed.is_empty() {
                ("blank", String::new())
            } else if let Some(rest) = bullet_rest(raw) {
                // Nested bullets (indented 2+ spaces) get a "  ◦ " hollow marker;
                // top-level ones a "• " filled marker.
                let nested = raw.len() - raw.trim_start().len() >= 2;
                let marker = if nested { "  ◦ " } else { "• " };
                ("bullet", format!("{marker}{}", strip_inline_md(rest)))
            } else {
                ("normal", strip_inline_md(trimmed))
            };
        items.push(DocItem {
            text: text.into(),
            kind: kind.into(),
        });
    }
    items
}

/// If `line` is a markdown bullet (`- ` or `* ` after optional indentation),
/// return the text after the marker. Avoids treating a `***` rule (already
/// handled) or a bare `*` as a bullet.
fn bullet_rest(line: &str) -> Option<&str> {
    let t = line.trim_start();
    t.strip_prefix("- ").or_else(|| t.strip_prefix("* "))
}

/// Build the `[DocItem]` model for a markdown doc view (Help / changelog).
fn md_items_model(src: &str) -> ModelRc<DocItem> {
    ModelRc::new(VecModel::from(md_items(src)))
}

/// Briefly show an inline "Copied!" confirmation in a doc window (Help / Update
/// / Diag), then clear it. These windows have no main status bar, so a tiny
/// transient hint stands in. `set_hint` writes the window's `copied-hint`.
fn flash_copied_hint<W, F>(win: &W, set_hint: F, msg: &str)
where
    W: ComponentHandle + 'static,
    F: Fn(&W, SharedString) + Copy + 'static,
{
    set_hint(win, msg.into());
    let weak = win.as_weak();
    let timer = Timer::default();
    timer.start(
        TimerMode::SingleShot,
        Duration::from_millis(1500),
        move || {
            if let Some(w) = weak.upgrade() {
                set_hint(&w, SharedString::new());
            }
        },
    );
    // Leak the timer so it lives long enough to fire (the window outlives it).
    std::mem::forget(timer);
}

/// Open (or re-open) the in-app Help window with the embedded guide.
fn show_help_window() {
    let Ok(win) = HelpWindow::new() else { return };
    win.set_items(md_items_model(HELP_TEXT));
    win.set_full_text(HELP_TEXT.into());
    {
        let hw = win.as_weak();
        win.on_close_me(move || {
            if let Some(h) = hw.upgrade() {
                let _ = h.hide();
            }
        });
    }
    {
        let hw = win.as_weak();
        win.on_copy_line(move |line| {
            if copy_to_clipboard(&line)
                && let Some(h) = hw.upgrade()
            {
                flash_copied_hint(&h, |w, m| w.set_copied_hint(m), "Copied!");
            }
        });
    }
    {
        let hw = win.as_weak();
        win.on_copy_all(move || {
            let Some(h) = hw.upgrade() else { return };
            if copy_to_clipboard(&h.get_full_text()) {
                flash_copied_hint(&h, |w, m| w.set_copied_hint(m), "Copied all!");
            }
        });
    }
    let _ = win.show();
    HELPWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

/// Kick off a background `update::check()` for an open Updates window, posting
/// the status + update-available flag back on the UI thread.
fn run_update_check(win: &UpdateWindow) {
    win.set_status("Checking…".into());
    win.set_update_available(false);
    let ww = win.as_weak();
    std::thread::spawn(move || {
        let found = update::check();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = ww.upgrade() else { return };
            match found {
                Ok(Some(info)) => {
                    w.set_status(
                        format!("Update available: v{} → v{}", info.current, info.latest).into(),
                    );
                    w.set_update_available(true);
                }
                Ok(None) => {
                    w.set_status(
                        format!("You are on the latest version (v{}).", w.get_installed()).into(),
                    );
                    w.set_update_available(false);
                }
                Err(_) => {
                    w.set_status("Couldn't check for updates (offline?).".into());
                    w.set_update_available(false);
                }
            }
        });
    });
}

/// Trim the dev-facing Keep-a-Changelog header/intro and the (usually empty)
/// `[Unreleased]` section for the in-app view — end users just want the released
/// versions, so start at the first `## [x.y.z]` heading.
fn changelog_for_display(src: &str) -> String {
    for (i, line) in src.lines().enumerate() {
        let t = line.trim_start();
        if t.starts_with("## [") && t.as_bytes().get(4).is_some_and(u8::is_ascii_digit) {
            return src.lines().skip(i).collect::<Vec<_>>().join("\n");
        }
    }
    src.to_string()
}

/// Open (or re-open) the Updates window: shows the installed version + embedded
/// changelog, then kicks off a background check.
fn show_update_window() {
    let Ok(win) = UpdateWindow::new() else { return };
    win.set_installed(env!("CARGO_PKG_VERSION").into());
    let log = changelog_for_display(CHANGELOG_TEXT);
    win.set_changelog_items(md_items_model(&log));
    win.set_changelog_text(log.into());
    {
        let uw = win.as_weak();
        win.on_close_me(move || {
            if let Some(u) = uw.upgrade() {
                let _ = u.hide();
            }
        });
    }
    {
        let uw = win.as_weak();
        win.on_copy_line(move |line| {
            if copy_to_clipboard(&line)
                && let Some(u) = uw.upgrade()
            {
                flash_copied_hint(&u, |w, m| w.set_copied_hint(m), "Copied!");
            }
        });
    }
    {
        let uw = win.as_weak();
        win.on_copy_all(move || {
            let Some(u) = uw.upgrade() else { return };
            if copy_to_clipboard(&u.get_changelog_text()) {
                flash_copied_hint(&u, |w, m| w.set_copied_hint(m), "Copied all!");
            }
        });
    }
    {
        let uw = win.as_weak();
        win.on_check_updates(move || {
            if let Some(u) = uw.upgrade() {
                run_update_check(&u);
            }
        });
    }
    {
        let uw = win.as_weak();
        win.on_do_update(move || {
            let Some(u) = uw.upgrade() else { return };
            u.set_status("Downloading and verifying the update…".into());
            u.set_update_available(false);
            let uw = u.as_weak();
            std::thread::spawn(move || {
                let result = update::download_and_verify().and_then(|dir| update::apply(&dir));
                let msg = match result {
                    Ok(()) => "Update installed — restart wireguard-gui to apply.".to_string(),
                    Err(e) => format!("Update failed: {e}"),
                };
                let uw = uw.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(u) = uw.upgrade() {
                        u.set_status(msg.into());
                    }
                });
            });
        });
    }
    run_update_check(&win);
    let _ = win.show();
    UPDATEWIN.with(|slot| *slot.borrow_mut() = Some(win));
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
fn load_show_advanced() -> bool {
    match mode_state_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => matches!(s.trim(), "expanded" | "advanced"),
        None => false,
    }
}

/// Persist the disclosure state so a power user who expands it keeps it.
fn save_show_advanced(expanded: bool) {
    if let Some(p) = mode_state_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, if expanded { "expanded" } else { "collapsed" });
    }
}

/// Copy text to the system clipboard. Returns whether it succeeded.
fn copy_to_clipboard(text: &str) -> bool {
    // Native clipboard via arboard (the Wayland backend is enabled in Cargo.toml
    // via the `wayland-data-control` feature). Cache the handle so the served
    // selection persists for the app's lifetime.
    let via_arboard = CLIPBOARD.with(|c| {
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
    copy_via_cli(text)
}

/// Last-resort clipboard: pipe `text` into a clipboard CLI. Tries the
/// session-appropriate tool first and never blocks (wl-copy/xclip daemonize to
/// serve the selection and the foreground process exits immediately).
fn copy_via_cli(text: &str) -> bool {
    use std::io::Write as _;
    use std::process::{Command, Stdio};

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

    for (prog, args) in candidates {
        let Ok(mut child) = Command::new(prog)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
            // Drop stdin to signal EOF so the tool stops reading and serves.
        }
        if child.wait().map(|s| s.success()).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Replace (or insert) the `PrivateKey` line in a config with `key`.
fn set_private_key(config: &str, key: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in config.lines() {
        let t = line.trim_start().to_ascii_lowercase();
        if !replaced && t.starts_with("privatekey") && line.contains('=') {
            out.push_str(&format!("PrivateKey = {key}\n"));
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if replaced {
        return out;
    }
    // No PrivateKey line: insert after the first [Interface], else prepend.
    if let Some(pos) = out.to_ascii_lowercase().find("[interface]") {
        let nl = out[pos..]
            .find('\n')
            .map(|n| pos + n + 1)
            .unwrap_or(out.len());
        out.insert_str(nl, &format!("PrivateKey = {key}\n"));
        out
    } else {
        format!("[Interface]\nPrivateKey = {key}\n{out}")
    }
}

/// Replace (or insert) the `PresharedKey` line in a config with `key`.
fn set_psk(config: &str, key: &str) -> String {
    let mut out = String::new();
    let mut replaced = false;
    for line in config.lines() {
        let t = line.trim_start().to_ascii_lowercase();
        if !replaced && t.starts_with("presharedkey") && line.contains('=') {
            out.push_str(&format!("PresharedKey = {key}\n"));
            replaced = true;
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    if replaced {
        return out;
    }
    // Insert right after the first peer's PublicKey line, else append.
    let mut result = String::new();
    let mut inserted = false;
    for line in out.lines() {
        result.push_str(line);
        result.push('\n');
        if !inserted
            && line
                .trim_start()
                .to_ascii_lowercase()
                .starts_with("publickey")
        {
            result.push_str(&format!("PresharedKey = {key}\n"));
            inserted = true;
        }
    }
    if !inserted {
        result.push_str(&format!("PresharedKey = {key}\n"));
    }
    result
}

/// Push parsed fields into the editor's form properties.
fn apply_fields(ed: &EditWindow, f: &Fields, requested_peer: usize) {
    let mut f = f.clone();
    f.ensure_peer();
    let idx = requested_peer.min(f.peers.len() - 1);
    let peer = &f.peers[idx];
    ed.set_f_private_key(f.private_key.clone().into());
    ed.set_f_address(f.address.clone().into());
    ed.set_f_dns(f.dns.clone().into());
    ed.set_f_listen_port(f.listen_port.clone().into());
    ed.set_f_mtu(f.mtu.clone().into());
    ed.set_peer_index(idx as i32);
    ed.set_peer_count(f.peers.len() as i32);
    ed.set_peer_label(format!("{} of {}", idx + 1, f.peers.len()).into());
    ed.set_f_peer_public_key(peer.peer_public_key.clone().into());
    ed.set_f_preshared_key(peer.preshared_key.clone().into());
    ed.set_f_allowed_ips(peer.allowed_ips.clone().into());
    ed.set_f_endpoint(peer.endpoint.clone().into());
    ed.set_f_keepalive(peer.keepalive.clone().into());
}

/// Read the editor's form properties back into `Fields`, preserving peers that
/// are not currently visible in the form.
fn read_fields(ed: &EditWindow) -> Fields {
    let mut f = config_to_fields(&ed.get_config_text());
    f.ensure_peer();
    f.private_key = ed.get_f_private_key().to_string();
    f.address = ed.get_f_address().to_string();
    f.dns = ed.get_f_dns().to_string();
    f.listen_port = ed.get_f_listen_port().to_string();
    f.mtu = ed.get_f_mtu().to_string();
    let idx = (ed.get_peer_index().max(0) as usize).min(f.peers.len() - 1);
    f.peers[idx] = PeerFields {
        peer_public_key: ed.get_f_peer_public_key().to_string(),
        preshared_key: ed.get_f_preshared_key().to_string(),
        allowed_ips: ed.get_f_allowed_ips().to_string(),
        endpoint: ed.get_f_endpoint().to_string(),
        keepalive: ed.get_f_keepalive().to_string(),
    };
    f
}

fn commit_form_to_config(ed: &EditWindow) -> Option<String> {
    if !form_representable(&ed.get_config_text()) {
        return None;
    }
    let cfg = fields_to_config(&read_fields(ed));
    ed.set_public_key(backend::public_key_for_config(&cfg).into());
    ed.set_config_text(cfg.clone().into());
    Some(cfg)
}

fn switch_editor_peer(ed: &EditWindow, requested_peer: usize) {
    let Some(cfg) = commit_form_to_config(ed) else {
        return;
    };
    let fields = config_to_fields(&cfg);
    apply_fields(ed, &fields, requested_peer);
}

#[allow(dead_code)]
fn single_peer_fields(ed: &EditWindow) -> Fields {
    Fields {
        private_key: ed.get_f_private_key().to_string(),
        address: ed.get_f_address().to_string(),
        dns: ed.get_f_dns().to_string(),
        listen_port: ed.get_f_listen_port().to_string(),
        mtu: ed.get_f_mtu().to_string(),
        peers: vec![PeerFields {
            peer_public_key: ed.get_f_peer_public_key().to_string(),
            preshared_key: ed.get_f_preshared_key().to_string(),
            allowed_ips: ed.get_f_allowed_ips().to_string(),
            endpoint: ed.get_f_endpoint().to_string(),
            keepalive: ed.get_f_keepalive().to_string(),
        }],
    }
}

/// Render a string to a black-on-white QR-code Slint image.
fn qr_image(text: &str) -> slint::Image {
    use slint::{Rgba8Pixel, SharedPixelBuffer};
    let (scale, quiet) = (8usize, 4usize);
    let code = match qrcode::QrCode::new(text.as_bytes()) {
        Ok(c) => c,
        Err(_) => return slint::Image::default(),
    };
    let modules = code.width();
    let colors = code.to_colors();
    let px = (modules + 2 * quiet) * scale;
    let mut buf = SharedPixelBuffer::<Rgba8Pixel>::new(px as u32, px as u32);
    let bytes = buf.make_mut_bytes();
    for y in 0..px {
        for x in 0..px {
            let mx = (x / scale).wrapping_sub(quiet);
            let my = (y / scale).wrapping_sub(quiet);
            let dark =
                mx < modules && my < modules && colors[my * modules + mx] == qrcode::Color::Dark;
            let v = if dark { 0u8 } else { 255u8 };
            let i = (y * px + x) * 4;
            bytes[i] = v;
            bytes[i + 1] = v;
            bytes[i + 2] = v;
            bytes[i + 3] = 255;
        }
    }
    slint::Image::from_rgba8(buf)
}

/// Open the tunnel editor in its own top-level window. A fresh window paints
/// cleanly on this Wayland setup, so its text fields show up immediately.
fn open_editor(
    main: &MainWindow,
    is_new: bool,
    orig_name: String,
    prefill_name: String,
    text: String,
) {
    let ed = match EditWindow::new() {
        Ok(e) => e,
        Err(e) => {
            set_status(main, format!("Couldn't open editor: {e}"));
            return;
        }
    };
    ed.set_is_new(is_new);
    ed.set_tunnel_name(prefill_name.into());
    if backend::config_runs_scripts(&text) {
        ed.set_warning(
            "⚠ This config runs commands as root on activation (PostUp/PreUp/…). \
             Only save it if you trust the source."
                .into(),
        );
    }
    ed.set_public_key(backend::public_key_for_config(&text).into());
    ed.set_config_text(text.clone().into());

    // New tunnels open in the structured form; existing ones in raw-text mode
    // (so a hand-tuned config is never silently rewritten on open). Either way,
    // populate the form fields from the starting config.
    apply_fields(&ed, &config_to_fields(&text), 0);
    // New tunnels open in the form; existing ones in raw text. Never open the
    // form for a config the form can't represent (scripts / Table / unknown
    // keys), or saving would silently drop those parts.
    ed.set_form_mode(is_new && form_representable(&text));

    // Live-update the public key as the config is edited.
    {
        let edw = ed.as_weak();
        ed.on_config_changed(move |cfg| {
            if let Some(ed) = edw.upgrade() {
                ed.set_public_key(backend::public_key_for_config(&cfg).into());
            }
        });
    }

    // Form edits: rebuild the raw config (the source of truth for Save) and
    // refresh the live public key.
    {
        let edw = ed.as_weak();
        ed.on_fields_changed(move || {
            if let Some(ed) = edw.upgrade() {
                let _ = commit_form_to_config(&ed);
            }
        });
    }

    // Toggle between the form and the raw-text views, converting as we go so no
    // edits are lost in either direction.
    {
        let edw = ed.as_weak();
        ed.on_switch_mode(move |to_form| {
            let Some(ed) = edw.upgrade() else { return };
            if to_form {
                // Refuse to enter the form for configs it can't represent -
                // keep raw text so nothing is dropped, and say why.
                if !form_representable(&ed.get_config_text()) {
                    ed.set_warning(
                        "This config has parts the form can't show (PostUp/Table, \
                         unknown keys, ...). Edit it as Config text."
                            .into(),
                    );
                    ed.set_form_mode(false);
                    return;
                }
                let idx = ed.get_peer_index().max(0) as usize;
                apply_fields(&ed, &config_to_fields(&ed.get_config_text()), idx);
            } else {
                let _ = commit_form_to_config(&ed);
            }
            ed.set_form_mode(to_form);
        });
    }

    // Multi-peer form navigation. The raw config remains the source of truth;
    // each navigation commits the current peer first so peer edits are not lost.
    {
        let edw = ed.as_weak();
        ed.on_peer_prev(move || {
            let Some(ed) = edw.upgrade() else { return };
            let idx = ed.get_peer_index().max(0) as usize;
            switch_editor_peer(&ed, idx.saturating_sub(1));
        });
    }
    {
        let edw = ed.as_weak();
        ed.on_peer_next(move || {
            let Some(ed) = edw.upgrade() else { return };
            let idx = ed.get_peer_index().max(0) as usize;
            switch_editor_peer(&ed, idx + 1);
        });
    }
    {
        let edw = ed.as_weak();
        ed.on_peer_add(move || {
            let Some(ed) = edw.upgrade() else { return };
            if !form_representable(&ed.get_config_text()) {
                return;
            }
            let mut fields = read_fields(&ed);
            fields.peers.push(PeerFields::default());
            let idx = fields.peers.len() - 1;
            let cfg = fields_to_config(&fields);
            ed.set_public_key(backend::public_key_for_config(&cfg).into());
            ed.set_config_text(cfg.into());
            apply_fields(&ed, &fields, idx);
        });
    }
    {
        let edw = ed.as_weak();
        ed.on_peer_remove(move || {
            let Some(ed) = edw.upgrade() else { return };
            if !form_representable(&ed.get_config_text()) {
                return;
            }
            let mut fields = read_fields(&ed);
            if fields.peers.len() <= 1 {
                return;
            }
            let idx = (ed.get_peer_index().max(0) as usize).min(fields.peers.len() - 1);
            fields.peers.remove(idx);
            let next = idx.min(fields.peers.len() - 1);
            let cfg = fields_to_config(&fields);
            ed.set_public_key(backend::public_key_for_config(&cfg).into());
            ed.set_config_text(cfg.into());
            apply_fields(&ed, &fields, next);
        });
    }

    // Create presets for Easy Mode. Each preset regenerates a keypair, then keeps
    // the explicit review/save step in the editor.
    {
        let edw = ed.as_weak();
        ed.on_create_preset(move |preset| {
            let Some(ed) = edw.upgrade() else { return };
            let kind = match preset.as_str() {
                "interface" => create::TunnelTemplateKind::InterfaceOnly,
                "split" => create::TunnelTemplateKind::ClientSplitTunnel,
                _ => create::TunnelTemplateKind::ClientFullTunnel,
            };
            let (private_key, public_key) = match backend::generate_keypair() {
                Ok(keys) => keys,
                Err(e) => {
                    ed.set_error(format!("Key generation failed: {e}").into());
                    return;
                }
            };
            let cfg = create::generate_template(kind, &private_key);
            ed.set_config_text(cfg.clone().into());
            ed.set_public_key(public_key.into());
            apply_fields(&ed, &config_to_fields(&cfg), 0);
            ed.set_form_mode(form_representable(&cfg));
            ed.set_error("".into());
            ed.set_warning(
                format!("{} preset loaded. Review before creating.", kind.label()).into(),
            );
        });
    }

    // Copy (public key / config) to the clipboard.
    ed.on_copy(move |t| {
        copy_to_clipboard(&t);
    });

    // Header "? Help" link: open the in-app guide.
    ed.on_show_help(show_help_window);

    // Cancel: just hide (the strong handle lives in EDITOR until next open).
    {
        let edw = ed.as_weak();
        ed.on_cancel(move || {
            if let Some(ed) = edw.upgrade() {
                let _ = ed.hide();
            }
        });
    }

    // Save: validate, write, handle rename, optionally activate, then close +
    // refresh the main view.
    let save_handler = Rc::new({
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        let orig = Rc::new(orig_name);
        move |name: SharedString, text: SharedString, activate_after: bool| {
            let Some(ed) = edw.upgrade() else { return };
            ed.set_error("".into());
            let name = name.to_string();
            let name = name.trim();
            if let Err(e) = backend::validate_tunnel_name(name) {
                ed.set_error(e.into());
                return;
            }
            let name = name.to_string();
            let text = text.to_string();
            if let Err(e) = backend::validate_config(&text) {
                ed.set_error(e.into());
                return;
            }
            let orig_name = orig.as_str();
            let renaming = !orig_name.is_empty() && orig_name != name;
            if (renaming || orig_name.is_empty()) && backend::tunnel_exists(&name) {
                ed.set_error(format!("A tunnel named “{name}” already exists.").into());
                return;
            }
            let save_result = if renaming {
                backend::rename_config(orig_name, &name, &text)
            } else {
                backend::save_config(&name, &text)
            };
            if let Err(e) = save_result {
                ed.set_error(
                    format!("{} failed: {e}", if renaming { "Rename" } else { "Save" }).into(),
                );
                return;
            }
            if let Some(main) = mainw.upgrade() {
                if renaming {
                    set_status(&main, format!("Renamed {orig_name} → {name}"));
                } else if activate_after {
                    match backend::activate(&name) {
                        Ok(()) => set_status(&main, format!("Created and activated {name}")),
                        Err(e) => {
                            set_status(&main, format!("Created {name}, but activation failed: {e}"))
                        }
                    }
                } else {
                    // If the tunnel is up, apply the change live without dropping
                    // peer sessions (wg syncconf). wg-quick-only fields
                    // (Address/DNS/MTU/Table) still need a reconnect.
                    let active = backend::list_tunnels()
                        .iter()
                        .any(|t| t.name == name && t.active);
                    if active {
                        match backend::sync_running(&name) {
                            Ok(()) => set_status(&main, format!("Saved {name} (applied live)")),
                            Err(_) => set_status(
                                &main,
                                format!("Saved {name} — reconnect to apply Address/DNS/MTU/routes"),
                            ),
                        }
                    } else {
                        set_status(
                            &main,
                            if orig_name.is_empty() {
                                format!("Created {name}")
                            } else {
                                format!("Saved {name}")
                            },
                        );
                    }
                }
                refresh_list(&main);
                select_by_name(&main, &name);
            }
            let _ = ed.hide();
        }
    });
    {
        let save_handler = save_handler.clone();
        ed.on_save(move |name, text| save_handler(name, text, false));
    }
    {
        let save_handler = save_handler.clone();
        ed.on_save_and_activate(move |name, text| save_handler(name, text, true));
    }

    // Generate keypair: insert a fresh PrivateKey, show the public key.
    {
        let edw = ed.as_weak();
        ed.on_generate_key(move || {
            let Some(ed) = edw.upgrade() else { return };
            match backend::generate_keypair() {
                Ok((priv_k, pub_k)) => {
                    let updated = set_private_key(&ed.get_config_text(), &priv_k);
                    let idx = ed.get_peer_index().max(0) as usize;
                    apply_fields(&ed, &config_to_fields(&updated), idx);
                    ed.set_config_text(updated.into());
                    ed.set_public_key(pub_k.clone().into());
                    ed.set_error("".into());
                    ed.set_warning("New keypair generated.".into());
                }
                Err(e) => ed.set_error(format!("Key generation failed: {e}").into()),
            }
        });
    }

    // Generate a preshared key for the peer.
    {
        let edw = ed.as_weak();
        ed.on_generate_psk(move || {
            let Some(ed) = edw.upgrade() else { return };
            // A preshared key belongs to a [Peer]; without one it would dangle.
            if !ed.get_config_text().to_ascii_lowercase().contains("[peer]") {
                ed.set_error("Add a [Peer] before generating a preshared key.".into());
                return;
            }
            match backend::generate_psk() {
                Ok(psk) => {
                    let idx = ed.get_peer_index().max(0) as usize;
                    let updated = if ed.get_form_mode() && form_representable(&ed.get_config_text())
                    {
                        let mut fields = read_fields(&ed);
                        fields.ensure_peer();
                        let peer_idx = idx.min(fields.peers.len() - 1);
                        fields.peers[peer_idx].preshared_key = psk;
                        let cfg = fields_to_config(&fields);
                        apply_fields(&ed, &fields, peer_idx);
                        cfg
                    } else {
                        set_psk(&ed.get_config_text(), &psk)
                    };
                    ed.set_config_text(updated.into());
                    ed.set_error("".into());
                    ed.set_warning("New preshared key generated.".into());
                }
                Err(e) => ed.set_error(format!("PSK generation failed: {e}").into()),
            }
        });
    }

    let _ = ed.show();
    EDITOR.with(|slot| *slot.borrow_mut() = Some(ed));
}

/// Compact "seconds ago" → "12s" / "3m" / "2h".
fn fmt_ago(s: u64) -> String {
    if s < 60 {
        format!("{s}s")
    } else if s < 3600 {
        format!("{}m", s / 60)
    } else {
        format!("{}h", s / 3600)
    }
}

/// Connection-health text + whether it's healthy, from the handshake age.
fn health_str(active: bool, age: Option<u64>) -> (String, bool) {
    if !active {
        return (String::new(), false);
    }
    match age {
        Some(s) if s < 180 => (format!("OK (last handshake {} ago)", fmt_ago(s)), true),
        Some(s) => (format!("stale (last handshake {} ago)", fmt_ago(s)), false),
        None => ("waiting for handshake…".into(), false),
    }
}

fn to_slint_detail(d: backend::Detail) -> TunnelDetail {
    let (health, health_good) = health_str(d.active, d.handshake_age);
    let peers: Vec<PeerInfo> = d
        .peers
        .into_iter()
        .map(|p| PeerInfo {
            public_key: p.public_key.into(),
            preshared: p.preshared,
            allowed_ips: p.allowed_ips.into(),
            endpoint: p.endpoint.into(),
            keepalive: p.keepalive.into(),
            latest_handshake: p.latest_handshake.into(),
            transfer: p.transfer.into(),
        })
        .collect();
    TunnelDetail {
        name: d.name.into(),
        active: d.active,
        autostart: d.autostart,
        killswitch: d.killswitch,
        public_key: d.public_key.into(),
        listen_port: d.listen_port.into(),
        addresses: d.addresses.into(),
        dns: d.dns.into(),
        peers: ModelRc::new(VecModel::from(peers)),
        health: health.into(),
        health_good,
        speed: SharedString::new(),
    }
}

/// Rebuild the left-hand tunnel list, preserving the selected name if possible.
fn refresh_list(ui: &MainWindow) {
    let prev = if ui.get_has_selection() {
        Some(ui.get_detail().name.to_string())
    } else {
        None
    };

    let tunnels = backend::list_tunnels();
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

/// Re-apply the Log tab's filter + this-tunnel toggle to the stored raw log.
fn apply_log_filter(ui: &MainWindow) {
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
        .map(SharedString::from)
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
fn refresh_log_async(w: slint::Weak<MainWindow>) {
    std::thread::spawn(move || {
        let text = backend::get_log();
        let _ = w.upgrade_in_event_loop(move |ui| {
            RAW_LOG.with(|r| *r.borrow_mut() = text);
            apply_log_filter(&ui);
        });
    });
}

/// Rebuild the Backup tab's list model from disk.
fn populate_backups(ui: &MainWindow) {
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

fn load_detail(ui: &MainWindow, name: &str) {
    *SELECTED.lock().unwrap() = Some(name.to_string());
    let detail = backend::get_detail(name);
    ui.set_detail(to_slint_detail(detail));
    ui.set_has_selection(true);
}

/// Selected-tunnel name from the live model + selected index.
fn selected_name(ui: &MainWindow) -> Option<String> {
    let idx = ui.get_selected_index();
    if idx < 0 {
        return None;
    }
    ui.get_tunnels()
        .row_data(idx as usize)
        .map(|t| t.name.to_string())
}

fn set_status(ui: &MainWindow, msg: impl Into<SharedString>) {
    let msg: SharedString = msg.into();
    ui.set_status(msg.clone());
    // Auto-dismiss after a few seconds (clear only if it hasn't been replaced).
    let w = ui.as_weak();
    slint::Timer::single_shot(Duration::from_secs(4), move || {
        if let Some(ui) = w.upgrade()
            && ui.get_status() == msg
        {
            ui.set_status(SharedString::new());
        }
    });
}

/// Single-instance guard via an abstract Unix socket (auto-released when the
/// process exits — no stale lock files). If another instance already holds it,
/// nudge that instance to raise its window and return None so we exit quietly.
/// Otherwise return the listener; the primary keeps it alive and accepts pings.
fn single_instance_listener() -> Option<std::os::unix::net::UnixListener> {
    use std::io::Write as _;
    use std::os::linux::net::SocketAddrExt as _;
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};

    let uid = unsafe { libc::getuid() };
    let addr = SocketAddr::from_abstract_name(format!("wireguard-gui-{uid}")).ok()?;
    match UnixListener::bind_addr(&addr) {
        Ok(listener) => Some(listener),
        Err(_) => {
            // Already running — ask it to show its window, then bow out.
            if let Ok(mut s) = UnixStream::connect_addr(&addr) {
                let _ = s.write_all(b"show");
            }
            None
        }
    }
}

/// NVIDIA's proprietary driver stutters on native Wayland (frame-callback
/// present path); XWayland's X11/GLX path is smooth. If we're on NVIDIA +
/// Wayland and XWayland is available (DISPLAY set), drop WAYLAND_DISPLAY so
/// winit uses X11. Opt out with WG_FORCE_WAYLAND=1; non-NVIDIA GPUs are left on
/// native Wayland.
fn prefer_xwayland_on_nvidia() {
    if std::env::var_os("WG_FORCE_WAYLAND").is_some() {
        return;
    }
    let on_wayland = std::env::var_os("WAYLAND_DISPLAY").is_some();
    let have_xwayland = std::env::var_os("DISPLAY").is_some();
    let nvidia_proprietary = std::path::Path::new("/proc/driver/nvidia/version").exists();
    if on_wayland && have_xwayland && nvidia_proprietary {
        // SAFETY: single-threaded, at the very start of main() before any threads.
        unsafe { std::env::remove_var("WAYLAND_DISPLAY") };
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // NVIDIA + native Wayland stutters; fall back to XWayland before any
    // windowing/Slint init (and before any threads spawn).
    prefer_xwayland_on_nvidia();

    // Answer --version/--help without opening a window (useful in scripts).
    if let Some(arg) = std::env::args().nth(1) {
        match arg.as_str() {
            "-V" | "--version" => {
                println!("wireguard-gui {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "-h" | "--help" => {
                println!(
                    "wireguard-gui {} - a desktop GUI for managing WireGuard tunnels\n\n\
                     Usage: wireguard-gui            launch the app\n       \
                     wireguard-gui --version    print the version\n       \
                     wireguard-gui --help       show this help",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            other => {
                eprintln!("wireguard-gui: unknown argument '{other}' (try --help)");
                std::process::exit(2);
            }
        }
    }

    // Single-instance: if WireGuard is already running, raise the existing
    // window (the listener thread below) instead of opening a second window and
    // a second tray icon — then exit this launch.
    let instance = match single_instance_listener() {
        Some(listener) => listener,
        None => return Ok(()),
    };

    backend::init();

    let ui = MainWindow::new()?;

    // Match the Wayland app_id / X11 WM_CLASS to the .desktop file basename
    // (`wireguard-gui`) so the compositor maps the window to our .desktop and
    // shows the installed icon instead of a generic placeholder. Must be set
    // AFTER the platform is initialized (MainWindow::new does that) but BEFORE
    // the window is shown.
    let _ = slint::set_xdg_app_id("wireguard-gui");

    refresh_list(&ui);

    // A second launch connects to our abstract socket; raise the window for it.
    {
        let w = ui.as_weak();
        std::thread::spawn(move || {
            for conn in instance.incoming() {
                if conn.is_ok() {
                    let w = w.clone();
                    let _ = slint::invoke_from_event_loop(move || {
                        if let Some(ui) = w.upgrade() {
                            let _ = ui.show();
                            ui.window().set_minimized(false);
                        }
                    });
                }
            }
        });
    }

    // Pre-load the journal OFF the UI thread so the Log tab shows content the
    // instant it opens without ever blocking startup. apply_log_filter() runs
    // now to paint a "(loading…)" placeholder until the background fetch lands.
    apply_log_filter(&ui);
    refresh_log_async(ui.as_weak());

    // ---- Advanced disclosure — load the saved state + persist on toggle ----
    ui.set_show_advanced(load_show_advanced());
    ui.on_persist_advanced(move |expanded| {
        save_show_advanced(expanded);
    });

    // ---- close to tray: hide instead of quitting (Quit is on the tray) ----
    // …but only if a tray actually exists to restore the window from. With no
    // SNI host, hiding would strand the app with no window and no way to quit,
    // so close quits instead.
    ui.window().on_close_requested(|| {
        if TRAY_ONLINE.load(Ordering::Relaxed) {
            slint::CloseRequestResponse::HideWindow
        } else {
            let _ = slint::quit_event_loop();
            slint::CloseRequestResponse::HideWindow
        }
    });

    // ---- system-tray icon (best-effort; needs SNI support on the desktop,
    // e.g. KDE, or GNOME with the AppIndicator extension). Uses libdbus on its
    // own thread, independent of the zbus stack Slint already uses. ----
    use ksni::blocking::TrayMethods;
    // `assume_sni_available` keeps the service alive even if the SNI host isn't
    // up yet at startup; the watcher_online/offline callbacks then track it.
    let tray = Tray {
        window: ui.as_weak(),
    };
    if let Ok(tray_handle) = tray.assume_sni_available(true).spawn() {
        // Refresh the tray's tooltip/status periodically so it tracks live state.
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(3));
                let _ = tray_handle.update(|_| {});
            }
        });
    }

    // ---- select ----
    {
        let w = ui.as_weak();
        ui.on_select(move |idx| {
            let ui = w.unwrap();
            ui.set_selected_index(idx);
            if let Some(name) = selected_name(&ui) {
                load_detail(&ui, &name);
            }
        });
    }

    // ---- activate / deactivate ----
    {
        let w = ui.as_weak();
        ui.on_activate(move |name| {
            let ui = w.unwrap();
            match backend::activate(&name) {
                Ok(()) => {
                    // A wrong clock makes the server silently reject the handshake
                    // (TAI64N anti-replay): the tunnel reads "active" but never
                    // connects. Flag it up front, non-blocking, and point at Diagnose.
                    if doctor::clock_synced() == Some(false) {
                        set_status(
                            &ui,
                            "Activated — but the clock isn't NTP-synced; the handshake may be rejected. Use Diagnose.",
                        );
                    } else {
                        set_status(&ui, format!("Activated {name}"));
                    }
                }
                Err(e) => set_status(
                    &ui,
                    format!("Activate failed: {}", doctor::friendly_error(&e)),
                ),
            }
            load_detail(&ui, &name);
            refresh_list(&ui);
        });
    }
    {
        let w = ui.as_weak();
        ui.on_deactivate(move |name| {
            let ui = w.unwrap();
            match backend::deactivate(&name) {
                Ok(()) => set_status(&ui, format!("Deactivated {name}")),
                Err(e) => set_status(&ui, format!("Deactivate failed: {e}")),
            }
            load_detail(&ui, &name);
            refresh_list(&ui);
        });
    }

    // ---- diagnose a stuck connection ----
    // The report pings the endpoint / resolves DNS / shells out, so it MUST run
    // off the UI thread. We post the finished text back via the event loop and
    // show it in a simple read-only popup.
    {
        let w = ui.as_weak();
        ui.on_diagnose_conn(move |name| {
            let ui = w.unwrap();
            set_status(&ui, "Diagnosing…");
            std::thread::spawn(move || {
                let report = backend::diagnose_report(&name);
                let _ = slint::invoke_from_event_loop(move || {
                    let Ok(win) = DiagWindow::new() else { return };
                    win.set_report_lines(lines_model(&report));
                    win.set_full_text(report.as_str().into());
                    {
                        let dw = win.as_weak();
                        win.on_close_me(move || {
                            if let Some(d) = dw.upgrade() {
                                let _ = d.hide();
                            }
                        });
                    }
                    {
                        let dw = win.as_weak();
                        win.on_copy_line(move |line| {
                            if copy_to_clipboard(&line)
                                && let Some(d) = dw.upgrade()
                            {
                                flash_copied_hint(&d, |w, m| w.set_copied_hint(m), "Copied!");
                            }
                        });
                    }
                    {
                        let dw = win.as_weak();
                        win.on_copy_all(move || {
                            let Some(d) = dw.upgrade() else { return };
                            if copy_to_clipboard(&d.get_full_text()) {
                                flash_copied_hint(&d, |w, m| w.set_copied_hint(m), "Copied all!");
                            }
                        });
                    }
                    let _ = win.show();
                    DIAGWIN.with(|slot| *slot.borrow_mut() = Some(win));
                });
            });
        });
    }

    // ---- import from file ----
    {
        let w = ui.as_weak();
        ui.on_import_file(move || {
            let ui = w.unwrap();
            let files = rfd::FileDialog::new()
                .add_filter("WireGuard config", &["conf"])
                .set_title("Import tunnel(s) from file")
                .pick_files();
            let Some(files) = files else { return };

            let read = |path: &std::path::PathBuf| -> Option<(String, String)> {
                let content = std::fs::read_to_string(path).ok()?;
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "tunnel".into());
                Some((stem, content))
            };

            // Single file: open the editor pre-filled so the user can name it
            // (suggested name is already collision-free), then Save.
            if files.len() == 1 {
                match read(&files[0]) {
                    Some((stem, content)) => {
                        let suggested = backend::unique_name(&stem);
                        open_editor(&ui, true, String::new(), suggested, content);
                    }
                    None => set_status(&ui, "Couldn't read that file"),
                }
                return;
            }

            // Multiple files: import each with an auto-deduped name (never
            // overwrites an existing tunnel). Validate each, skip the invalid
            // ones, and flag any that run root scripts on activation.
            let mut last = None;
            let mut count = 0;
            let mut skipped = 0;
            let mut scripts = false;
            for path in &files {
                let Some((stem, content)) = read(path) else {
                    skipped += 1;
                    continue;
                };
                if backend::validate_config(&content).is_err() {
                    skipped += 1;
                    continue;
                }
                scripts |= backend::config_runs_scripts(&content);
                let name = backend::unique_name(&stem);
                match backend::save_config(&name, &content) {
                    Ok(()) => {
                        last = Some(name);
                        count += 1;
                    }
                    Err(_) => skipped += 1,
                }
            }
            if count > 0 {
                let warn = if scripts {
                    " — ⚠ some run scripts as root"
                } else {
                    ""
                };
                let skip = if skipped > 0 {
                    format!(", {skipped} skipped (invalid)")
                } else {
                    String::new()
                };
                set_status(&ui, format!("Imported {count} tunnel(s){skip}{warn}"));
            } else if skipped > 0 {
                set_status(&ui, format!("Nothing imported — {skipped} file(s) invalid"));
            }
            refresh_list(&ui);
            if let Some(name) = last {
                select_by_name(&ui, &name);
            }
        });
    }

    // ---- add empty tunnel ----
    {
        let w = ui.as_weak();
        ui.on_add_empty(move || {
            let ui = w.unwrap();
            // Pre-generate a keypair so a new tunnel opens ready, like the
            // WireGuard for Windows "Create new tunnel" dialog.
            let text = match backend::generate_keypair() {
                Ok((priv_k, _)) => {
                    create::generate_template(create::TunnelTemplateKind::ClientFullTunnel, &priv_k)
                }
                Err(_) => create::generate_template(
                    create::TunnelTemplateKind::ClientFullTunnel,
                    create::FALLBACK_PRIVATE_KEY,
                ),
            };
            open_editor(&ui, true, String::new(), String::new(), text);
        });
    }

    // ---- delete ----
    {
        let w = ui.as_weak();
        ui.on_delete_tunnel(move |name| {
            let ui = w.unwrap();
            if name.is_empty() {
                return;
            }
            match backend::delete(&name) {
                Ok(()) => set_status(&ui, format!("Removed {name}")),
                Err(e) => set_status(&ui, format!("Remove failed: {e}")),
            }
            ui.set_has_selection(false);
            ui.set_selected_index(-1);
            refresh_list(&ui);
        });
    }

    // ---- begin edit ----
    {
        let w = ui.as_weak();
        ui.on_begin_edit(move |name| {
            let ui = w.unwrap();
            match backend::read_config(&name) {
                Ok(text) => open_editor(&ui, false, name.to_string(), name.to_string(), text),
                Err(e) => set_status(&ui, format!("Open failed: {e}")),
            }
        });
    }

    // ---- Log tab: refresh / filter / save / clear ----
    {
        let w = ui.as_weak();
        ui.on_refresh_log(move || {
            refresh_log_async(w.clone());
        });
    }
    {
        let w = ui.as_weak();
        ui.on_apply_log_filter(move || apply_log_filter(&w.unwrap()));
    }
    {
        let w = ui.as_weak();
        ui.on_save_log(move || {
            let ui = w.unwrap();
            let Some(dest) = rfd::FileDialog::new()
                .set_title("Save log to a file")
                .set_file_name("wireguard-gui-log.txt")
                .save_file()
            else {
                return;
            };
            let text = RAW_LOG.with(|r| r.borrow().clone());
            match backend::save_text_to(&dest, &text) {
                Ok(()) => set_status(&ui, format!("Log saved to {}", dest.display())),
                Err(e) => set_status(&ui, format!("Save failed: {e}")),
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_copy_log(move || {
            let ui = w.unwrap();
            let text = RAW_LOG.with(|r| r.borrow().clone());
            if text.is_empty() {
                set_status(&ui, "Nothing to copy yet — press Refresh");
                return;
            }
            if copy_to_clipboard(&text) {
                set_status(&ui, "Copied the full log to clipboard");
            } else {
                set_status(&ui, "Couldn't access the clipboard");
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_copy_line(move |line| {
            let ui = w.unwrap();
            if copy_to_clipboard(&line) {
                set_status(&ui, "Copied line");
            } else {
                set_status(&ui, "Couldn't access the clipboard");
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_clear_log(move || {
            let ui = w.unwrap();
            RAW_LOG.with(|r| r.borrow_mut().clear());
            ui.set_log_lines(one_line_model(
                "(cleared — press Refresh to reload from the journal)",
            ));
        });
    }

    // ---- Backup tab: list / create / restore / delete / export ----
    {
        let w = ui.as_weak();
        ui.on_refresh_backups(move || populate_backups(&w.unwrap()));
    }
    {
        let w = ui.as_weak();
        ui.on_backup_create(move || {
            let ui = w.unwrap();
            match backend::create_backup() {
                Ok(info) => set_status(&ui, format!("Backed up {} tunnel(s)", info.count)),
                Err(e) => set_status(&ui, format!("Backup failed: {e}")),
            }
            populate_backups(&ui);
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_restore(move |name| {
            let ui = w.unwrap();
            let path = match backend::backup_dir() {
                Ok(d) => d.join(name.as_str()),
                Err(e) => {
                    set_status(&ui, format!("Restore failed: {e}"));
                    return;
                }
            };
            match backend::restore_backup(&path) {
                Ok(n) => set_status(&ui, format!("Restored {n} tunnel(s) from backup")),
                Err(e) => set_status(&ui, format!("Restore failed: {e}")),
            }
            refresh_list(&ui);
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_delete(move |name| {
            let ui = w.unwrap();
            if let Ok(d) = backend::backup_dir() {
                match backend::delete_backup(&d.join(name.as_str())) {
                    Ok(()) => set_status(&ui, "Backup deleted"),
                    Err(e) => set_status(&ui, format!("Delete failed: {e}")),
                }
            }
            populate_backups(&ui);
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_export(move |name| {
            let ui = w.unwrap();
            let src = match backend::backup_dir() {
                Ok(d) => d.join(name.as_str()),
                Err(e) => {
                    set_status(&ui, format!("Export failed: {e}"));
                    return;
                }
            };
            let Some(dest) = rfd::FileDialog::new()
                .set_title("Export backup")
                .set_file_name(name.as_str())
                .add_filter("Zip archive", &["zip"])
                .save_file()
            else {
                return;
            };
            match backend::export_backup_to(&src, &dest) {
                Ok(()) => set_status(&ui, format!("Exported to {}", dest.display())),
                Err(e) => set_status(&ui, format!("Export failed: {e}")),
            }
        });
    }

    // ---- export all tunnels to a .zip ----
    {
        let w = ui.as_weak();
        ui.on_export_zip(move || {
            let ui = w.unwrap();
            let Some(path) = rfd::FileDialog::new()
                .set_title("Export tunnels to a zip archive")
                .set_file_name("wireguard-tunnels.zip")
                .add_filter("Zip archive", &["zip"])
                .save_file()
            else {
                return;
            };
            match backend::export_zip(&path) {
                Ok(n) => set_status(&ui, format!("Exported {n} tunnel(s) to {}", path.display())),
                Err(e) => set_status(&ui, format!("Export failed: {e}")),
            }
        });
    }

    // ---- import a tunnel from a QR-code image ----
    {
        let w = ui.as_weak();
        ui.on_import_qr(move || {
            let ui = w.unwrap();
            let Some(path) = rfd::FileDialog::new()
                .set_title("Import tunnel from a QR-code image")
                .add_filter("Image", &["png", "jpg", "jpeg"])
                .pick_file()
            else {
                return;
            };
            match backend::decode_qr(&path) {
                Ok(text) => {
                    let stem = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "tunnel".into());
                    let name = backend::unique_name(&stem);
                    open_editor(&ui, true, String::new(), name, text);
                }
                Err(e) => set_status(&ui, format!("QR import failed: {e}")),
            }
        });
    }

    // ---- show a tunnel's config as a QR code ----
    {
        let w = ui.as_weak();
        ui.on_show_qr(move |name| {
            let ui = w.unwrap();
            let text = match backend::read_config(&name) {
                Ok(t) => t,
                Err(e) => {
                    set_status(&ui, format!("Couldn't read {name}: {e}"));
                    return;
                }
            };
            let Ok(win) = QrWindow::new() else { return };
            win.set_name(name.clone());
            win.set_qr(qr_image(&text));
            let _ = win.show();
            QRWIN.with(|slot| *slot.borrow_mut() = Some(win));
        });
    }

    // ---- toggle start-on-boot ----
    {
        let w = ui.as_weak();
        ui.on_set_autostart(move |name, on| {
            let ui = w.unwrap();
            match backend::set_autostart(&name, on) {
                Ok(()) => set_status(
                    &ui,
                    format!(
                        "{} autostart for {name}",
                        if on { "Enabled" } else { "Disabled" }
                    ),
                ),
                Err(e) => set_status(&ui, format!("Autostart change failed: {e}")),
            }
            load_detail(&ui, &name);
        });
    }

    // ---- toggle helper-managed firewall kill switch ----
    {
        let w = ui.as_weak();
        ui.on_set_killswitch(move |name, on| {
            let ui = w.unwrap();
            match backend::set_killswitch(&name, on) {
                Ok(()) => set_status(
                    &ui,
                    format!(
                        "{} kill switch for {name}",
                        if on { "Enabled" } else { "Disabled" }
                    ),
                ),
                Err(e) => set_status(&ui, format!("Kill switch change failed: {e}")),
            }
            load_detail(&ui, &name);
        });
    }

    // ---- copy single-field values to the clipboard ----
    {
        let w = ui.as_weak();
        ui.on_copy_value(move |text| {
            let ui = w.unwrap();
            let value = clipboard::normalize_single_field_copy_value(&text);
            if value.is_empty() {
                return;
            }
            if copy_to_clipboard(&value) {
                set_status(&ui, "Copied value to clipboard");
            } else {
                set_status(&ui, "Couldn't access the clipboard");
            }
        });
    }

    // ---- copy raw multiline payloads (logs/configs) to the clipboard ----
    {
        let w = ui.as_weak();
        ui.on_copy_raw(move |text| {
            let ui = w.unwrap();
            if text.is_empty() {
                return;
            }
            if copy_to_clipboard(&text) {
                set_status(&ui, "Copied to clipboard");
            } else {
                set_status(&ui, "Couldn't access the clipboard");
            }
        });
    }

    // ---- About window ----
    ui.on_show_about(move || {
        let Ok(about) = AboutWindow::new() else {
            return;
        };
        about.set_version(env!("CARGO_PKG_VERSION").into());
        about.on_open_url(|u| open_url(&u));
        {
            let aw = about.as_weak();
            about.on_close_me(move || {
                if let Some(a) = aw.upgrade() {
                    let _ = a.hide();
                }
            });
        }
        // ---- About → "Check for updates": open the unified Updates window
        // (current version, status, changelog, and the update actions). ----
        about.on_show_update(show_update_window);
        let _ = about.show();
        ABOUTWIN.with(|slot| *slot.borrow_mut() = Some(about));
    });

    // ---- Help window (in-app guide) ----
    ui.on_show_help(show_help_window);

    // ---- Updates window (status + changelog) ----
    ui.on_show_update(show_update_window);

    // ---- copy the live running config (wg showconf) ----
    {
        let w = ui.as_weak();
        ui.on_show_running(move |name| {
            let ui = w.unwrap();
            match backend::running_config(&name) {
                Ok(cfg) if !cfg.trim().is_empty() => {
                    copy_to_clipboard(&cfg);
                    set_status(&ui, format!("Copied {name}'s running config"));
                }
                Ok(_) => set_status(&ui, "No running config (is the tunnel up?)"),
                Err(e) => set_status(&ui, format!("showconf failed: {e}")),
            }
        });
    }

    // ---- save the live running state to disk (wg-quick save) ----
    {
        let w = ui.as_weak();
        ui.on_persist_live(move |name| {
            let ui = w.unwrap();
            match backend::persist_live(&name) {
                Ok(()) => set_status(&ui, format!("Saved {name}'s live state to disk")),
                Err(e) => set_status(&ui, format!("Save live failed: {e}")),
            }
            load_detail(&ui, &name);
        });
    }

    // ---- live polling: data on a BACKGROUND thread, applied on the UI thread ----
    // The background thread does all the blocking `wg`/`sudo` calls (no UI
    // stutter) and drops the result into LIVE. A cheap UI-thread timer then
    // applies it with set_detail — a real property change, so Slint always
    // repaints (request_redraw alone can stall on Wayland).
    std::thread::spawn(move || {
        // Previous (name, rx, tx, when) sample for the selected tunnel, to derive
        // a live throughput rate.
        let mut prev: Option<(String, u64, u64, std::time::Instant)> = None;
        loop {
            std::thread::sleep(Duration::from_secs(1));
            let name = SELECTED.lock().unwrap().clone();
            let tunnels = backend::list_tunnels();
            let actives: Vec<String> = tunnels
                .iter()
                .filter(|t| t.active)
                .map(|t| t.name.clone())
                .collect();
            let detail = name
                .as_ref()
                .filter(|n| tunnels.iter().any(|t| &t.name == *n))
                .map(|n| backend::get_detail(n));

            // Throughput for the selected tunnel from successive samples.
            let mut speed = String::new();
            match (name.as_ref(), detail.as_ref()) {
                (Some(n), Some(d)) => {
                    let now = std::time::Instant::now();
                    if let Some((pn, prx, ptx, pt)) = prev.as_ref()
                        && pn == n
                    {
                        let dt = now.duration_since(*pt).as_secs_f64();
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
                    prev = Some((n.clone(), d.rx_bytes, d.tx_bytes, now));
                }
                _ => prev = None,
            }

            // Tray tooltip summary.
            let tray = if actives.is_empty() {
                "No active tunnel".to_string()
            } else if speed.is_empty() {
                format!("Active: {}", actives.join(", "))
            } else {
                format!("Active: {} · {}", actives.join(", "), speed)
            };
            *TRAY_INFO.lock().unwrap() = tray;

            *LIVE.lock().unwrap() = Some(Live {
                name,
                detail,
                actives,
                speed,
            });
        }
    });

    // ---- Startup update check: detached thread, never blocks the UI. Gated by
    // update::disabled() (WG_NO_UPDATE_CHECK env + persistent opt-out file).
    // Offline / curl error → silent no-op. ----
    if !update::disabled() {
        let w = ui.as_weak();
        std::thread::spawn(move || {
            if let Ok(Some(info)) = update::check() {
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(ui) = w.upgrade() {
                        ui.set_update_banner(
                            format!("Update available: v{} → v{}", info.current, info.latest)
                                .into(),
                        );
                        ui.set_update_available(true);
                    }
                });
            }
        });
    }

    let live_timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        live_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let Some(ui) = w.upgrade() else { return };
                // Live status only matters on the Tunnels tab. Skip the apply +
                // repaint on the Log/Backup tabs so the per-second redraw never
                // churns the Log view — that churn made dragging a text
                // selection there feel laggy/janky.
                if ui.get_active_tab() != 0 {
                    return;
                }
                let Some(live) = LIVE.lock().unwrap().take() else {
                    return;
                };
                // Only apply detail if it still matches the current selection,
                // so a payload computed for a previously-selected tunnel can't
                // overwrite a selection the user just changed.
                let current = SELECTED.lock().unwrap().clone();
                if let Some(d) = live.detail
                    && live.name == current
                {
                    // "Active but not connected": up, yet no recent handshake
                    // (none ever, or older than 3 min). Drives the amber banner.
                    let stuck = d.active && d.handshake_age.is_none_or(|a| a > 180);
                    let mut det = to_slint_detail(d);
                    det.speed = live.speed.clone().into();
                    ui.set_detail(det);
                    ui.set_has_selection(true);
                    ui.set_conn_warning(
                        if stuck {
                            "Not connected: no handshake yet. Click Diagnose for why."
                        } else {
                            ""
                        }
                        .into(),
                    );
                }
                let model = ui.get_tunnels();
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        let want = live.actives.iter().any(|a| a == &row.name.to_string());
                        if row.active != want {
                            row.active = want;
                            model.set_row_data(i, row);
                        }
                    }
                }
                ui.window().request_redraw();
            },
        );
    }

    // (No periodic Log auto-refresh: re-pulling the journal on a timer ran
    // get_log() on the UI thread — which shells out to sudo+journalctl — and
    // re-set log-text underneath the user, stuttering the UI and clobbering an
    // in-progress text selection. The log loads on startup, on opening the Log
    // tab, and via the Refresh button, all off-thread now.)

    // First-run check: if something critical is missing, greet new users with a
    // friendly Setup wizard instead of an empty technical window. Otherwise go
    // straight to the (Easy-mode) main window.
    if doctor::system_check().critical_ok() {
        ui.show()?;
    } else {
        show_setup_wizard(&ui);
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}

/// Build the wizard's checklist model from a doctor report.
fn build_checks(report: &doctor::Report) -> ModelRc<CheckItem> {
    let items: Vec<CheckItem> = report
        .checks
        .iter()
        .map(|c| CheckItem {
            name: c.name.into(),
            detail: c.detail.clone().into(),
            ok: matches!(c.status, doctor::Status::Ok),
            warn: matches!(c.status, doctor::Status::Warning),
        })
        .collect();
    ModelRc::new(VecModel::from(items))
}

/// The "Show commands" text: the fix command for each non-OK check.
fn commands_text(report: &doctor::Report) -> String {
    let mut s = String::new();
    for c in &report.checks {
        if !matches!(c.status, doctor::Status::Ok)
            && let Some(fix) = &c.fix
        {
            s.push_str(&format!("# {}\n{}\n\n", c.name, fix));
        }
    }
    if s.is_empty() {
        "Everything looks good.".to_string()
    } else {
        s.trim_end().to_string()
    }
}

/// Show the first-run Setup wizard. The main window is shown when the user skips
/// or once the checks pass. Never auto-installs or connects without confirmation.
fn show_setup_wizard(main: &MainWindow) {
    let Ok(win) = SetupWindow::new() else {
        let _ = main.show();
        return;
    };
    let report = doctor::system_check();
    win.set_checks(build_checks(&report));
    win.set_commands(commands_text(&report).into());

    {
        let mw = main.as_weak();
        let sw = win.as_weak();
        win.on_skip(move || {
            if let Some(s) = sw.upgrade() {
                let _ = s.hide();
            }
            if let Some(m) = mw.upgrade() {
                let _ = m.show();
            }
        });
    }
    {
        let sw = win.as_weak();
        win.on_toggle_commands(move || {
            if let Some(s) = sw.upgrade() {
                s.set_show_commands(!s.get_show_commands());
            }
        });
    }
    {
        let mw = main.as_weak();
        let sw = win.as_weak();
        win.on_re_check(move || {
            let Some(s) = sw.upgrade() else { return };
            let report = doctor::system_check();
            s.set_checks(build_checks(&report));
            s.set_commands(commands_text(&report).into());
            if report.critical_ok() {
                let _ = s.hide();
                if let Some(m) = mw.upgrade() {
                    let _ = m.show();
                }
            } else {
                s.set_busy("Still missing something - see the list above.".into());
            }
        });
    }
    {
        let sw = win.as_weak();
        win.on_fix_automatically(move || {
            let Some(s) = sw.upgrade() else { return };
            // Only the safely-automatable steps: install wireguard-tools (+ a
            // resolvconf provider for DNS) and create /etc/wireguard. Never
            // installs the helper or touches configs.
            let mut steps: Vec<String> = Vec::new();
            if !(doctor::which("wg") && doctor::which("wg-quick"))
                && let Some(c) = doctor::install_tools_command()
            {
                steps.push(c);
            }
            if !doctor::dns_ok()
                && let Some(c) = doctor::install_resolvconf_command()
            {
                steps.push(c);
            }
            if !std::path::Path::new("/etc/wireguard").is_dir() {
                steps.push("install -d -m 700 /etc/wireguard".to_string());
            }
            if steps.is_empty() {
                s.set_busy(
                    "Nothing to auto-install here. Install the helper via the .deb / AUR / \
                     ./install.sh, then Re-check."
                        .into(),
                );
                return;
            }
            s.set_busy("Working - enter your password in the dialog that appears...".into());
            let script = steps.join(" && ");
            let sw2 = s.as_weak();
            std::thread::spawn(move || {
                let ok = std::process::Command::new("pkexec")
                    .arg("sh")
                    .arg("-c")
                    .arg(&script)
                    .status()
                    .map(|st| st.success())
                    .unwrap_or(false);
                let _ = sw2.upgrade_in_event_loop(move |s| {
                    let report = doctor::system_check();
                    s.set_checks(build_checks(&report));
                    s.set_commands(commands_text(&report).into());
                    s.set_busy(if ok {
                        "Done - click Re-check to continue.".into()
                    } else {
                        "Couldn't finish automatically - try Show commands and run them by hand."
                            .into()
                    });
                });
            });
        });
    }

    let _ = win.show();
    SETUPWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

fn select_by_name(ui: &MainWindow, name: &str) {
    let model = ui.get_tunnels();
    for i in 0..model.row_count() {
        if let Some(row) = model.row_data(i)
            && row.name == name
        {
            ui.set_selected_index(i as i32);
            load_detail(ui, name);
            return;
        }
    }
}

#[cfg(test)]
mod md_tests {
    use super::{md_items, strip_inline_md};

    /// Find the kind tag a given line text was assigned.
    fn kind_of<'a>(items: &'a [super::DocItem], text: &str) -> Option<&'a str> {
        items
            .iter()
            .find(|i| i.text == text)
            .map(|i| i.kind.as_str())
    }

    #[test]
    fn strips_bold_code_italic_markers() {
        assert_eq!(strip_inline_md("**bold** and `code`"), "bold and code");
        assert_eq!(strip_inline_md("an *italic* word"), "an italic word");
        // A standalone marker just vanishes (display-only, lenient).
        assert_eq!(strip_inline_md("a ** b"), "a  b");
    }

    #[test]
    fn rewrites_links_to_their_label() {
        assert_eq!(
            strip_inline_md("see [Keep a Changelog](https://example.com) here"),
            "see Keep a Changelog here"
        );
        // An unmatched bracket is left verbatim.
        assert_eq!(strip_inline_md("an [open bracket"), "an [open bracket");
    }

    #[test]
    fn preserves_non_ascii_text() {
        // Em dash and bullet glyphs must round-trip untouched.
        assert_eq!(strip_inline_md("a — b • c"), "a — b • c");
    }

    #[test]
    fn classifies_headings_rules_and_blanks() {
        let items = md_items("# Title\n## Sub\n### SubSub\n\n---\nplain line");
        assert_eq!(kind_of(&items, "Title"), Some("h1"));
        assert_eq!(kind_of(&items, "Sub"), Some("h2"));
        assert_eq!(kind_of(&items, "SubSub"), Some("h3"));
        // A blank and a rule both carry empty text; check by position/count.
        assert!(items.iter().any(|i| i.kind == "blank"));
        assert!(items.iter().any(|i| i.kind == "rule"));
        assert_eq!(kind_of(&items, "plain line"), Some("normal"));
    }

    #[test]
    fn bullets_get_markers_and_nesting() {
        let items = md_items("- top\n  - nested\n* star top");
        assert_eq!(kind_of(&items, "• top"), Some("bullet"));
        assert_eq!(kind_of(&items, "  ◦ nested"), Some("bullet"));
        assert_eq!(kind_of(&items, "• star top"), Some("bullet"));
    }

    #[test]
    fn quote_strips_marker() {
        let items = md_items("> a quoted **note**");
        assert_eq!(kind_of(&items, "a quoted note"), Some("quote"));
    }

    #[test]
    fn fenced_code_is_verbatim_and_skips_fences() {
        let items = md_items("```\nlet x = **not bold**;\n```\nafter");
        // The fence lines are dropped; the body keeps its markers verbatim.
        assert_eq!(kind_of(&items, "let x = **not bold**;"), Some("code"));
        assert!(!items.iter().any(|i| i.text.starts_with("```")));
        assert_eq!(kind_of(&items, "after"), Some("normal"));
    }

    #[test]
    fn real_docs_parse_without_panicking() {
        // Smoke: the embedded docs must parse and produce styled rows.
        let help = md_items(super::HELP_TEXT);
        let changelog = md_items(super::CHANGELOG_TEXT);
        assert!(help.iter().any(|i| i.kind == "h1"));
        assert!(changelog.iter().any(|i| i.kind == "h2"));
        // No emitted (non-code) line should still contain a literal `**`.
        assert!(
            !help
                .iter()
                .any(|i| i.kind != "code" && i.text.contains("**"))
        );
    }
}
