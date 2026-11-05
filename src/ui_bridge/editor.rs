//! The tunnel editor window: form/raw editing, key generation, and saving
//! with draft recovery (the draft is kept on every failure).

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use slint::{ComponentHandle, SharedString};

use super::editor_form::{
    Fields, PeerFields, config_to_fields, fields_to_config, form_representable,
};
use super::windows::show_help_window;
use super::{copy_to_clipboard, refresh_list_after, run_ui_task, selected_name, set_status};
use crate::actions::{self, SaveRequest, SyncState};
use crate::draft::{Draft, DraftState, Effect, FailureKind, Recovery};
use crate::state::EDITOR;
use crate::{EditWindow, MainWindow, backend, config, create};

/// Replace only exact keys inside the intended section. Prefix matching can
/// overwrite unknown directives, and raw multi-peer editing must not change a
/// different peer's preshared key.
fn section_header(line: &str) -> Option<&str> {
    let line = line.split_once('#').map_or(line, |(line, _)| line).trim();
    line.strip_prefix('[')?.strip_suffix(']').map(str::trim)
}

fn set_section_key(
    config: &str,
    section: &str,
    index: usize,
    key: &str,
    value: &str,
) -> Option<String> {
    let mut lines: Vec<String> = config.lines().map(str::to_string).collect();
    let start = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            section_header(line).is_some_and(|header| header.eq_ignore_ascii_case(section))
        })
        .nth(index)?
        .0;
    let end = (start + 1..lines.len())
        .find(|&i| section_header(&lines[i]).is_some())
        .unwrap_or(lines.len());
    let mut replaced = false;
    for line in &mut lines[start + 1..end] {
        let code = line.split_once('#').map_or(line.as_str(), |(code, _)| code);
        if code
            .split_once('=')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case(key))
        {
            let comment = line
                .split_once('#')
                .map(|(_, comment)| format!(" #{comment}"))
                .unwrap_or_default();
            *line = format!("{key} = {value}{comment}");
            replaced = true;
        }
    }
    if !replaced {
        lines.insert(start + 1, format!("{key} = {value}"));
    }
    Some(format!("{}\n", lines.join("\n")))
}

pub(crate) fn set_private_key(config: &str, key: &str) -> String {
    let interfaces = config
        .lines()
        .filter(|line| {
            section_header(line).is_some_and(|header| header.eq_ignore_ascii_case("Interface"))
        })
        .count();
    if interfaces == 0 {
        return format!("[Interface]\nPrivateKey = {key}\n{config}");
    }
    // WireGuard accepts repeated Interface sections and the last key wins.
    // Replacing every exact PrivateKey prevents an older effective key from
    // silently surviving generation, including sections after a Peer.
    (0..interfaces).fold(config.to_string(), |config, index| {
        set_section_key(&config, "Interface", index, "PrivateKey", key).unwrap()
    })
}

pub(crate) fn set_psk(config: &str, key: &str, peer_index: usize) -> Option<String> {
    set_section_key(config, "Peer", peer_index, "PresharedKey", key)
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
    ed.set_config_text(cfg.clone().into());
    ed.invoke_config_changed(cfg.clone().into());
    Some(cfg)
}

fn switch_editor_peer(ed: &EditWindow, requested_peer: usize) {
    let Some(cfg) = commit_form_to_config(ed) else {
        return;
    };
    let fields = config_to_fields(&cfg);
    apply_fields(ed, &fields, requested_peer);
}

fn run_editor_task<T: crate::ops::TaskOutcome + Send + 'static>(
    main: &MainWindow,
    editor: &EditWindow,
    message: &str,
    work: impl FnOnce() -> T + Send + 'static,
    done: impl FnOnce(&EditWindow, T) + Send + 'static,
) {
    let before = editor.get_config_text().to_string();
    let peer = editor.get_peer_index();
    let weak = editor.as_weak();
    let message_owned = message.to_string();
    if !run_ui_task(main, message, work, move |main, result| {
        if main.get_status() == message_owned {
            main.set_status("".into());
        }
        let Some(editor) = weak.upgrade() else { return };
        if editor.get_config_text() != before || editor.get_peer_index() != peer {
            editor.set_error("Generation finished; your newer draft was preserved. Try again to apply a new key.".into());
            return;
        }
        done(&editor, result);
    }) {
        editor.set_error("Another operation is still running.".into());
    }
}

/// Show a failure and the recovery choices; the draft text is untouched.
fn fail_draft(ed: &EditWindow, draft: &Rc<RefCell<Draft>>, kind: FailureKind, message: &str) {
    draft.borrow_mut().fail(kind, message);
    let prefix = match kind {
        FailureKind::Validation => "Not saved: the config is invalid",
        FailureKind::Save => "Save failed",
        FailureKind::Unknown => "Save outcome unknown",
    };
    ed.set_error(format!("{prefix}: {message}").into());
    ed.set_recovery("Your draft is kept. Retry, re-edit it, or discard it.".into());
}

/// Cancel / window close: true when the window may close. A failed or
/// in-flight draft is kept and the recovery choices are shown instead.
fn close_or_ask(ed: &EditWindow, draft: &Rc<RefCell<Draft>>) -> bool {
    let saving = *draft.borrow().state() == DraftState::Saving;
    if draft.borrow_mut().close_requested() == Effect::Close {
        return true;
    }
    ed.set_recovery(
        if saving {
            "A save is still in progress; your draft is kept until it finishes."
        } else {
            "This draft has not been saved. Retry, re-edit it, or discard it."
        }
        .into(),
    );
    false
}

/// The draft of the editor currently kept in `EDITOR`, if `ed` is that one.
fn draft_of(ed: &EditWindow) -> Option<Rc<RefCell<Draft>>> {
    EDITOR.with(|slot| {
        slot.borrow()
            .as_ref()
            .filter(|(kept, _)| std::ptr::eq(kept.window(), ed.window()))
            .map(|(_, draft)| draft.clone())
    })
}

fn submit_draft(
    ed: &EditWindow,
    main: &MainWindow,
    draft: &Rc<RefCell<Draft>>,
    activate_after: bool,
) {
    if !draft.borrow_mut().submit() {
        return;
    }
    ed.set_error(SharedString::new());
    ed.set_recovery(SharedString::new());
    let name = ed.get_tunnel_name().trim().to_string();
    let text = ed.get_config_text().to_string();
    if let Err(error) =
        backend::validate_tunnel_name(&name).and_then(|()| backend::validate_config(&text))
    {
        fail_draft(ed, draft, FailureKind::Validation, &error);
        return;
    }
    let prior_selection = selected_name(main);
    let request = SaveRequest {
        original: ed.get_original_name().to_string(),
        name: name.clone(),
        text: text.clone(),
        activate_after,
    };
    ed.set_busy(true);
    let edw = ed.as_weak();
    let started = run_ui_task(
        main,
        "Saving tunnel…",
        move || actions::save_tunnel(&request),
        move |main, result| {
            let ed = edw.upgrade();
            let draft_done = ed.as_ref().and_then(draft_of);
            if let Some(ed) = &ed {
                ed.set_busy(false);
            }
            match result {
                Ok(saved) => {
                    if let Some(draft) = &draft_done {
                        draft.borrow_mut().saved();
                    }
                    if let Some(ed) = ed {
                        ed.set_original_name(saved.name.clone().into());
                        if ed.get_tunnel_name().trim() == name && ed.get_config_text() == text {
                            let _ = ed.hide();
                        } else {
                            ed.set_is_new(false);
                            ed.set_error(
                                "Earlier draft saved; your newer edits have not been saved.".into(),
                            );
                        }
                    }
                    set_status(main, saved.message.clone());
                    if let SyncState::Failed {
                        error,
                        needs_reconnect,
                    } = &saved.sync
                    {
                        crate::events::show_sync_problem(
                            main,
                            &saved.name,
                            error,
                            *needs_reconnect,
                        );
                    } else {
                        crate::events::clear_sync_problem(main, &saved.name);
                    }
                    refresh_list_after(main, Some((saved.name, prior_selection)));
                }
                Err(error) => {
                    if let (Some(ed), Some(draft)) = (&ed, &draft_done) {
                        fail_draft(ed, draft, error.kind, &error.message);
                    }
                    set_status(main, "Save failed; your draft is still open in the editor.");
                }
            }
        },
    );
    if !started {
        ed.set_busy(false);
        fail_draft(
            ed,
            draft,
            FailureKind::Save,
            "another operation is still running; nothing was saved yet",
        );
    }
}

/// Open the tunnel editor in its own top-level window. A fresh window paints
/// cleanly on this Wayland setup, so its text fields show up immediately.
pub fn open_editor(
    main: &MainWindow,
    is_new: bool,
    orig_name: String,
    prefill_name: String,
    text: String,
) {
    // Never replace an editor whose draft failed to save: bring it back.
    let pending = EDITOR.with(|slot| {
        slot.borrow().as_ref().and_then(|(ed, draft)| {
            (draft.borrow().has_failure() || *draft.borrow().state() == DraftState::Saving)
                .then(|| ed.as_weak())
        })
    });
    if let Some(existing) = pending.and_then(|w| w.upgrade()) {
        let _ = existing.show();
        set_status(
            main,
            "An unsaved draft is still open: retry, re-edit or discard it first.",
        );
        return;
    }
    let ed = match EditWindow::new() {
        Ok(e) => e,
        Err(e) => {
            set_status(main, format!("Couldn't open editor: {e}"));
            return;
        }
    };
    crate::apply_theme!(ed);
    ed.set_is_new(is_new);
    let draft = Rc::new(RefCell::new(Draft::new()));
    ed.set_tunnel_name(prefill_name.into());
    if backend::config_runs_scripts(&text) {
        ed.set_warning(
            crate::theme::text(
                "⚠ This config runs commands as root on activation (PostUp/PreUp/…). \
                 Only save it if you trust the source.",
            )
            .into(),
        );
    }
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
        let last_key = Rc::new(RefCell::new(None::<String>));
        ed.on_config_changed(move |cfg| {
            let Some(ed) = edw.upgrade() else { return };
            let private_key = config::parse_config(&cfg).private_key.unwrap_or_default();
            if last_key.borrow().as_ref() == Some(&private_key) {
                return;
            }
            *last_key.borrow_mut() = Some(private_key.clone());
            ed.set_public_key("".into());
            if private_key.is_empty() {
                return;
            }
            // Typing in any form field used to invoke wg synchronously. Only
            // derive when the private key changes, after a short debounce, and
            // reject stale worker results if the user edits it again.
            let weak = ed.as_weak();
            slint::Timer::single_shot(Duration::from_millis(100), move || {
                let Some(ed) = weak.upgrade() else { return };
                let cfg = ed.get_config_text().to_string();
                if config::parse_config(&cfg).private_key.as_deref() != Some(&private_key) {
                    return;
                }
                let weak = ed.as_weak();
                std::thread::spawn(move || {
                    let public_key = backend::public_key_for_config(&cfg);
                    let _ = weak.upgrade_in_event_loop(move |ed| {
                        if config::parse_config(&ed.get_config_text())
                            .private_key
                            .as_deref()
                            == Some(&private_key)
                        {
                            ed.set_public_key(public_key.into());
                        }
                    });
                });
            });
        });
        ed.invoke_config_changed(text.clone().into());
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
            ed.set_config_text(cfg.clone().into());
            ed.invoke_config_changed(cfg.into());
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
            ed.set_config_text(cfg.clone().into());
            ed.invoke_config_changed(cfg.into());
            apply_fields(&ed, &fields, next);
        });
    }

    // Presets generate keys off-thread, then retain explicit review/save.
    {
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        ed.on_create_preset(move |preset| {
            let (Some(ed), Some(main)) = (edw.upgrade(), mainw.upgrade()) else {
                return;
            };
            let kind = match preset.as_str() {
                "interface" => create::TunnelTemplateKind::InterfaceOnly,
                "split" => create::TunnelTemplateKind::ClientSplitTunnel,
                _ => create::TunnelTemplateKind::ClientFullTunnel,
            };
            run_editor_task(
                &main,
                &ed,
                "Generating preset keys…",
                backend::generate_keypair,
                move |ed, result| match result {
                    Ok((private_key, public_key)) => {
                        let cfg = create::generate_template(kind, &private_key);
                        ed.set_config_text(cfg.clone().into());
                        ed.set_public_key(public_key.into());
                        apply_fields(ed, &config_to_fields(&cfg), 0);
                        ed.set_form_mode(form_representable(&cfg));
                        ed.set_error("".into());
                        ed.set_warning(
                            format!("{} preset loaded. Review before creating.", kind.label())
                                .into(),
                        );
                    }
                    Err(error) => ed.set_error(format!("Key generation failed: {error}").into()),
                },
            );
        });
    }

    // Copy (public key / config) to the clipboard.
    ed.on_copy(move |t| {
        copy_to_clipboard(&t);
    });

    // Header "? Help" link: open the in-app guide.
    ed.on_show_help(show_help_window);

    // Cancel / window close: a failed or in-flight draft is never dropped
    // silently; the recovery bar asks instead.
    {
        let edw = ed.as_weak();
        let draft = draft.clone();
        ed.on_cancel(move || {
            if let Some(ed) = edw.upgrade()
                && close_or_ask(&ed, &draft)
            {
                let _ = ed.hide();
            }
        });
    }
    {
        let edw = ed.as_weak();
        let draft = draft.clone();
        ed.window().on_close_requested(move || match edw.upgrade() {
            Some(ed) if !close_or_ask(&ed, &draft) => slint::CloseRequestResponse::KeepWindowShown,
            _ => slint::CloseRequestResponse::HideWindow,
        });
    }
    // Typing after an earlier save returns the draft to plain editing.
    {
        let draft = draft.clone();
        ed.on_draft_edited(move || draft.borrow_mut().edited());
    }

    // Save: validate locally, then write through the helper off-thread. The
    // draft stays in the editor on every failure (validation, permission,
    // timeout, busy lock, I/O) until the user retries, re-edits or discards.
    ed.set_original_name(orig_name.into());
    let last_activate = Rc::new(std::cell::Cell::new(false));
    let submit: Rc<dyn Fn(bool)> = Rc::new({
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        let draft = draft.clone();
        let last_activate = last_activate.clone();
        move |activate_after: bool| {
            let (Some(ed), Some(main)) = (edw.upgrade(), mainw.upgrade()) else {
                return;
            };
            last_activate.set(activate_after);
            submit_draft(&ed, &main, &draft, activate_after);
        }
    });
    {
        let submit = submit.clone();
        ed.on_save(move |_, _| submit(false));
    }
    {
        let submit = submit.clone();
        ed.on_save_and_activate(move |_, _| submit(true));
    }
    {
        let edw = ed.as_weak();
        let draft = draft.clone();
        let submit = submit.clone();
        ed.on_recover_retry(move || {
            let Some(ed) = edw.upgrade() else { return };
            let effect = draft.borrow_mut().choose(Recovery::Retry);
            match effect {
                Effect::Resubmit => submit(last_activate.get()),
                _ => ed.set_recovery("A save is still in progress; your draft is kept.".into()),
            }
        });
    }
    {
        let edw = ed.as_weak();
        let draft = draft.clone();
        ed.on_recover_reedit(move || {
            let Some(ed) = edw.upgrade() else { return };
            draft.borrow_mut().choose(Recovery::ReEdit);
            ed.set_recovery(SharedString::new());
            ed.invoke_focus_draft();
        });
    }
    {
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        let draft = draft.clone();
        ed.on_recover_discard(move || {
            let Some(ed) = edw.upgrade() else { return };
            if draft.borrow_mut().choose(Recovery::Discard) == Effect::Close {
                let _ = ed.hide();
                if let Some(main) = mainw.upgrade() {
                    set_status(&main, "Draft discarded; nothing was saved.");
                }
            }
        });
    }
    // Key generation may invoke wg; never wait for it on the event loop.
    {
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        ed.on_generate_key(move || {
            let (Some(ed), Some(main)) = (edw.upgrade(), mainw.upgrade()) else {
                return;
            };
            run_editor_task(
                &main,
                &ed,
                "Generating keys…",
                backend::generate_keypair,
                |ed, result| match result {
                    Ok((private_key, public_key)) => {
                        let updated = set_private_key(&ed.get_config_text(), &private_key);
                        let peer = ed.get_peer_index().max(0) as usize;
                        apply_fields(ed, &config_to_fields(&updated), peer);
                        ed.set_config_text(updated.into());
                        ed.set_public_key(public_key.into());
                        ed.set_error("".into());
                        ed.set_warning("New keypair generated.".into());
                    }
                    Err(error) => ed.set_error(format!("Key generation failed: {error}").into()),
                },
            );
        });
    }
    {
        let edw = ed.as_weak();
        let mainw = main.as_weak();
        ed.on_generate_psk(move || {
            let (Some(ed), Some(main)) = (edw.upgrade(), mainw.upgrade()) else {
                return;
            };
            if !ed.get_config_text().lines().any(|line| {
                section_header(line).is_some_and(|header| header.eq_ignore_ascii_case("Peer"))
            }) {
                ed.set_error("Add a [Peer] before generating a preshared key.".into());
                return;
            }
            run_editor_task(
                &main,
                &ed,
                "Generating preshared key…",
                backend::generate_psk,
                |ed, result| match result {
                    Ok(psk) => {
                        let index = ed.get_peer_index().max(0) as usize;
                        let updated = if ed.get_form_mode()
                            && form_representable(&ed.get_config_text())
                        {
                            let mut fields = read_fields(ed);
                            fields.ensure_peer();
                            let index = index.min(fields.peers.len() - 1);
                            fields.peers[index].preshared_key = psk;
                            let cfg = fields_to_config(&fields);
                            apply_fields(ed, &fields, index);
                            cfg
                        } else {
                            let Some(updated) = set_psk(&ed.get_config_text(), &psk, index) else {
                                ed.set_error(
                                    "Select an existing [Peer] before generating a preshared key."
                                        .into(),
                                );
                                return;
                            };
                            updated
                        };
                        ed.set_config_text(updated.into());
                        ed.set_error("".into());
                        ed.set_warning("New preshared key generated.".into());
                    }
                    Err(error) => ed.set_error(format!("PSK generation failed: {error}").into()),
                },
            );
        });
    }

    let _ = ed.show();
    EDITOR.with(|slot| *slot.borrow_mut() = Some((ed, draft)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_private_key_preserves_prefix_directives_and_peer_fields() {
        let config = "[Interface] # client\nPrivateKeyFile = keep\nPrivateKey = old\n[Peer]\nPrivateKey = peer-value\n";
        let updated = set_private_key(config, "new");
        assert!(updated.contains("PrivateKeyFile = keep\nPrivateKey = new"));
        assert!(updated.contains("[Peer]\nPrivateKey = peer-value"));
    }
    #[test]
    fn generated_private_key_replaces_all_interface_sections_including_after_peers() {
        let config = "[Interface]\nPrivateKey = first # keep this comment\nPrivateKeyFile = keep\n[Peer]\nPrivateKey = peer-field\n[Interface] # repeated\nPrivateKey = last\nAddress = 10.0.0.1/32\n";
        let updated = set_private_key(config, "new");
        assert_eq!(updated.matches("PrivateKey = new").count(), 2);
        assert!(!updated.contains("PrivateKey = first"));
        assert!(!updated.contains("PrivateKey = last"));
        assert!(updated.contains("PrivateKey = new # keep this comment"));
        assert!(updated.contains("PrivateKeyFile = keep"));
        assert!(updated.contains("[Peer]\nPrivateKey = peer-field"));
        assert!(updated.contains("Address = 10.0.0.1/32"));
    }

    #[test]
    fn raw_psk_generation_targets_selected_peer() {
        let config = "[Interface]\nPrivateKey = keep\n[Peer]\nPresharedKey = first\n[Peer] # second\nPresharedKeyFile = keep-too\nPresharedKey = second\n";
        let updated = set_psk(config, "new", 1).unwrap();
        assert!(updated.contains("PresharedKey = first"));
        assert!(updated.contains("PresharedKeyFile = keep-too\nPresharedKey = new"));
        assert!(set_psk("# [Peer]\n[Interface]\n", "new", 0).is_none());
    }
}
