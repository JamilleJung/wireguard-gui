//! Window tests on Slint's headless testing backend: layout at small and
//! narrow sizes, accessibility semantics, keyboard navigation and shortcuts,
//! peer paging, detail scrolling and themes. Each test runs on its own
//! thread with its own backend instance.

use std::cell::RefCell;
use std::rc::Rc;

use i_slint_backend_testing::{AccessibleRole, ElementHandle, ElementQuery};
use slint::platform::{Key, WindowEvent};
use slint::{ComponentHandle, LogicalSize, ModelRc, SharedString, VecModel};

use crate::backend::{Detail, KillSwitch, Peer};
use crate::{EditWindow, HelpWindow, MainWindow, Theme, TunnelItem, ui_bridge};

type Log = Rc<RefCell<Vec<String>>>;

fn init() {
    i_slint_backend_testing::init_no_event_loop();
}

fn key(n: usize) -> String {
    // 43 base64 chars + '=' — unique per peer.
    format!("{:A<41}{:02}=", "PEER", n % 100)
}

fn sample_detail(peers: usize) -> Detail {
    Detail {
        name: "wg0".into(),
        active: true,
        killswitch: KillSwitch::Disabled,
        public_key: key(99),
        listen_port: "51820".into(),
        addresses: "10.0.0.2/32".into(),
        dns: "1.1.1.1".into(),
        peers: (1..=peers)
            .map(|n| Peer {
                public_key: key(n),
                allowed_ips: format!("10.0.0.{n}/32"),
                endpoint: format!("vpn{n}.example.com:51820"),
                keepalive: "25".into(),
                keepalive_secs: 25,
                latest_handshake: "5 seconds ago".into(),
                handshake_age: Some(5),
                rx: 100,
                tx: 100,
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    }
}

fn main_window(width: f32, height: f32, peers: usize) -> MainWindow {
    let ui = MainWindow::new().unwrap();
    ui.window().set_size(LogicalSize::new(width, height));
    ui.show().unwrap();
    ui.set_tunnels(ModelRc::new(VecModel::from(vec![
        TunnelItem {
            name: "wg0".into(),
            active: true,
        },
        TunnelItem {
            name: "office".into(),
            active: false,
        },
    ])));
    ui.set_selected_index(0);
    ui.set_show_advanced(true);
    ui_bridge::show_detail(&ui, &sample_detail(peers), &[], "");
    ui
}

fn record(ui: &MainWindow) -> Log {
    let log: Log = Rc::default();
    macro_rules! on {
        ($($cb:ident($($arg:ident),*)),* $(,)?) => {$(
            let l = log.clone();
            ui.$cb(move |$($arg),*| {
                $(let _ = $arg;)*
                l.borrow_mut().push(stringify!($cb)[3..].to_string());
            });
        )*};
    }
    on!(
        on_activate(a),
        on_deactivate(a),
        on_diagnose_conn(a),
        on_show_qr(a),
        on_begin_edit(a),
        on_delete_tunnel(a),
        on_export_zip(),
        on_show_help(),
        on_select(a),
        on_persist_advanced(a),
        on_set_autostart(a, b),
        on_copy_value(a),
        on_peer_page_changed(a),
        on_refresh_log(),
        on_refresh_backups(),
        on_import_file(),
        on_import_qr(),
        on_add_empty(),
        on_show_update(),
        on_refresh_tunnels(),
        on_set_killswitch(a, b),
        on_show_running(a),
        on_persist_live(a),
        on_show_about(),
        on_save_log(),
        on_peer_filter_changed(a),
    );
    log
}

fn press(win: &slint::Window, text: impl Into<SharedString>) {
    let text = text.into();
    win.dispatch_event(WindowEvent::KeyPressed { text: text.clone() });
    win.dispatch_event(WindowEvent::KeyReleased { text });
}

fn ctrl(win: &slint::Window, text: &str) {
    win.dispatch_event(WindowEvent::KeyPressed {
        text: Key::Control.into(),
    });
    press(win, text);
    win.dispatch_event(WindowEvent::KeyReleased {
        text: Key::Control.into(),
    });
}

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

fn rect(e: &ElementHandle) -> Rect {
    let p = e.absolute_position();
    let s = e.size();
    Rect {
        x: p.x,
        y: p.y,
        w: s.width,
        h: s.height,
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    const EPS: f32 = 0.5;
    a.x + a.w > b.x + EPS && b.x + b.w > a.x + EPS && a.y + a.h > b.y + EPS && b.y + b.h > a.y + EPS
}

const INTERACTIVE: [AccessibleRole; 5] = [
    AccessibleRole::Button,
    AccessibleRole::Tab,
    AccessibleRole::ListItem,
    AccessibleRole::Checkbox,
    AccessibleRole::TextInput,
];

fn interactive(root: &impl i_slint_backend_testing::ElementRoot) -> Vec<ElementHandle> {
    ElementQuery::from_root(root)
        .match_predicate(|e| {
            e.accessible_role()
                .is_some_and(|r| INTERACTIVE.contains(&r))
        })
        .find_all()
}

fn labelled(root: &impl i_slint_backend_testing::ElementRoot, label: &str) -> ElementHandle {
    ElementHandle::find_by_accessible_label(root, label)
        .next()
        .unwrap_or_else(|| panic!("no element labelled {label:?}"))
}

/// Interactive controls must sit inside the window horizontally and must
/// not overlap each other where both are fully on screen.
fn assert_controls_fit(root: &impl i_slint_backend_testing::ElementRoot, width: f32, height: f32) {
    let controls: Vec<(String, Rect)> = interactive(root)
        .iter()
        .map(|e| {
            (
                e.accessible_label().unwrap_or_default().to_string(),
                rect(e),
            )
        })
        .filter(|(_, r)| r.w > 0.0 && r.h > 0.0)
        .collect();
    assert!(!controls.is_empty());
    for (label, r) in &controls {
        assert!(
            r.x >= -0.5 && r.x + r.w <= width + 0.5,
            "{label:?} is clipped horizontally at {width}x{height}: {r:?}"
        );
    }
    let on_screen: Vec<&(String, Rect)> = controls
        .iter()
        .filter(|(_, r)| r.y >= 0.0 && r.y + r.h <= height)
        .collect();
    for (i, (a, ra)) in on_screen.iter().enumerate() {
        for (b, rb) in &on_screen[i + 1..] {
            // A list row legitimately contains nothing else; rows vs rows
            // and controls vs controls must not overlap.
            assert!(
                !overlaps(*ra, *rb),
                "{a:?} {ra:?} overlaps {b:?} {rb:?} at {width}x{height}"
            );
        }
    }
}

#[test]
fn main_window_controls_fit_at_minimum_narrow_and_default_sizes() {
    init();
    for (w, h) in [
        (720.0, 480.0),
        (859.0, 560.0),
        (860.0, 560.0),
        (1000.0, 720.0),
    ] {
        let ui = main_window(w, h, 2);
        assert_controls_fit(&ui, w, h);
        // The bottom action bars stay fully visible.
        for label in ["Edit", "Add Tunnel", "Export all tunnels (Ctrl+S)"] {
            let r = rect(&labelled(&ui, label));
            assert!(r.y + r.h <= h, "{label} below the window at {w}x{h}: {r:?}");
        }
        let narrow = w < crate::layout::NARROW_BREAKPOINT;
        assert_eq!(ui.global::<Theme>().get_narrow(), narrow);
        // The extra Help button only exists when there is room for it.
        assert_eq!(
            ElementHandle::find_by_accessible_label(&ui, "Help")
                .next()
                .is_none(),
            narrow
        );
    }
}

#[test]
fn other_tabs_fit_at_the_minimum_size() {
    init();
    let ui = main_window(720.0, 480.0, 1);
    for tab in [1, 2] {
        ui.set_active_tab(tab);
        assert_controls_fit(&ui, 720.0, 480.0);
    }
}

#[test]
fn editor_fits_at_its_minimum_size() {
    init();
    let ed = EditWindow::new().unwrap();
    ed.window().set_size(LogicalSize::new(
        crate::layout::EDITOR_MIN.0,
        crate::layout::EDITOR_MIN.1,
    ));
    ed.show().unwrap();
    ed.set_is_new(true);
    ed.set_form_mode(true);
    ed.set_recovery("Your draft is kept. Retry, re-edit it, or discard it.".into());
    assert_controls_fit(
        &ed,
        crate::layout::EDITOR_MIN.0,
        crate::layout::EDITOR_MIN.1,
    );
    for label in ["Cancel", "Create", "Retry", "Re-edit", "Discard"] {
        let r = rect(&labelled(&ed, label));
        assert!(r.y + r.h <= crate::layout::EDITOR_MIN.1, "{label}: {r:?}");
    }
}

#[test]
fn tabbing_through_the_editor_form_keeps_the_focused_field_visible() {
    init();
    let ed = EditWindow::new().unwrap();
    ed.window().set_size(LogicalSize::new(520.0, 520.0));
    ed.show().unwrap();
    ed.set_form_mode(true);
    ed.set_show_advanced_fields(true);
    let top = rect(&labelled(&ed, "Config text"));
    let fields = [
        "Private key",
        "Address",
        "DNS",
        "Listen port",
        "MTU",
        "Public key",
        "Preshared key",
        "Allowed IPs",
        "Endpoint",
        "Persistent keepalive",
    ];
    let mut seen = std::collections::HashSet::new();
    for _ in 0..60 {
        press(ed.window(), Key::Tab);
        // Type a marker to learn which field has focus, then remove it.
        press(ed.window(), "x");
        let values = [
            ed.get_f_private_key(),
            ed.get_f_address(),
            ed.get_f_dns(),
            ed.get_f_listen_port(),
            ed.get_f_mtu(),
            ed.get_f_peer_public_key(),
            ed.get_f_preshared_key(),
            ed.get_f_allowed_ips(),
            ed.get_f_endpoint(),
            ed.get_f_keepalive(),
        ];
        if let Some(index) = values.iter().position(|v| v == "x") {
            let field = labelled(&ed, fields[index]);
            let r = rect(&field);
            let bottom = rect(&labelled(&ed, "Cancel")).y;
            assert!(
                r.y >= top.y + top.h - 0.5 && r.y + r.h <= bottom + 0.5,
                "focused {} at {r:?} is outside the visible form ({}..{bottom})",
                fields[index],
                top.y + top.h
            );
            seen.insert(index);
        }
        press(ed.window(), Key::Backspace);
    }
    assert_eq!(
        seen.len(),
        fields.len(),
        "not every field was reachable with Tab: {seen:?}"
    );
}

#[test]
fn every_interactive_element_has_a_role_and_a_label() {
    init();
    let ui = main_window(1000.0, 720.0, 7);
    let ed = EditWindow::new().unwrap();
    ed.set_form_mode(true);
    ed.set_is_new(true);
    let help = HelpWindow::new().unwrap();
    help.set_items(ui_bridge::markdown::md_items_model("# Help\n\ntext"));
    let check = |what: &str, elements: Vec<ElementHandle>| {
        assert!(!elements.is_empty(), "{what}");
        for e in elements {
            let label = e.accessible_label().unwrap_or_default();
            assert!(
                !label.trim().is_empty(),
                "{what}: {:?} element without an accessible label at {:?}",
                e.accessible_role(),
                rect(&e)
            );
        }
    };
    check("main window", interactive(&ui));
    check("editor", interactive(&ed));
    check("help", interactive(&help));
    check(
        "diagnostics",
        interactive(&crate::DiagWindow::new().unwrap()),
    );
    check("updates", interactive(&crate::UpdateWindow::new().unwrap()));
    check("about", interactive(&crate::AboutWindow::new().unwrap()));
    check("setup", interactive(&crate::SetupWindow::new().unwrap()));
    check("qr", interactive(&crate::QrWindow::new().unwrap()));
    // Buttons expose a default action that performs the click.
    let log = record(&ui);
    labelled(&ui, "Diagnose").invoke_accessible_default_action();
    labelled(&ui, "Advanced").invoke_accessible_default_action();
    assert_eq!(*log.borrow(), vec!["diagnose_conn", "persist_advanced"]);
}

#[test]
fn every_main_window_action_is_reachable_with_the_keyboard() {
    init();
    // Fresh window per probe: Tab k times, press Space, see what fired.
    let mut reached = std::collections::BTreeSet::new();
    for k in 1..=70 {
        let ui = main_window(1000.0, 720.0, 7);
        let log = record(&ui);
        for _ in 0..k {
            press(ui.window(), Key::Tab);
        }
        press(ui.window(), " ");
        reached.extend(log.borrow().iter().cloned());
    }
    for action in [
        "deactivate",
        "diagnose_conn",
        "show_qr",
        "begin_edit",
        "delete_tunnel",
        "export_zip",
        "show_help",
        "select",
        "persist_advanced",
        "set_autostart",
        "copy_value",
        "peer_page_changed",
        "refresh_log",
        "refresh_backups",
        "show_running",
        "persist_live",
    ] {
        assert!(
            reached.contains(action),
            "{action} not reachable by Tab+Space; reached {reached:?}"
        );
    }
}

#[test]
fn keyboard_shortcuts_dispatch_their_actions() {
    init();
    let cases: [(&str, Option<&str>, &str); 12] = [
        ("o", Some("ctrl"), "import_file"),
        ("i", Some("ctrl"), "import_qr"),
        ("n", Some("ctrl"), "add_empty"),
        ("e", Some("ctrl"), "begin_edit"),
        ("t", Some("ctrl"), "deactivate"),
        ("d", Some("ctrl"), "diagnose_conn"),
        ("r", Some("ctrl"), "refresh_tunnels"),
        ("s", Some("ctrl"), "export_zip"),
        ("u", Some("ctrl"), "show_update"),
        ("2", Some("ctrl"), "refresh_log"),
        ("F1", None, "show_help"),
        ("F5", None, "refresh_tunnels"),
    ];
    for (text, modifier, expected) in cases {
        let ui = main_window(1000.0, 720.0, 1);
        let log = record(&ui);
        let text: SharedString = match text {
            "F1" => Key::F1.into(),
            "F5" => Key::F5.into(),
            t => t.into(),
        };
        match modifier {
            Some(_) => ctrl(ui.window(), &text),
            None => press(ui.window(), text.clone()),
        }
        assert_eq!(
            *log.borrow(),
            vec![expected.to_string()],
            "shortcut {text:?}"
        );
    }
    // Arrow keys move the list selection from a focused row.
    let ui = main_window(1000.0, 720.0, 1);
    let log = record(&ui);
    let picked = Rc::new(RefCell::new(Vec::new()));
    {
        let picked = picked.clone();
        ui.on_select(move |i| picked.borrow_mut().push(i));
    }
    for _ in 0..4 {
        press(ui.window(), Key::Tab);
        press(ui.window(), Key::DownArrow);
        if !picked.borrow().is_empty() {
            break;
        }
    }
    assert_eq!(*picked.borrow(), vec![1], "{:?}", log.borrow());
}

#[test]
fn many_peers_are_paged_and_filterable() {
    init();
    let ui = main_window(1000.0, 720.0, 12);
    // Cards are created lazily as they scroll into view, so check the
    // rendered page model (what the cards are built from) and the first card.
    let peer_cards = |ui: &MainWindow| -> Vec<String> {
        let peers = ui.get_detail().peers;
        (0..slint::Model::row_count(&peers))
            .map(|i| format!("Peer {}", slint::Model::row_data(&peers, i).unwrap().number))
            .collect()
    };
    assert!(
        ElementHandle::find_by_accessible_label(&ui, "Peer 1")
            .next()
            .is_some()
    );
    assert_eq!(
        peer_cards(&ui),
        ["Peer 1", "Peer 2", "Peer 3", "Peer 4", "Peer 5"]
    );
    assert_eq!(ui.get_peer_page_label(), "Peers 1–5 of 12");
    {
        let weak = ui.as_weak();
        ui.on_peer_page_changed(move |page| {
            let ui = weak.upgrade().unwrap();
            ui.set_peer_page(page);
            ui_bridge::rerender_detail(&ui);
        });
    }
    labelled(&ui, "Next peers").invoke_accessible_default_action();
    labelled(&ui, "Next peers").invoke_accessible_default_action();
    assert_eq!(peer_cards(&ui), ["Peer 11", "Peer 12"]);
    // The last page has no further Next.
    assert_eq!(
        labelled(&ui, "Next peers").accessible_enabled(),
        Some(false)
    );
    ui.set_peer_filter("vpn7.example".into());
    ui.set_peer_page(0);
    ui_bridge::rerender_detail(&ui);
    assert_eq!(peer_cards(&ui), ["Peer 7"]);
    assert_eq!(ui.get_peer_page_label(), "1 peer (filtered from 12)");
}

#[test]
fn detail_pane_scrolls_from_the_keyboard_at_small_sizes() {
    init();
    let ui = main_window(720.0, 480.0, 5);
    let card = || rect(&labelled(&ui, "Interface: wg0"));
    let before = card().y;
    press(ui.window(), Key::PageDown);
    let after = card().y;
    assert!(
        after < before - 50.0,
        "PageDown did not scroll: {before} -> {after}"
    );
    press(ui.window(), Key::PageUp);
    assert!((card().y - before).abs() < 0.5);
}

#[test]
fn themes_switch_palette_and_plain_text_glyphs() {
    init();
    let ui = main_window(1000.0, 720.0, 2);
    let theme = ui.global::<Theme>();
    assert_eq!(theme.get_copy_glyph(), "⧉");
    theme.set_high_contrast(true);
    assert_eq!(theme.get_text(), slint::Color::from_rgb_u8(0, 0, 0));
    assert!(theme.get_focus_width() >= 3.0);
    theme.set_ascii(true);
    assert_eq!(theme.get_copy_glyph(), "[copy]");
    // Every label and text on screen is plain ASCII in this mode.
    for e in ElementQuery::from_root(&ui).find_all() {
        if let Some(label) = e.accessible_label() {
            assert!(
                label.is_ascii(),
                "non-ASCII text in plain-text theme: {label:?}"
            );
        }
    }
}
