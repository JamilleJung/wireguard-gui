// Hide the console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use std::cell::RefCell;
use std::sync::Mutex;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

slint::include_modules!();

/// The currently-selected tunnel name, shared with the background poll thread
/// so it can fetch live status off the UI thread (keeps the UI smooth).
static SELECTED: Mutex<Option<String>> = Mutex::new(None);

thread_local! {
    // Keeps the currently-open editor window alive. Replaced (and the old one
    // dropped) on the next open; only ever touched on the UI thread.
    static EDITOR: RefCell<Option<EditWindow>> = const { RefCell::new(None) };
    // Keeps the currently-open QR window alive.
    static QRWIN: RefCell<Option<QrWindow>> = const { RefCell::new(None) };
    // Keeps the About window alive.
    static ABOUTWIN: RefCell<Option<AboutWindow>> = const { RefCell::new(None) };
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
    fn tool_tip(&self) -> ksni::ToolTip {
        let actives: Vec<String> = backend::list_tunnels()
            .into_iter()
            .filter(|t| t.active)
            .map(|t| t.name)
            .collect();
        let description = if actives.is_empty() {
            "No active tunnel".to_string()
        } else {
            format!("Active: {}", actives.join(", "))
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
                let active = t.active;
                CheckmarkItem {
                    label: name.clone(),
                    checked: active,
                    activate: Box::new(move |_: &mut Self| {
                        if active {
                            let _ = backend::deactivate(&name);
                        } else {
                            let _ = backend::activate(&name);
                        }
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
    ed.set_config_text(text.into());

    // Live-update the public key as the config is edited.
    {
        let edw = ed.as_weak();
        ed.on_config_changed(move |cfg| {
            if let Some(ed) = edw.upgrade() {
                ed.set_public_key(backend::public_key_for_config(&cfg).into());
            }
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

    // Save: validate, write, handle rename, then close + refresh the main view.
    {
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        let orig = orig_name;
        ed.on_save(move |name, text| {
            let Some(ed) = edw.upgrade() else { return };
            ed.set_error("".into());
            let name = backend::sanitize_name(name.trim());
            if name.is_empty() {
                ed.set_error("Tunnel name is required.".into());
                return;
            }
            if let Err(e) = backend::validate_config(&text) {
                ed.set_error(e.into());
                return;
            }
            let renaming = !orig.is_empty() && orig != name;
            if (renaming || orig.is_empty()) && backend::tunnel_exists(&name) {
                ed.set_error(format!("A tunnel named “{name}” already exists.").into());
                return;
            }
            if let Err(e) = backend::save_config(&name, &text) {
                ed.set_error(format!("Save failed: {e}").into());
                return;
            }
            if let Some(main) = mainw.upgrade() {
                if renaming {
                    let _ = backend::deactivate(&orig);
                    let _ = backend::delete(&orig);
                    set_status(&main, format!("Renamed {orig} → {name}"));
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
                        set_status(&main, format!("Saved {name}"));
                    }
                }
                refresh_list(&main);
                select_by_name(&main, &name);
            }
            let _ = ed.hide();
        });
    }

    // Generate keypair: insert a fresh PrivateKey, show the public key.
    {
        let edw = ed.as_weak();
        ed.on_generate_key(move || {
            let Some(ed) = edw.upgrade() else { return };
            match backend::generate_keypair() {
                Ok((priv_k, pub_k)) => {
                    let updated = set_private_key(&ed.get_config_text(), &priv_k);
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
            match backend::generate_psk() {
                Ok(psk) => {
                    let updated = set_psk(&ed.get_config_text(), &psk);
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

fn to_slint_detail(d: backend::Detail) -> TunnelDetail {
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
        public_key: d.public_key.into(),
        listen_port: d.listen_port.into(),
        addresses: d.addresses.into(),
        dns: d.dns.into(),
        peers: ModelRc::new(VecModel::from(peers)),
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
        if let Some(ui) = w.upgrade() {
            if ui.get_status() == msg {
                ui.set_status(SharedString::new());
            }
        }
    });
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    backend::init();

    let ui = MainWindow::new()?;
    refresh_list(&ui);

    // ---- system-tray icon (best-effort; needs SNI support on the desktop,
    // e.g. KDE, or GNOME with the AppIndicator extension). Uses libdbus on its
    // own thread, independent of the zbus stack Slint already uses. ----
    let tray_service = ksni::TrayService::new(Tray {
        window: ui.as_weak(),
    });
    let tray_handle = tray_service.handle();
    tray_service.spawn();
    // Refresh the tray's tooltip/status periodically so it tracks live state.
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3));
        tray_handle.update(|_| {});
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
                Err(e) => set_status(&ui, format!("Activate failed: {e}")),
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
            // overwrites an existing tunnel).
            let mut last = None;
            let mut count = 0;
            for path in &files {
                let Some((stem, content)) = read(path) else {
                    continue;
                };
                let name = backend::unique_name(&stem);
                match backend::save_config(&name, &content) {
                    Ok(()) => {
                        last = Some(name);
                        count += 1;
                    }
                    Err(e) => set_status(&ui, format!("Import failed: {e}")),
                }
            }
            if count > 0 {
                set_status(&ui, format!("Imported {count} tunnel(s)"));
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
                Ok((priv_k, _)) => new_tunnel_template(&priv_k),
                Err(_) => TEMPLATE.to_string(),
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

    // ---- copy text to the clipboard ----
    {
        let w = ui.as_weak();
        ui.on_copy_text(move |text| {
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

    // ---- live polling on a BACKGROUND thread ----
    // All `wg`/`sudo` subprocess calls happen here, off the UI thread, so the
    // interface never stutters. Results are pushed back via the event loop.
    {
        let w = ui.as_weak();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(1));
            let name = SELECTED.lock().unwrap().clone();

            // Blocking helper calls — fine here, we're not on the UI thread.
            let tunnels = backend::list_tunnels();
            let actives: std::collections::HashSet<String> = tunnels
                .iter()
                .filter(|t| t.active)
                .map(|t| t.name.clone())
                .collect();
            let detail = name
                .as_ref()
                .filter(|n| actives.contains(*n) || tunnels.iter().any(|t| &t.name == *n))
                .map(|n| backend::get_detail(n));

            let pushed = w.upgrade_in_event_loop(move |ui| {
                if let Some(d) = detail {
                    ui.set_detail(to_slint_detail(d));
                    ui.set_has_selection(true);
                }
                let model = ui.get_tunnels();
                for i in 0..model.row_count() {
                    if let Some(mut row) = model.row_data(i) {
                        let want = actives.contains(&row.name.to_string());
                        if row.active != want {
                            row.active = want;
                            model.set_row_data(i, row);
                        }
                    }
                }
                ui.window().request_redraw();
            });
            if pushed.is_err() {
                break; // UI is gone
            }
        });
    }

    // A cheap UI-thread timer that just asks for a repaint each second, so the
    // live status fields refresh even if a frame callback would otherwise stall.
    let redraw_timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        redraw_timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                if let Some(ui) = w.upgrade() {
                    ui.window().request_redraw();
                }
            },
        );
    }

    ui.run()?;
    Ok(())
}

fn select_by_name(ui: &MainWindow, name: &str) {
    let model = ui.get_tunnels();
    for i in 0..model.row_count() {
        if let Some(row) = model.row_data(i) {
            if row.name == name {
                ui.set_selected_index(i as i32);
                load_detail(ui, name);
                return;
            }
        }
    }
}

const TEMPLATE: &str = "[Interface]
PrivateKey = <your private key>
Address = 10.0.0.2/24
DNS = 1.1.1.1

[Peer]
PublicKey = <server public key>
AllowedIPs = 0.0.0.0/0
Endpoint = server.example.com:51820
PersistentKeepalive = 25
";

/// A fresh-tunnel template pre-filled with a real generated private key.
fn new_tunnel_template(private_key: &str) -> String {
    format!(
        "[Interface]\nPrivateKey = {private_key}\nAddress = 10.0.0.2/24\nDNS = 1.1.1.1\n\n\
         [Peer]\nPublicKey = <server public key>\nAllowedIPs = 0.0.0.0/0\n\
         Endpoint = server.example.com:51820\nPersistentKeepalive = 25\n"
    )
}
