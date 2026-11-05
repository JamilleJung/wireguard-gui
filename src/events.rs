//! Main-window event wiring: every callback the UI raises is connected here
//! to a background action (`actions`) and a rendering step (`ui_bridge`).

use slint::{ComponentHandle, SharedString};

use crate::actions::{self, RestoreOutcome};
use crate::ui_bridge::editor::open_editor;
use crate::ui_bridge::windows::{
    qr_image, show_about_window, show_diag_window, show_help_window, show_qr_window,
    show_update_window,
};
use crate::ui_bridge::{
    apply_log_filter, copy_to_clipboard, one_line_model, populate_backups, refresh_list,
    refresh_list_after, refresh_log_async, rerender_detail, run_ui_task, selected_name, set_status,
    ui_idle,
};
use crate::{MainWindow, backend, clipboard, create, state};

/// Show the "saved file and running interface differ" banner.
pub fn show_sync_problem(ui: &MainWindow, name: &str, error: &str, needs_reconnect: bool) {
    ui.set_sync_tunnel(name.into());
    ui.set_sync_needs_reconnect(needs_reconnect);
    ui.set_sync_problem(
        crate::theme::text(&actions::sync_failure_message(name, error, needs_reconnect)).into(),
    );
}

/// Clear the banner once the running interface matches again.
pub fn clear_sync_problem(ui: &MainWindow, name: &str) {
    if ui.get_sync_tunnel() == name {
        ui.set_sync_problem(SharedString::new());
        ui.set_sync_tunnel(SharedString::new());
    }
}

fn pick_conf_files() -> Option<Vec<std::path::PathBuf>> {
    rfd::FileDialog::new()
        .add_filter("WireGuard config", &["conf"])
        .set_title("Import tunnel(s) from file")
        .pick_files()
}

pub fn wire(ui: &MainWindow) {
    wire_tunnels(ui);
    wire_import_export(ui);
    wire_log(ui);
    wire_backups(ui);
    wire_misc(ui);
}

fn wire_tunnels(ui: &MainWindow) {
    {
        let w = ui.as_weak();
        ui.on_select(move |idx| {
            let Some(ui) = w.upgrade() else { return };
            ui.set_selected_index(idx);
            if let Some(name) = selected_name(&ui) {
                crate::ui_bridge::load_detail(&ui, &name);
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_refresh_tunnels(move || {
            if let Some(ui) = w.upgrade() {
                refresh_list(&ui);
            }
        });
    }

    // ---- activate / deactivate: verified against the real interface state ----
    {
        let weak = ui.as_weak();
        ui.on_activate(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Activating tunnel…",
                move || backend::activate_verified(&name),
                |ui, verified| {
                    set_status(ui, verified.message);
                    refresh_list(ui);
                },
            );
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_deactivate(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Deactivating tunnel…",
                move || backend::deactivate_verified(&name),
                |ui, verified| {
                    set_status(ui, verified.message);
                    refresh_list(ui);
                },
            );
        });
    }

    // ---- diagnose a stuck connection (pings / resolves: off the UI thread) ----
    {
        let w = ui.as_weak();
        ui.on_diagnose_conn(move |name| {
            let Some(ui) = w.upgrade() else { return };
            set_status(&ui, "Diagnosing…");
            let name = name.to_string();
            std::thread::spawn(move || {
                let report = backend::diagnose_report(&name, state::known_health(&name));
                let _ = slint::invoke_from_event_loop(move || show_diag_window(&report));
            });
        });
    }

    // ---- add empty tunnel ----
    {
        let w = ui.as_weak();
        ui.on_add_empty(move || {
            let Some(ui) = w.upgrade() else { return };
            run_ui_task(
                &ui,
                "Generating tunnel keys…",
                || {
                    backend::generate_keypair().map(|(private, _)| {
                        create::generate_template(
                            create::TunnelTemplateKind::ClientFullTunnel,
                            &private,
                        )
                    })
                },
                |ui, result| match result {
                    Ok(text) => open_editor(ui, true, String::new(), String::new(), text),
                    Err(error) => set_status(ui, format!("Key generation failed: {error}")),
                },
            );
        });
    }

    // ---- delete: verified (the file must actually be gone) ----
    {
        let weak = ui.as_weak();
        ui.on_delete_tunnel(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.to_string();
            if name.is_empty() {
                return;
            }
            run_ui_task(
                &ui,
                "Removing tunnel…",
                move || backend::delete_verified(&name),
                |ui, verified| {
                    set_status(ui, verified.message);
                    refresh_list(ui);
                },
            );
        });
    }

    // ---- begin edit ----
    {
        let weak = ui.as_weak();
        ui.on_begin_edit(move |name| {
            let Some(ui) = weak.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Loading tunnel for editing…",
                move || {
                    backend::read_config(&name)
                        .map(|text| (name, text))
                        .map_err(String::from)
                },
                |ui, result| match result {
                    Ok((name, text)) => open_editor(ui, false, name.clone(), name, text),
                    Err(error) => set_status(ui, format!("Open failed: {error}")),
                },
            );
        });
    }

    // ---- peer paging / filtering in the detail pane ----
    {
        let w = ui.as_weak();
        ui.on_peer_page_changed(move |page| {
            let Some(ui) = w.upgrade() else { return };
            ui.set_peer_page(page.max(0));
            rerender_detail(&ui);
        });
    }
    {
        let w = ui.as_weak();
        ui.on_peer_filter_changed(move |filter| {
            let Some(ui) = w.upgrade() else { return };
            ui.set_peer_filter(filter);
            ui.set_peer_page(0);
            rerender_detail(&ui);
        });
    }

    // ---- live sync recovery: saved file and running interface differ ----
    {
        let w = ui.as_weak();
        ui.on_sync_retry(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Applying the saved config to the running tunnel…",
                {
                    let name = name.clone();
                    move || backend::sync_running(&name)
                },
                move |ui, result| match result {
                    Ok(()) => {
                        clear_sync_problem(ui, &name);
                        set_status(
                            ui,
                            format!("Applied the saved {name} to the running interface"),
                        );
                        refresh_list(ui);
                    }
                    Err(error) => {
                        let needs = backend::sync_needs_reconnect(&error);
                        show_sync_problem(ui, &name, &error.to_string(), needs);
                        set_status(ui, format!("Live sync failed again: {error}"));
                    }
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_sync_reconnect(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Reconnecting tunnel…",
                {
                    let name = name.clone();
                    move || backend::reconnect_verified(&name)
                },
                move |ui, verified| {
                    if verified.applied() {
                        clear_sync_problem(ui, &name);
                    }
                    set_status(ui, verified.message);
                    refresh_list(ui);
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_sync_dismiss(move || {
            let Some(ui) = w.upgrade() else { return };
            let name = ui.get_sync_tunnel().to_string();
            ui.set_sync_problem(SharedString::new());
            ui.set_sync_tunnel(SharedString::new());
            set_status(
                &ui,
                format!("Kept as is: the saved {name} and the running interface differ until you reconnect."),
            );
        });
    }

    // ---- start-on-boot / kill switch ----
    {
        let w = ui.as_weak();
        ui.on_set_autostart(move |name, on| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Updating start-on-boot…",
                move || {
                    let result = backend::set_autostart(&name, on);
                    // Report the real unit state, not just the exit status.
                    let now_on = backend::is_autostart(&name);
                    let state = if now_on { "enabled" } else { "disabled" };
                    match result {
                        Ok(()) if now_on == on => Ok(format!(
                            "{} autostart for {name}",
                            if on { "Enabled" } else { "Disabled" }
                        )),
                        Ok(()) => Err(format!(
                            "Autostart change reported success, but start-on-boot is {state}"
                        )),
                        Err(e) if now_on == on && e.outcome_unknown() => Ok(format!(
                            "Autostart change timed out, but start-on-boot is now {state}"
                        )),
                        Err(e) => Err(format!(
                            "Autostart change failed: {e} (start-on-boot is {state})"
                        )),
                    }
                },
                |ui, result| {
                    set_status(ui, result.unwrap_or_else(|error| error));
                    refresh_list(ui);
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_set_killswitch(move |name, on| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Updating kill switch…",
                move || {
                    let result = backend::set_killswitch(&name, on);
                    // Report what is actually in force afterwards.
                    let status = backend::killswitch_status(&name);
                    let wanted = if on {
                        backend::KillSwitch::Enabled
                    } else {
                        backend::KillSwitch::Disabled
                    };
                    match result {
                        Ok(()) if status == wanted => Ok(format!(
                            "Kill switch {} for {name}",
                            if on { "enabled" } else { "disabled" }
                        )),
                        Ok(()) => Err(format!(
                            "Kill switch change reported success, but its status is {}",
                            status.as_str()
                        )),
                        Err(e) => Err(format!(
                            "Kill switch change failed: {e} (status now: {})",
                            status.as_str()
                        )),
                    }
                },
                |ui, result| {
                    set_status(ui, result.unwrap_or_else(|error| error));
                    refresh_list(ui);
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_repair_killswitch(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Repairing kill switch…",
                move || actions::repair_killswitch(&name),
                |ui, result| {
                    set_status(ui, result.unwrap_or_else(|error| error));
                    refresh_list(ui);
                },
            );
        });
    }

    // ---- show a tunnel's config as a QR code ----
    {
        let w = ui.as_weak();
        ui.on_show_qr(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Reading tunnel QR config…",
                move || {
                    backend::read_config(&name)
                        .map(|text| (name, text))
                        .map_err(String::from)
                },
                |ui, result| match result
                    .and_then(|(name, text)| qr_image(&text).map(|image| (name, image)))
                {
                    Ok((name, image)) => show_qr_window(name, image),
                    Err(error) => set_status(ui, format!("QR failed: {error}")),
                },
            );
        });
    }

    // ---- live running config / save live ----
    {
        let w = ui.as_weak();
        ui.on_show_running(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Reading running config…",
                move || backend::running_config(&name).map_err(String::from),
                |ui, result| match result {
                    Ok(text) => set_status(
                        ui,
                        if copy_to_clipboard(&text) {
                            "Copied running config"
                        } else {
                            "Couldn't access the clipboard"
                        },
                    ),
                    Err(error) => set_status(ui, format!("Couldn't read running config: {error}")),
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_persist_live(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Saving live state…",
                move || {
                    backend::persist_live(&name)
                        .map(|()| format!("Saved live state of {name}"))
                        .map_err(|e| format!("Save live failed: {e}"))
                },
                |ui, result| {
                    set_status(ui, result.unwrap_or_else(|error| error));
                    refresh_list(ui);
                },
            );
        });
    }
}

fn wire_import_export(ui: &MainWindow) {
    // ---- import from file ----
    {
        let weak = ui.as_weak();
        ui.on_import_file(move || {
            let Some(ui) = weak.upgrade() else { return };
            if !ui_idle(&ui) {
                return;
            }
            let Some(files) = pick_conf_files() else {
                return;
            };
            if files.len() == 1 {
                let path = files[0].clone();
                run_ui_task(
                    &ui,
                    "Reading tunnel config…",
                    move || actions::read_import(&path),
                    |ui, result| match result {
                        Ok((name, content)) => open_editor(ui, true, String::new(), name, content),
                        Err(error) => set_status(ui, format!("Import failed: {error}")),
                    },
                );
            } else {
                let expected = selected_name(&ui);
                run_ui_task(
                    &ui,
                    "Importing tunnel configs…",
                    move || actions::import_files(files),
                    move |ui, summary| {
                        set_status(ui, summary.message());
                        refresh_list_after(ui, summary.last.map(|name| (name, expected)));
                    },
                );
            }
        });
    }

    // ---- export all tunnels to a .zip ----
    {
        let w = ui.as_weak();
        ui.on_export_zip(move || {
            let Some(ui) = w.upgrade() else { return };
            if !ui_idle(&ui) {
                return;
            }
            let Some(path) = rfd::FileDialog::new()
                .set_title("Export tunnels to a zip archive")
                .set_file_name("wireguard-tunnels.zip")
                .add_filter("Zip archive", &["zip"])
                .save_file()
            else {
                return;
            };
            run_ui_task(
                &ui,
                "Exporting tunnel configs…",
                move || {
                    backend::export_zip(&path)
                        .map(|count| format!("Exported {count} tunnel(s) to {}", path.display()))
                },
                |ui, result| {
                    set_status(
                        ui,
                        result.unwrap_or_else(|error| format!("Export failed: {error}")),
                    )
                },
            );
        });
    }

    // ---- import a tunnel from a QR-code image ----
    {
        let w = ui.as_weak();
        ui.on_import_qr(move || {
            let Some(ui) = w.upgrade() else { return };
            if !ui_idle(&ui) {
                return;
            }
            let Some(path) = rfd::FileDialog::new()
                .set_title("Import tunnel from a QR-code image")
                .add_filter("Image", &["png", "jpg", "jpeg"])
                .pick_file()
            else {
                return;
            };
            run_ui_task(
                &ui,
                "Reading QR image…",
                move || actions::read_qr_import(&path),
                |ui, result| match result {
                    Ok((name, text)) => open_editor(ui, true, String::new(), name, text),
                    Err(error) => set_status(ui, format!("QR import failed: {error}")),
                },
            );
        });
    }
}

fn wire_log(ui: &MainWindow) {
    {
        let w = ui.as_weak();
        ui.on_refresh_log(move || refresh_log_async(w.clone()));
    }
    {
        let w = ui.as_weak();
        ui.on_apply_log_filter(move || {
            if let Some(ui) = w.upgrade() {
                apply_log_filter(&ui);
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_save_log(move || {
            let Some(ui) = w.upgrade() else { return };
            let Some(dest) = rfd::FileDialog::new()
                .set_title("Save log to a file")
                .set_file_name("wireguard-gui-log.txt")
                .save_file()
            else {
                return;
            };
            let text = state::RAW_LOG.with(|r| r.borrow().clone());
            match backend::save_text_to(&dest, &text) {
                Ok(()) => set_status(&ui, format!("Log saved to {}", dest.display())),
                Err(e) => set_status(&ui, format!("Save failed: {e}")),
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_copy_log(move || {
            let Some(ui) = w.upgrade() else { return };
            let text = state::RAW_LOG.with(|r| r.borrow().clone());
            if text.is_empty() {
                set_status(&ui, "Nothing to copy yet — press Refresh");
            } else if copy_to_clipboard(&text) {
                set_status(&ui, "Copied the full log to clipboard");
            } else {
                set_status(&ui, "Couldn't access the clipboard");
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_copy_line(move |line| {
            let Some(ui) = w.upgrade() else { return };
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
            let Some(ui) = w.upgrade() else { return };
            state::LOG_REQUEST.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            state::RAW_LOG.with(|r| r.borrow_mut().clear());
            ui.set_log_lines(one_line_model(
                "(cleared — press Refresh to reload from the journal)",
            ));
        });
    }
}

fn wire_backups(ui: &MainWindow) {
    {
        let w = ui.as_weak();
        ui.on_refresh_backups(move || {
            if let Some(ui) = w.upgrade() {
                populate_backups(&ui);
            }
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_create(move || {
            let Some(ui) = w.upgrade() else { return };
            run_ui_task(
                &ui,
                "Creating backup…",
                backend::create_backup,
                |ui, result| {
                    set_status(
                        ui,
                        result
                            .map(|info| format!("Backed up {} tunnel(s)", info.count))
                            .unwrap_or_else(|error| format!("Backup failed: {error}")),
                    );
                    populate_backups(ui);
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_restore(move |name| {
            let Some(ui) = w.upgrade() else { return };
            let name = name.to_string();
            run_ui_task(
                &ui,
                "Restoring backup…",
                move || {
                    RestoreOutcome(
                        backend::backup_dir()
                            .and_then(|dir| backend::restore_backup(&dir.join(name))),
                    )
                },
                |ui, outcome| {
                    set_status(ui, outcome.message());
                    refresh_list(ui);
                },
            );
        });
    }
    {
        let w = ui.as_weak();
        ui.on_backup_delete(move |name| {
            let Some(ui) = w.upgrade() else { return };
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
            let Some(ui) = w.upgrade() else { return };
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
}

fn wire_misc(ui: &MainWindow) {
    ui.set_show_advanced(state::load_show_advanced());
    ui.on_persist_advanced(state::save_show_advanced);

    // ---- copy single-field values to the clipboard ----
    {
        let w = ui.as_weak();
        ui.on_copy_value(move |text| {
            let Some(ui) = w.upgrade() else { return };
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
            let Some(ui) = w.upgrade() else { return };
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
    ui.on_show_about(show_about_window);
    ui.on_show_help(show_help_window);
    ui.on_show_update(show_update_window);
}
