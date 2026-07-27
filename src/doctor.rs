// Shared, read-only "first-run / doctor" system check. No root required for the
// checks; it never modifies the system. Identical in wireguard-gui and
// wireguard-tui so the two stay consistent.
#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Severity of a single check.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Warning,
    Missing,
    Failed,
    Unknown,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Ok => "OK",
            Status::Warning => "WARN",
            Status::Missing => "MISSING",
            Status::Failed => "FAILED",
            Status::Unknown => "UNKNOWN",
        }
    }
}

/// One line of the system check.
pub struct Check {
    pub name: &'static str,
    pub status: Status,
    pub detail: String,
    /// A copy-pasteable command/hint to fix it, if applicable.
    pub fix: Option<String>,
    /// Whether the app is essentially unusable without this.
    pub critical: bool,
}

/// The full system check.
pub struct Report {
    pub checks: Vec<Check>,
}

impl Report {
    /// True when nothing critical is missing/failed (the app can be used).
    pub fn critical_ok(&self) -> bool {
        !self
            .checks
            .iter()
            .any(|c| c.critical && matches!(c.status, Status::Missing | Status::Failed))
    }
    /// 0 = all OK, 1 = warnings only, 2 = something critical missing/failed.
    pub fn exit_code(&self) -> i32 {
        if !self.critical_ok() {
            2
        } else if self.checks.iter().any(|c| !matches!(c.status, Status::Ok)) {
            1
        } else {
            0
        }
    }
    /// Critical items that are missing/failed (for friendly summaries).
    pub fn blocking(&self) -> Vec<&Check> {
        self.checks
            .iter()
            .filter(|c| c.critical && matches!(c.status, Status::Missing | Status::Failed))
            .collect()
    }
}

/// Whether the system clock is disciplined by NTP. `None` if it can't be told.
/// Read-only adjtimex (modes stays 0) — never changes the clock.
pub fn clock_synced() -> Option<bool> {
    let mut tx: libc::timex = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::adjtimex(&mut tx) };
    if ret == -1 {
        return None;
    }
    Some((tx.status & libc::STA_UNSYNC) == 0)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsResolverState {
    NativeResolvconf,
    ResolvectlActive,
    ResolvectlInactive,
    Missing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DnsSetupAction {
    InstallProvider,
    EnableResolved,
}

/// Resolve `cmd` on PATH (or in a standard bin dir). Read-only, no root.
fn command_path(cmd: &str) -> Option<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in ["/usr/bin", "/usr/sbin", "/bin", "/sbin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.into_iter()
        .map(|dir| dir.join(cmd))
        .find(|path| path.exists())
}

/// Is `cmd` on PATH (or in a standard bin dir)? Read-only, no root.
pub fn which(cmd: &str) -> bool {
    command_path(cmd).is_some()
}

/// The first installed package manager we recognise.
fn pkg_manager() -> Option<&'static str> {
    [
        "apt-get",
        "dnf",
        "yum",
        "pacman",
        "zypper",
        "apk",
        "xbps-install",
        "eopkg",
    ]
    .into_iter()
    .find(|m| which(m))
}

/// Whether THIS user can actually escalate with `sudo` - not merely whether the
/// `sudo` binary exists. On many Debian servers the login user isn't in the
/// sudoers file, so presence of `sudo` is not enough. True for root, when a
/// non-interactive `sudo -n true` succeeds (passwordless / cached creds), or
/// when the user is in a typical admin group (sudo/wheel/admin).
pub fn can_sudo() -> bool {
    if unsafe { libc::geteuid() } == 0 {
        return true;
    }
    if !which("sudo") {
        return false;
    }
    let probe_ok = Command::new("sudo")
        .args(["-n", "true"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if probe_ok {
        return true;
    }
    // Password-required sudoers won't pass `-n`; fall back to group membership.
    Command::new("id")
        .arg("-nG")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|g| {
            g.split_whitespace()
                .any(|grp| matches!(grp, "sudo" | "wheel" | "admin"))
        })
        .unwrap_or(false)
}

/// Prefix a package-manager command with whatever gains root here (root → none,
/// usable sudo → `sudo`, otherwise `su root -c '…'` for boxes where the user
/// can't sudo - e.g. Debian-minimal, or a server where they're not a sudoer).
fn with_priv(pkg_cmd: &str) -> String {
    if unsafe { libc::geteuid() } == 0 {
        pkg_cmd.to_string()
    } else if can_sudo() {
        format!("sudo {pkg_cmd}")
    } else if which("su") {
        format!("su root -c '{pkg_cmd}'")
    } else {
        format!("{pkg_cmd}   (run as root)")
    }
}

/// The bare `<pkgmgr> install <pkg>` command for this distro (no privilege prefix).
fn pkg_install(pkg: &str) -> Option<String> {
    Some(match pkg_manager()? {
        "apt-get" => format!("apt install {pkg}"),
        "dnf" => format!("dnf install {pkg}"),
        "yum" => format!("yum install {pkg}"),
        "pacman" => format!("pacman -S {pkg}"),
        "zypper" => format!("zypper install {pkg}"),
        "apk" => format!("apk add {pkg}"),
        "xbps-install" => format!("xbps-install -S {pkg}"),
        "eopkg" => format!("eopkg install {pkg}"),
        _ => return None,
    })
}

/// A copy-pasteable command to install `wireguard-tools` on this distro, with a
/// privilege prefix that actually works here.
pub fn install_tools_hint() -> String {
    match pkg_install("wireguard-tools") {
        Some(c) => with_priv(&c),
        None => {
            "install the 'wireguard-tools' package with your distro's package manager".to_string()
        }
    }
}

fn classify_dns_resolver(
    canonical_resolvconf: Option<&std::path::Path>,
    resolved_active: bool,
) -> DnsResolverState {
    let Some(path) = canonical_resolvconf else {
        return DnsResolverState::Missing;
    };
    if path.file_name().is_some_and(|name| name == "resolvectl") {
        if resolved_active {
            DnsResolverState::ResolvectlActive
        } else {
            DnsResolverState::ResolvectlInactive
        }
    } else {
        DnsResolverState::NativeResolvconf
    }
}

/// Whether systemd-resolved is running. Prefer systemd's actual service state;
/// only use its runtime directory on systems without `systemctl`.
fn systemd_resolved_active() -> bool {
    if let Some(systemctl) = command_path("systemctl") {
        return Command::new(systemctl)
            .args(["is-active", "--quiet", "systemd-resolved.service"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    }
    std::path::Path::new("/run/systemd/resolve").is_dir()
}

/// Classify the resolver integration that `wg-quick` will actually execute.
pub fn dns_resolver_state() -> DnsResolverState {
    let canonical =
        command_path("resolvconf").map(|path| std::fs::canonicalize(&path).unwrap_or(path));
    classify_dns_resolver(canonical.as_deref(), systemd_resolved_active())
}

pub fn dns_setup_action(state: DnsResolverState) -> Option<DnsSetupAction> {
    match state {
        DnsResolverState::Missing => Some(DnsSetupAction::InstallProvider),
        DnsResolverState::ResolvectlInactive => Some(DnsSetupAction::EnableResolved),
        DnsResolverState::NativeResolvconf | DnsResolverState::ResolvectlActive => None,
    }
}

fn resolvconf_pkg_for(manager: Option<&str>, resolved_active: bool) -> &'static str {
    match manager {
        Some("dnf" | "yum") => "systemd-resolved",
        Some("apt-get") if resolved_active => "systemd-resolvconf",
        _ => "openresolv",
    }
}

/// The package providing a working `resolvconf` command on this system.
fn resolvconf_pkg() -> &'static str {
    resolvconf_pkg_for(pkg_manager(), systemd_resolved_active())
}

/// True when `DNS =` lines in configs can be applied by `wg-quick`. Read-only.
pub fn dns_ok() -> bool {
    matches!(
        dns_resolver_state(),
        DnsResolverState::NativeResolvconf | DnsResolverState::ResolvectlActive
    )
}

/// A copy-pasteable command to install a resolvconf provider (for `DNS =` lines).
pub fn install_resolvconf_hint() -> String {
    match pkg_install(resolvconf_pkg()) {
        Some(c) => with_priv(&c),
        None => "install 'openresolv' (or another resolvconf provider)".to_string(),
    }
}

pub fn enable_resolved_command() -> &'static str {
    "systemctl enable --now systemd-resolved"
}

pub fn enable_resolved_hint() -> String {
    with_priv(enable_resolved_command())
}

/// Non-interactive root install command for `pkg`; `None` on an unknown distro.
fn pkg_install_noninteractive(pkg: &str) -> Option<String> {
    Some(match pkg_manager()? {
        "apt-get" => format!("apt-get update && apt-get install -y {pkg}"),
        "dnf" => format!("dnf install -y {pkg}"),
        "yum" => format!("yum install -y {pkg}"),
        "pacman" => format!("pacman -Sy --noconfirm {pkg}"),
        "zypper" => format!("zypper --non-interactive install {pkg}"),
        "apk" => format!("apk add {pkg}"),
        "xbps-install" => format!("xbps-install -Sy {pkg}"),
        "eopkg" => format!("eopkg install -y {pkg}"),
        _ => return None,
    })
}

/// The non-interactive root command to install `wireguard-tools` (auto-fix).
pub fn install_tools_command() -> Option<String> {
    pkg_install_noninteractive("wireguard-tools")
}

/// The non-interactive root command to install a resolvconf provider (auto-fix).
pub fn install_resolvconf_command() -> Option<String> {
    pkg_install_noninteractive(resolvconf_pkg())
}

/// Turn a raw `wg-quick`/helper error into a concise, actionable message for
/// common, confusing failures (e.g. the missing-resolvconf one on Debian).
pub fn friendly_error(raw: &str) -> String {
    let r = raw.trim();
    let resolved_unavailable = r.contains("org.freedesktop.resolve1")
        || (r.contains("Failed to set DNS configuration")
            && (r.contains("resolvectl") || r.contains("systemd-resolved")));
    if resolved_unavailable {
        return format!(
            "this tunnel's 'DNS =' reached the resolvectl shim, but \
             systemd-resolved is unavailable - run: {}",
            enable_resolved_hint()
        );
    }
    if r.contains("resolvconf") && r.contains("not found") {
        return format!(
            "this tunnel's 'DNS =' needs a resolvconf provider - install it: {}",
            install_resolvconf_hint()
        );
    }
    r.to_string()
}

/// The installed helper path, if any (trusted locations only). Read-only.
pub fn installed_helper_path() -> Option<String> {
    [
        "/usr/local/lib/wireguard-tui/wg-helper",
        "/usr/lib/wireguard-tui/wg-helper",
        "/usr/local/lib/wireguard-gui/wg-helper",
        "/usr/lib/wireguard-gui/wg-helper",
    ]
    .into_iter()
    .find(|c| PathBuf::from(c).exists())
    .map(str::to_string)
}

/// Whether passwordless helper authorization is set up (root/sudoers/polkit).
/// Returns `(status, detail, fix, critical)`.
fn helper_auth(helper: &Option<String>) -> (Status, String, Option<String>, bool) {
    let Some(h) = helper else {
        return (
            Status::Missing,
            "install the helper first".to_string(),
            Some("./install.sh   (or install the .deb / AUR / COPR package)".to_string()),
            true,
        );
    };
    if unsafe { libc::geteuid() } == 0 {
        return (Status::Ok, "running as root".to_string(), None, false);
    }
    let sudo_ok = Command::new("sudo")
        .args(["-n", h, "list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if sudo_ok {
        return (
            Status::Ok,
            "passwordless (sudoers)".to_string(),
            None,
            false,
        );
    }
    let pkexec = which("pkexec");
    let polkit = [
        "/etc/polkit-1/rules.d/49-wireguard-tui.rules",
        "/etc/polkit-1/rules.d/49-wireguard-gui.rules",
        "/usr/share/polkit-1/rules.d/49-wireguard-tui.rules",
        "/usr/share/polkit-1/rules.d/49-wireguard-gui.rules",
    ]
    .iter()
    .any(|p| PathBuf::from(p).exists());
    if polkit && pkexec {
        (Status::Ok, "passwordless (polkit)".to_string(), None, false)
    } else if polkit {
        (
            Status::Missing,
            "polkit rule is installed, but pkexec is missing".to_string(),
            Some("install the `polkit` package (for `pkexec`) or set up sudoers".to_string()),
            true,
        )
    } else if pkexec {
        (
            Status::Warning,
            "not set up - pkexec will prompt each time".to_string(),
            Some("./install.sh   (sets up passwordless sudoers)".to_string()),
            false,
        )
    } else {
        (
            Status::Missing,
            "no passwordless sudo and pkexec is missing".to_string(),
            Some("./install.sh   (or install the .deb / AUR / COPR package)".to_string()),
            true,
        )
    }
}

/// Cheap critical-only check (no subprocess / no sudo), for app startup: returns
/// a friendly sentence per missing critical requirement, empty if good to go.
pub fn critical_blockers() -> Vec<String> {
    let mut v = Vec::new();
    if !(which("wg") && which("wg-quick")) {
        v.push(format!(
            "WireGuard tools are not installed - {}",
            install_tools_hint()
        ));
    }
    let helper = installed_helper_path();
    if helper.is_none() {
        v.push(
            "the privileged helper is not installed - run ./install.sh or install the package"
                .to_string(),
        );
    } else {
        let (_, detail, fix, critical) = helper_auth(&helper);
        if critical {
            v.push(format!(
                "helper authorization is missing - {}",
                fix.unwrap_or(detail)
            ));
        }
    }
    v
}

fn dns_check(state: DnsResolverState) -> Check {
    let (status, detail, fix) = match state {
        DnsResolverState::NativeResolvconf => (
            Status::Ok,
            "native resolvconf provider available".to_string(),
            None,
        ),
        DnsResolverState::ResolvectlActive => (
            Status::Ok,
            "resolvectl shim available; systemd-resolved is active".to_string(),
            None,
        ),
        DnsResolverState::ResolvectlInactive => (
            Status::Warning,
            "resolvectl shim is installed, but systemd-resolved is inactive".to_string(),
            Some(enable_resolved_hint()),
        ),
        DnsResolverState::Missing => (
            Status::Warning,
            "resolvconf compatibility command is missing; tunnels with 'DNS =' will fail"
                .to_string(),
            Some(install_resolvconf_hint()),
        ),
    };
    Check {
        name: "DNS for tunnels (resolvconf)",
        status,
        detail,
        fix,
        critical: false,
    }
}

/// Run all checks. Read-only; does not require root and never modifies anything.
pub fn system_check() -> Report {
    let mut checks = Vec::new();

    let (wg, wgq) = (which("wg"), which("wg-quick"));
    checks.push(Check {
        name: "WireGuard tools (wg, wg-quick)",
        status: if wg && wgq {
            Status::Ok
        } else {
            Status::Missing
        },
        detail: if wg && wgq {
            "found".to_string()
        } else {
            "not installed".to_string()
        },
        fix: if wg && wgq {
            None
        } else {
            Some(install_tools_hint())
        },
        critical: true,
    });

    // DNS integration is optional because DNS-less tunnels work without it.
    checks.push(dns_check(dns_resolver_state()));

    // A wrong system clock makes the server reject WireGuard handshakes via the
    // TAI64N anti-replay timestamp: the tunnel comes up but never connects. Not
    // critical (only matters once a tunnel is active), but a common silent cause.
    let (clock_status, clock_detail, clock_fix) = match clock_synced() {
        Some(true) => (Status::Ok, "system clock is NTP-synced".to_string(), None),
        Some(false) => (
            Status::Warning,
            "clock not NTP-synced — can break WireGuard handshakes (server anti-replay)"
                .to_string(),
            Some("enable NTP: timedatectl set-ntp true (or install chrony)".to_string()),
        ),
        None => (
            Status::Unknown,
            "could not read clock-sync status".to_string(),
            None,
        ),
    };
    checks.push(Check {
        name: "System clock (NTP)",
        status: clock_status,
        detail: clock_detail,
        fix: clock_fix,
        critical: false,
    });

    let helper = installed_helper_path();
    checks.push(Check {
        name: "Privileged helper",
        status: if helper.is_some() {
            Status::Ok
        } else {
            Status::Missing
        },
        detail: match &helper {
            Some(p) => format!("installed ({p})"),
            None => "not installed".to_string(),
        },
        fix: if helper.is_some() {
            None
        } else {
            Some("./install.sh   (or install the .deb / AUR / COPR package)".to_string())
        },
        critical: true,
    });

    let (auth_status, auth_detail, auth_fix, auth_critical) = helper_auth(&helper);
    checks.push(Check {
        name: "Helper authorization",
        status: auth_status,
        detail: auth_detail,
        fix: auth_fix,
        critical: auth_critical,
    });

    let dir = std::path::Path::new("/etc/wireguard").is_dir();
    checks.push(Check {
        name: "/etc/wireguard",
        status: if dir { Status::Ok } else { Status::Warning },
        detail: if dir {
            "exists".to_string()
        } else {
            "missing (created automatically on first save)".to_string()
        },
        fix: if dir {
            None
        } else {
            Some("sudo install -d -m 700 /etc/wireguard".to_string())
        },
        critical: false,
    });

    let systemd = which("systemctl");
    checks.push(Check {
        name: "Start-on-boot (systemd)",
        status: if systemd { Status::Ok } else { Status::Warning },
        detail: if systemd {
            "available".to_string()
        } else {
            "no systemctl - start-on-boot unavailable (everything else works)".to_string()
        },
        fix: None,
        critical: false,
    });

    let journal = which("journalctl");
    checks.push(Check {
        name: "Logs (journald)",
        status: if journal { Status::Ok } else { Status::Warning },
        detail: if journal {
            "available".to_string()
        } else {
            "no journalctl - the Log view will be empty".to_string()
        },
        fix: None,
        critical: false,
    });

    Report { checks }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn classifies_dns_resolver_capabilities() {
        assert_eq!(
            classify_dns_resolver(None, false),
            DnsResolverState::Missing
        );
        assert_eq!(
            classify_dns_resolver(Some(Path::new("/usr/sbin/openresolv")), false),
            DnsResolverState::NativeResolvconf
        );
        assert_eq!(
            classify_dns_resolver(Some(Path::new("/usr/bin/resolvectl")), true),
            DnsResolverState::ResolvectlActive
        );
        assert_eq!(
            classify_dns_resolver(Some(Path::new("/usr/bin/resolvectl")), false),
            DnsResolverState::ResolvectlInactive
        );
    }

    #[test]
    fn missing_resolvconf_is_missing_even_when_resolved_is_active() {
        assert_eq!(classify_dns_resolver(None, true), DnsResolverState::Missing);
    }

    #[test]
    fn resolver_setup_action_matches_state() {
        assert_eq!(
            dns_setup_action(DnsResolverState::Missing),
            Some(DnsSetupAction::InstallProvider)
        );
        assert_eq!(
            dns_setup_action(DnsResolverState::ResolvectlInactive),
            Some(DnsSetupAction::EnableResolved)
        );
        assert_eq!(dns_setup_action(DnsResolverState::NativeResolvconf), None);
        assert_eq!(dns_setup_action(DnsResolverState::ResolvectlActive), None);
    }

    #[test]
    fn resolver_package_policy_matches_package_manager() {
        assert_eq!(resolvconf_pkg_for(Some("dnf"), false), "systemd-resolved");
        assert_eq!(resolvconf_pkg_for(Some("yum"), false), "systemd-resolved");
        assert_eq!(
            resolvconf_pkg_for(Some("apt-get"), true),
            "systemd-resolvconf"
        );
        assert_eq!(resolvconf_pkg_for(Some("apt-get"), false), "openresolv");
        assert_eq!(resolvconf_pkg_for(Some("pacman"), false), "openresolv");
    }

    #[test]
    fn inactive_resolvectl_is_a_warning_with_enable_hint() {
        let check = dns_check(DnsResolverState::ResolvectlInactive);
        assert!(matches!(check.status, Status::Warning));
        assert!(check.detail.contains("inactive"));
        assert!(
            check
                .fix
                .as_deref()
                .is_some_and(|fix| fix.contains("systemctl enable --now systemd-resolved"))
        );
    }

    #[test]
    fn friendly_error_explains_inactive_systemd_resolved() {
        let raw = "resolvconf -a wg0 -m 0 -x: Could not activate remote peer \
                   'org.freedesktop.resolve1': activation request failed: unknown unit";
        let friendly = friendly_error(raw);
        assert!(friendly.contains("systemd-resolved"));
        assert!(friendly.contains("systemctl enable --now systemd-resolved"));
    }

    #[test]
    fn friendly_error_preserves_unknown_errors() {
        let raw = "wg-quick up wg0: RTNETLINK answers: File exists";
        assert_eq!(friendly_error(raw), raw);
    }

    #[test]
    fn report_exit_codes() {
        let mk = |status, critical| Check {
            name: "x",
            status,
            detail: String::new(),
            fix: None,
            critical,
        };
        // all ok -> 0
        let r = Report {
            checks: vec![mk(Status::Ok, true), mk(Status::Ok, false)],
        };
        assert_eq!(r.exit_code(), 0);
        assert!(r.critical_ok());
        // a warning only -> 1
        let r = Report {
            checks: vec![mk(Status::Ok, true), mk(Status::Warning, false)],
        };
        assert_eq!(r.exit_code(), 1);
        assert!(r.critical_ok());
        // critical missing -> 2
        let r = Report {
            checks: vec![mk(Status::Missing, true)],
        };
        assert_eq!(r.exit_code(), 2);
        assert!(!r.critical_ok());
        assert_eq!(r.blocking().len(), 1);
    }

    #[test]
    fn install_hint_is_nonempty() {
        assert!(!install_tools_hint().is_empty());
    }
}
