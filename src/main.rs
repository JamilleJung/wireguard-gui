// Hide the console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;
mod clipboard;
mod config;
mod create;
mod doctor;
mod secrets;
mod ui_bridge;
mod validation;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
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
    // Keeps the first-run Setup wizard alive while it's shown.
    static SETUPWIN: RefCell<Option<SetupWindow>> = const { RefCell::new(None) };
    // Long-lived clipboard handle (kept alive so the copied text persists).
    static CLIPBOARD: RefCell<Option<arboard::Clipboard>> = const { RefCell::new(None) };
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
    fn watcher_offine(&self) -> bool {
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

/// Path of the saved Easy/Advanced preference: $XDG_CONFIG_HOME (or ~/.config)
/// /wireguard-gui/mode.
fn mode_state_path() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wireguard-gui").join("mode"))
}

/// Load the saved mode. New users default to Easy mode.
fn load_easy() -> bool {
    match mode_state_path().and_then(|p| std::fs::read_to_string(p).ok()) {
        Some(s) => s.trim() != "advanced",
        None => true,
    }
}

/// Persist the mode so the choice sticks across runs.
fn save_easy(easy: bool) {
    if let Some(p) = mode_state_path() {
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(p, if easy { "easy" } else { "advanced" });
    }
}

/// Copy text to the system clipboard. Returns whether it succeeded.
fn copy_to_clipboard(text: &str) -> bool {
    CLIPBOARD.with(|c| {
        let mut c = c.borrow_mut();
        if c.is_none() {
            *c = arboard::Clipboard::new().ok();
        }
        c.as_mut()
            .map(|cb| cb.set_text(text.to_string()).is_ok())
            .unwrap_or(false)
    })
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
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

    backend::init();

    let ui = MainWindow::new()?;
    refresh_list(&ui);

    // ---- Easy mode (everyday actions only) — load + persist the preference ----
    ui.set_easy_mode(load_easy());
    {
        let w = ui.as_weak();
        ui.on_toggle_easy(move || {
            if let Some(ui) = w.upgrade() {
                let next = !ui.get_easy_mode();
                ui.set_easy_mode(next);
                save_easy(next);
            }
        });
    }

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
    let tray_service = ksni::TrayService::new(Tray {
        window: ui.as_weak(),
    });
    let tray_handle = tray_service.handle();
    tray_service.spawn();
    // Refresh the tray's tooltip/status periodically so it tracks live state.
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(3));
            tray_handle.update(|_| {});
        }
    });

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
                Ok(()) => set_status(&ui, format!("Activated {name}")),
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

    // ---- refresh the Log tab ----
    {
        let w = ui.as_weak();
        ui.on_refresh_log(move || {
            let ui = w.unwrap();
            ui.set_log_text(backend::get_log().into());
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
        let _ = about.show();
        ABOUTWIN.with(|slot| *slot.borrow_mut() = Some(about));
    });

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

    let live_timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        live_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let Some(ui) = w.upgrade() else { return };
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
                    let mut det = to_slint_detail(d);
                    det.speed = live.speed.clone().into();
                    ui.set_detail(det);
                    ui.set_has_selection(true);
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
