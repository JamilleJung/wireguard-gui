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

/// Is `cmd` on PATH (or in a standard bin dir)? Read-only, no root.
pub fn which(cmd: &str) -> bool {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for d in ["/usr/bin", "/usr/sbin", "/bin", "/sbin", "/usr/local/bin"] {
        dirs.push(PathBuf::from(d));
    }
    dirs.iter().any(|d| d.join(cmd).exists())
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

/// A copy-pasteable command to install `wireguard-tools` on this distro.
pub fn install_tools_hint() -> String {
    match pkg_manager() {
        Some("apt-get") => "sudo apt install wireguard-tools",
        Some("dnf") => "sudo dnf install wireguard-tools",
        Some("yum") => "sudo yum install wireguard-tools",
        Some("pacman") => "sudo pacman -S wireguard-tools",
        Some("zypper") => "sudo zypper install wireguard-tools",
        Some("apk") => "sudo apk add wireguard-tools",
        Some("xbps-install") => "sudo xbps-install -S wireguard-tools",
        Some("eopkg") => "sudo eopkg install wireguard-tools",
        _ => "install the 'wireguard-tools' package with your distro's package manager",
    }
    .to_string()
}

/// The non-interactive shell command (to run as root) that installs
/// `wireguard-tools`, for an automatic fix; `None` on an unknown distro.
pub fn install_tools_command() -> Option<String> {
    let c = match pkg_manager()? {
        "apt-get" => "apt-get update && apt-get install -y wireguard-tools",
        "dnf" => "dnf install -y wireguard-tools",
        "yum" => "yum install -y wireguard-tools",
        "pacman" => "pacman -Sy --noconfirm wireguard-tools",
        "zypper" => "zypper --non-interactive install wireguard-tools",
        "apk" => "apk add wireguard-tools",
        "xbps-install" => "xbps-install -Sy wireguard-tools",
        "eopkg" => "eopkg install -y wireguard-tools",
        _ => return None,
    };
    Some(c.to_string())
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
fn helper_auth(helper: &Option<String>) -> (Status, String, Option<String>) {
    let Some(h) = helper else {
        return (
            Status::Unknown,
            "install the helper first".to_string(),
            None,
        );
    };
    if unsafe { libc::geteuid() } == 0 {
        return (Status::Ok, "running as root".to_string(), None);
    }
    let sudo_ok = Command::new("sudo")
        .args(["-n", h, "list"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if sudo_ok {
        return (Status::Ok, "passwordless (sudoers)".to_string(), None);
    }
    let polkit = [
        "/etc/polkit-1/rules.d/49-wireguard-tui.rules",
        "/etc/polkit-1/rules.d/49-wireguard-gui.rules",
        "/usr/share/polkit-1/rules.d/49-wireguard-tui.rules",
        "/usr/share/polkit-1/rules.d/49-wireguard-gui.rules",
    ]
    .iter()
    .any(|p| PathBuf::from(p).exists());
    if polkit {
        (
            Status::Warning,
            "polkit (asks for your password)".to_string(),
            None,
        )
    } else {
        (
            Status::Warning,
            "not set up - pkexec will prompt each time".to_string(),
            Some("./install.sh   (sets up passwordless sudoers)".to_string()),
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
    if installed_helper_path().is_none() {
        v.push(
            "the privileged helper is not installed - run ./install.sh or install the package"
                .to_string(),
        );
    }
    v
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

    let (auth_status, auth_detail, auth_fix) = helper_auth(&helper);
    checks.push(Check {
        name: "Helper authorization",
        status: auth_status,
        detail: auth_detail,
        fix: auth_fix,
        critical: false,
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
