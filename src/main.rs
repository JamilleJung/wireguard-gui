// Hide the console window on Windows release builds (harmless on Linux).
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod actions;
mod backend;
mod clipboard;
mod config;
mod create;
mod doctor;
mod draft;
mod events;
mod layout;
mod live;
mod logging;
mod ops;
mod safe_file;
mod secrets;
mod state;
mod theme;
mod tray;
mod ui_bridge;
mod update;
mod validation;

#[cfg(test)]
mod ui_tests;

use std::sync::atomic::Ordering;
use std::time::Duration;

use slint::ComponentHandle;

slint::include_modules!();

/// What the command line asked for.
#[derive(Debug, PartialEq, Eq)]
enum Cli {
    Run { theme: Option<String> },
    Version,
    Help,
}

fn parse_args(args: &[String]) -> Result<Cli, String> {
    let mut theme = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "-V" | "--version" => return Ok(Cli::Version),
            "-h" | "--help" => return Ok(Cli::Help),
            "--theme" => {
                theme = Some(
                    iter.next()
                        .ok_or("--theme needs a value (default, high-contrast, ascii)")?
                        .clone(),
                );
            }
            other if other.starts_with("--theme=") => {
                theme = Some(other["--theme=".len()..].to_string());
            }
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }
    Ok(Cli::Run { theme })
}

fn help_text() -> String {
    format!(
        "wireguard-gui {} - a desktop GUI for managing WireGuard tunnels\n\n\
         Usage: wireguard-gui                 launch the app\n       \
         wireguard-gui --theme NAME      launch with a theme: default, high-contrast,\n                                       \
         ascii (plain-text symbols); combine with +\n       \
         wireguard-gui --version         print the version\n       \
         wireguard-gui --help            show this help\n\n\
         Environment: WG_GUI_THEME=NAME (same names), WG_GUI_LOG=PATH (structured log)",
        env!("CARGO_PKG_VERSION")
    )
}

/// Single-instance guard via an abstract Unix socket (auto-released when the
/// process exits — no stale lock files). If another instance already holds it,
/// nudge that instance to raise its window and return None so we exit quietly.
/// Otherwise return the listener; the primary keeps it alive and accepts pings.
#[cfg(target_os = "linux")]
fn single_instance_listener() -> std::io::Result<Option<std::os::unix::net::UnixListener>> {
    use std::os::linux::net::SocketAddrExt as _;
    let uid = unsafe { libc::getuid() };
    let addr = std::os::unix::net::SocketAddr::from_abstract_name(format!("wireguard-gui-{uid}"))?;
    bind_instance_listener(&addr)
}

#[cfg(target_os = "linux")]
fn bind_instance_listener(
    addr: &std::os::unix::net::SocketAddr,
) -> std::io::Result<Option<std::os::unix::net::UnixListener>> {
    use std::io::Write as _;
    use std::os::unix::net::{UnixListener, UnixStream};
    match UnixListener::bind_addr(addr) {
        Ok(listener) => Ok(Some(listener)),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            let mut stream = UnixStream::connect_addr(addr)?;
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
            stream.write_all(b"show")?;
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

#[cfg(not(target_os = "linux"))]
fn single_instance_listener() -> std::io::Result<Option<std::os::unix::net::UnixListener>> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "wireguard-gui requires Linux",
    ))
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
    let args: Vec<String> = std::env::args().skip(1).collect();
    let theme_arg = match parse_args(&args) {
        Ok(Cli::Version) => {
            println!("wireguard-gui {}", env!("CARGO_PKG_VERSION"));
            return Ok(());
        }
        Ok(Cli::Help) => {
            println!("{}", help_text());
            return Ok(());
        }
        Ok(Cli::Run { theme }) => theme,
        Err(error) => {
            eprintln!("wireguard-gui: {error}");
            std::process::exit(2);
        }
    };
    let env_theme = std::env::var("WG_GUI_THEME").ok();
    match theme::resolve(theme_arg.as_deref(), env_theme.as_deref()) {
        Ok(choice) => state::set_theme(choice),
        Err(error) => {
            eprintln!("wireguard-gui: {error}");
            std::process::exit(2);
        }
    }

    // Single-instance: if WireGuard is already running, raise the existing
    // window (the listener thread below) instead of opening a second window and
    // a second tray icon — then exit this launch.
    let instance = match single_instance_listener()? {
        Some(listener) => listener,
        None => return Ok(()),
    };

    backend::init();
    logging::log("start", &[("version", env!("CARGO_PKG_VERSION"))]);

    let ui = MainWindow::new()?;
    apply_theme!(ui);

    // Match the Wayland app_id / X11 WM_CLASS to the .desktop file basename
    // (`wireguard-gui`) so the compositor maps the window to our .desktop and
    // shows the installed icon instead of a generic placeholder. Must be set
    // AFTER the platform is initialized (MainWindow::new does that) but BEFORE
    // the window is shown.
    let _ = slint::set_xdg_app_id("wireguard-gui");

    ui_bridge::refresh_list(&ui);

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
    ui_bridge::apply_log_filter(&ui);
    ui_bridge::refresh_log_async(ui.as_weak());

    // ---- close to tray: hide instead of quitting (Quit is on the tray) ----
    // …but only if a tray actually exists to restore the window from. With no
    // SNI host, hiding would strand the app with no window and no way to quit,
    // so close quits instead.
    ui.window().on_close_requested(|| {
        if !state::TRAY_ONLINE.load(Ordering::Relaxed) {
            let _ = slint::quit_event_loop();
        }
        slint::CloseRequestResponse::HideWindow
    });

    // ---- system-tray icon (best-effort; needs SNI support on the desktop,
    // e.g. KDE, or GNOME with the AppIndicator extension). Uses libdbus on its
    // own thread, independent of the zbus stack Slint already uses. ----
    use ksni::blocking::TrayMethods;
    // `assume_sni_available` keeps the service alive even if the SNI host isn't
    // up yet at startup; the watcher_online/offline callbacks then track it.
    let tray = tray::Tray {
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

    events::wire(&ui);

    // ---- live polling: data on a BACKGROUND thread, applied on the UI thread ----
    live::start_poller();
    let _live_timer = live::start_apply_timer(&ui);

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
                            theme::text(&format!(
                                "Update available: v{} → v{}",
                                info.current, info.latest
                            ))
                            .into(),
                        );
                        ui.set_update_available(true);
                    }
                });
            }
        });
    }

    // First-run check: if something critical is missing, greet new users with a
    // friendly Setup wizard instead of an empty technical window.
    ui.show()?;
    {
        let weak = ui.as_weak();
        std::thread::spawn(move || {
            let report = doctor::system_check();
            let _ = weak.upgrade_in_event_loop(move |ui| {
                if !report.critical_ok() {
                    ui_bridge::windows::show_setup_wizard(&ui, report);
                }
            });
        });
    }
    slint::run_event_loop_until_quit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn command_line_selects_theme_or_prints_info() {
        assert_eq!(parse_args(&[]).unwrap(), Cli::Run { theme: None });
        assert_eq!(
            parse_args(&args(&["--theme", "high-contrast"])).unwrap(),
            Cli::Run {
                theme: Some("high-contrast".into())
            }
        );
        assert_eq!(
            parse_args(&args(&["--theme=ascii"])).unwrap(),
            Cli::Run {
                theme: Some("ascii".into())
            }
        );
        assert_eq!(parse_args(&args(&["--version"])).unwrap(), Cli::Version);
        assert_eq!(parse_args(&args(&["-h"])).unwrap(), Cli::Help);
        assert!(parse_args(&args(&["--theme"])).is_err());
        assert!(parse_args(&args(&["--bogus"])).is_err());
        assert!(help_text().contains("WG_GUI_THEME"));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn second_instance_notifies_the_bound_listener() {
        use std::io::Read as _;
        use std::os::linux::net::SocketAddrExt as _;
        let addr = std::os::unix::net::SocketAddr::from_abstract_name(format!(
            "wg-gui-test-{}",
            std::process::id()
        ))
        .unwrap();
        let listener = bind_instance_listener(&addr).unwrap().unwrap();
        let receiver = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut message = [0; 4];
            stream.read_exact(&mut message).unwrap();
            message
        });
        assert!(bind_instance_listener(&addr).unwrap().is_none());
        assert_eq!(receiver.join().unwrap(), *b"show");
    }
}
