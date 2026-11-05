//! Secondary windows: Help, Updates, About, diagnostics, QR code and the
//! first-run Setup wizard.

use slint::{ComponentHandle, ModelRc, VecModel};

use super::markdown::{CHANGELOG_TEXT, HELP_TEXT, changelog_for_display, md_items_model};
use super::{copy_to_clipboard, flash_copied_hint, lines_model, open_url};
use crate::ops::OpGuard;
use crate::{
    AboutWindow, CheckItem, DiagWindow, HelpWindow, MainWindow, QrWindow, SetupWindow,
    UpdateWindow, doctor, state, update,
};

/// Open (or re-open) the in-app Help window with the embedded guide.
pub fn show_help_window() {
    let Ok(win) = HelpWindow::new() else { return };
    crate::apply_theme!(win);
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
    state::HELPWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

/// Kick off a background `update::check()` for an open Updates window, posting
/// the status + update-available flag back on the UI thread.
pub fn run_update_check(win: &UpdateWindow) {
    if win.get_busy() {
        return;
    }
    win.set_busy(true);
    win.set_status("Checking…".into());
    win.set_update_available(false);
    let ww = win.as_weak();
    std::thread::spawn(move || {
        let found = update::check();
        let _ = slint::invoke_from_event_loop(move || {
            let Some(w) = ww.upgrade() else { return };
            w.set_busy(false);
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

/// Open (or re-open) the Updates window: shows the installed version + embedded
/// changelog, then kicks off a background check.
pub fn show_update_window() {
    let Ok(win) = UpdateWindow::new() else { return };
    crate::apply_theme!(win);
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
            if u.get_busy() {
                return;
            }
            let Some(guard) = OpGuard::acquire("Updating wireguard-gui") else {
                u.set_status("Another operation is still running.".into());
                return;
            };
            u.set_busy(true);
            u.set_status("Downloading and verifying the update…".into());
            u.set_update_available(false);
            let uw = u.as_weak();
            std::thread::spawn(move || {
                let result = update::download_and_verify().and_then(|dir| {
                    let version = dir.version().to_string();
                    update::apply(&dir).map(|()| version)
                });
                let msg = match result {
                    Ok(version) => {
                        format!("Updated to v{version} — restart wireguard-gui to apply.")
                    }
                    Err(e) => format!("Update failed: {e}"),
                };
                drop(guard);
                let uw = uw.clone();
                let _ = slint::invoke_from_event_loop(move || {
                    if let Some(u) = uw.upgrade() {
                        u.set_busy(false);
                        u.set_status(msg.into());
                    }
                });
            });
        });
    }
    run_update_check(&win);
    let _ = win.show();
    state::UPDATEWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

/// Open the About window.
pub fn show_about_window() {
    let Ok(about) = AboutWindow::new() else {
        return;
    };
    crate::apply_theme!(about);
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
    state::ABOUTWIN.with(|slot| *slot.borrow_mut() = Some(about));
}

/// Show a finished connection-diagnostics report.
pub fn show_diag_window(report: &str) {
    let Ok(win) = DiagWindow::new() else { return };
    crate::apply_theme!(win);
    win.set_report_lines(lines_model(report));
    win.set_full_text(report.into());
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
    state::DIAGWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

/// Show a tunnel's config as a QR code.
pub fn show_qr_window(name: String, image: slint::Image) {
    let Ok(win) = QrWindow::new() else {
        return;
    };
    crate::apply_theme!(win);
    win.set_name(name.into());
    win.set_qr(image);
    {
        let qw = win.as_weak();
        win.on_close_me(move || {
            if let Some(q) = qw.upgrade() {
                let _ = q.hide();
            }
        });
    }
    let _ = win.show();
    state::QRWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

/// Render a string to a black-on-white QR-code Slint image.
pub fn qr_image(text: &str) -> Result<slint::Image, String> {
    use slint::{Rgba8Pixel, SharedPixelBuffer};
    let (scale, quiet) = (8usize, 4usize);
    let code = qrcode::QrCode::new(text.as_bytes())
        .map_err(|error| format!("Config cannot fit in one QR code: {error}"))?;
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
    Ok(slint::Image::from_rgba8(buf))
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
pub fn show_setup_wizard(main: &MainWindow, report: doctor::Report) {
    let Ok(win) = SetupWindow::new() else {
        let _ = main.show();
        return;
    };
    crate::apply_theme!(win);
    let _ = main.hide();
    win.set_checks(build_checks(&report));
    win.set_commands(commands_text(&report).into());

    {
        let weak = main.as_weak();
        win.window().on_close_requested(move || {
            if let Some(main) = weak.upgrade() {
                let _ = main.show();
            }
            slint::CloseRequestResponse::HideWindow
        });
    }
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
            let Some(window) = sw.upgrade() else {
                return;
            };
            window.set_busy("Checking prerequisites…".into());
            let sw = window.as_weak();
            let mw = mw.clone();
            std::thread::spawn(move || {
                let report = doctor::system_check();
                let _ = sw.upgrade_in_event_loop(move |window| {
                    window.set_checks(build_checks(&report));
                    window.set_commands(commands_text(&report).into());
                    if report.critical_ok() {
                        let _ = window.hide();
                        if let Some(main) = mw.upgrade() {
                            let _ = main.show();
                        }
                    } else {
                        window.set_busy("Still missing something — see the list above.".into());
                    }
                });
            });
        });
    }
    {
        let sw = win.as_weak();
        win.on_fix_automatically(move || {
            let Some(s) = sw.upgrade() else { return };
            let Some(guard) = OpGuard::acquire("Setting up prerequisites") else {
                s.set_busy("Another operation is still running.".into());
                return;
            };
            s.set_busy("Checking setup actions…".into());
            let sw2 = s.as_weak();
            std::thread::spawn(move || {
            // Only the safely-automatable steps: install wireguard-tools (+ a
            // resolvconf provider for DNS) and create /etc/wireguard. Never
            // installs the helper or touches configs.
            let mut steps: Vec<String> = Vec::new();
            if !(doctor::which("wg") && doctor::which("wg-quick"))
                && let Some(c) = doctor::install_tools_command()
            {
                steps.push(c);
            }
            // A `DNS =` line needs both a `resolvconf` command and, when that
            // command is the resolvectl shim, a running systemd-resolved. An
            // install can only be judged once it has happened, so a freshly
            // installed-but-inactive shim is picked up by the next Re-check.
            match doctor::dns_setup_action(doctor::dns_resolver_state()) {
                Some(doctor::DnsSetupAction::EnableResolved) => {
                    steps.push(doctor::enable_resolved_command().to_string());
                }
                Some(doctor::DnsSetupAction::InstallProvider) => {
                    if let Some(c) = doctor::install_resolvconf_command() {
                        steps.push(c);
                    }
                }
                None => {}
            }
            if !std::path::Path::new("/etc/wireguard").is_dir() {
                steps.push("install -d -m 700 /etc/wireguard".to_string());
            }
                if steps.is_empty() {
                    let _ = sw2.upgrade_in_event_loop(move |s| {
                        drop(guard);
                        s.set_busy("Nothing to auto-install here. Install the helper via the .deb / AUR / ./install.sh, then Re-check.".into());
                    });
                    return;
                }
                let script = steps.join(" && ");
                let _ = sw2.upgrade_in_event_loop(|s| {
                    s.set_busy("Working - enter your password in the dialog that appears...".into());
                });
                let ok = std::process::Command::new("pkexec")
                    .arg("sh")
                    .arg("-c")
                    .arg(&script)
                    .status()
                    .map(|st| st.success())
                    .unwrap_or(false);
                let report = doctor::system_check();
                let _ = sw2.upgrade_in_event_loop(move |s| {
                    drop(guard);
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
    state::SETUPWIN.with(|slot| *slot.borrow_mut() = Some(win));
}

#[cfg(test)]
mod tests {
    #[test]
    fn oversized_config_qr_is_an_error_instead_of_blank_image() {
        assert!(super::qr_image(&"x".repeat(5000)).is_err());
    }
}
