// Hide the console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod backend;

use std::cell::RefCell;
use std::time::Duration;

use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

slint::include_modules!();

thread_local! {
    // Keeps the currently-open editor window alive. Replaced (and the old one
    // dropped) on the next open; only ever touched on the UI thread.
    static EDITOR: RefCell<Option<EditWindow>> = const { RefCell::new(None) };
    // True while the editor window is open. The main-window poll pauses then,
    // so its periodic repaints don't blank the editor's text fields.
    static EDITOR_OPEN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
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
    ed.set_config_text(text.into());

    // Cancel: just hide (the strong handle lives in EDITOR until next open).
    {
        let edw = ed.as_weak();
        ed.on_cancel(move || {
            EDITOR_OPEN.with(|f| f.set(false));
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
                    set_status(&main, format!("Saved {name}"));
                }
                refresh_list(&main);
                select_by_name(&main, &name);
            }
            EDITOR_OPEN.with(|f| f.set(false));
            let _ = ed.hide();
        });
    }

    EDITOR_OPEN.with(|f| f.set(true));
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
    }
}

fn load_detail(ui: &MainWindow, name: &str) {
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
            open_editor(
                &ui,
                true,
                String::new(),
                String::new(),
                TEMPLATE.to_string(),
            );
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

    // ---- live polling: refresh selected detail + list every 2s ----
    let timer = slint::Timer::default();
    {
        let w = ui.as_weak();
        timer.start(
            slint::TimerMode::Repeated,
            Duration::from_secs(1),
            move || {
                let Some(ui) = w.upgrade() else { return };

                // Pause while the editor window is open so our repaints don't
                // blank its text fields.
                if EDITOR_OPEN.with(|f| f.get()) {
                    return;
                }

                if let Some(name) = selected_name(&ui) {
                    load_detail(&ui, &name);
                }
                // refresh active dots in the list
                let actives: std::collections::HashSet<String> = backend::list_tunnels()
                    .into_iter()
                    .filter(|t| t.active)
                    .map(|t| t.name)
                    .collect();
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

                // Nudge a repaint for the live status fields.
                ui.window().request_redraw();
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
