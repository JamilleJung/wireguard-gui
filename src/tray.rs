//! System-tray icon and menu.

use std::sync::atomic::Ordering;

use slint::ComponentHandle;

use crate::state::{TRAY_INFO, TRAY_ONLINE, TRAY_TUNNELS};
use crate::ui_bridge::{refresh_list, run_ui_task, set_status};
use crate::{MainWindow, actions, theme};

/// System-tray icon (StatusNotifierItem). Shows on KDE, and on GNOME with the
/// AppIndicator extension. Menu: Show the window, or Quit.
pub struct Tray {
    pub window: slint::Weak<MainWindow>,
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
        // Never query privileged helpers from a DBus menu/tooltip callback.
        let description = {
            let info = TRAY_INFO.lock().unwrap().clone();
            if info.is_empty() {
                theme::text("Waiting for tunnel status…")
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
        let tunnels = TRAY_TUNNELS.lock().unwrap().clone();
        let any_active = tunnels.iter().any(|(_, active)| *active);
        let active_names: Vec<String> = tunnels
            .iter()
            .filter(|(_, active)| *active)
            .map(|(name, _)| name.clone())
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
            .map(|(name, active)| {
                let name = name.clone();
                CheckmarkItem {
                    label: name.clone(),
                    checked: *active,
                    activate: Box::new(move |this: &mut Self| {
                        let name = name.clone();
                        let _ = this.window.upgrade_in_event_loop(move |ui| {
                            run_ui_task(
                                &ui,
                                "Updating tunnel…",
                                move || actions::toggle_tunnel(&name),
                                complete_tray_action,
                            );
                        });
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
                activate: Box::new(move |this: &mut Self| {
                    let _ = this.window.upgrade_in_event_loop(move |ui| {
                        run_ui_task(
                            &ui,
                            "Deactivating tunnels…",
                            actions::deactivate_all,
                            complete_tray_action,
                        );
                    });
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

pub fn complete_tray_action(ui: &MainWindow, result: Result<String, String>) {
    match result {
        Ok(message) => set_status(ui, message),
        Err(error) => {
            set_status(ui, error);
            let _ = ui.show();
            ui.window().set_minimized(false);
        }
    }
    refresh_list(ui);
}
