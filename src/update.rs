//! GitHub-based, dependency-light auto-update.
//!
//! The whole flow is built on external programs (`curl`, `minisign`,
//! `sha256sum`, `tar`) driven via `std::process` — there is no new Rust crate.
//!
//! Trust chain (matches exactly what the release workflow signs):
//!   1. Verify `SHA256SUMS.minisig` against the **compiled-in** `minisign.pub`
//!      (baked with `include_str!`, so it can't be swapped on disk).
//!   2. With `SHA256SUMS` now trusted, verify the downloaded tarball's digest
//!      appears in it.
//!
//! No binary is replaced before [`verify_release`] returns `Ok`.
//!
//! APPLY re-runs the verified tarball's own `install.sh` — the same trusted
//! path as the initial install — so there is no new privileged surface.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const OWNER: &str = "JamilleJung";
const REPO: &str = "wireguard-gui";
const CURRENT: &str = env!("CARGO_PKG_VERSION");
/// GitHub requires a User-Agent on API requests.
const UA: &str = concat!("wireguard-gui/", env!("CARGO_PKG_VERSION"));

/// The public key committed to the repo, baked into the binary at build time so
/// it cannot be swapped on disk. Never trust an on-disk copy or the release's
/// own `minisign.pub` asset.
const MINISIGN_PUB: &str = include_str!("../minisign.pub");

/// A detected update: the version we are running vs. the latest published one.
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
}

/// One curl call, hard caps so it can never hang the UI.
fn curl(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "8",
            "--connect-timeout",
            "5",
            "--retry",
            "0",
            "-A",
            UA,
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
            url,
        ])
        .output()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl failed ({})", out.status));
    }
    Ok(out.stdout)
}

/// Download a URL to a file on disk (used for the release assets).
fn curl_to(url: &str, dest: &Path) -> Result<(), String> {
    let st = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--tlsv1.2",
            "--max-time",
            "120",
            "--connect-timeout",
            "5",
            "--retry",
            "1",
            "-A",
            UA,
            "-o",
        ])
        .arg(dest)
        .arg(url)
        .status()
        .map_err(|e| format!("curl spawn: {e}"))?;
    if !st.success() {
        return Err(format!("download failed ({st}): {url}"));
    }
    Ok(())
}

/// Latest published tag, e.g. "1.8.0" (leading 'v' stripped).
pub fn latest_tag() -> Result<String, String> {
    let body = curl(&format!(
        "https://api.github.com/repos/{OWNER}/{REPO}/releases/latest"
    ))?;
    let body = String::from_utf8_lossy(&body);
    extract_json_string(&body, "tag_name")
        .map(|t| t.trim_start_matches('v').to_string())
        .ok_or_else(|| "no tag_name in release JSON".to_string())
}

/// Minimal, dependency-free extraction of a top-level JSON string field
/// `"key":"value"`. Sufficient for tag_name (a flat ASCII value). Handles
/// surrounding whitespace; refuses values containing control/escape chars.
fn extract_json_string(json: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let mut idx = json.find(&needle)? + needle.len();
    let bytes = json.as_bytes();
    while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || bytes[idx] != b':' {
        return None;
    }
    idx += 1;
    while idx < bytes.len() && (bytes[idx] as char).is_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || bytes[idx] != b'"' {
        return None;
    }
    idx += 1;
    let start = idx;
    while idx < bytes.len() && bytes[idx] != b'"' {
        if bytes[idx] == b'\\' {
            return None; // refuse escapes -> not a plain tag
        }
        idx += 1;
    }
    let val = &json[start..idx];
    if val.is_empty() || val.len() > 32 {
        return None;
    }
    Some(val.to_string())
}

/// Parse a strict `MAJOR.MINOR.PATCH` triple (release tags are validated against
/// `^v[0-9]+\.[0-9]+\.[0-9]+$` in the workflow, so this is exact).
fn parse(v: &str) -> Option<(u64, u64, u64)> {
    let mut it = v.trim().trim_start_matches('v').split('.');
    let a = it.next()?.parse().ok()?;
    let b = it.next()?.parse().ok()?;
    let c = it.next()?.parse().ok()?;
    if it.next().is_some() {
        return None; // reject 4+ components
    }
    Some((a, b, c))
}

/// `Some(true)` iff `remote` is strictly newer than the compiled-in version.
/// `None` if either side is unparseable (never offers an update then).
pub fn is_newer(remote: &str) -> Option<bool> {
    Some(parse(remote)? > parse(CURRENT)?)
}

/// Query GitHub and decide whether an update is available.
/// `Ok(None)` when up-to-date or the version is unparseable; offline/curl error
/// → `Err` (callers treat that as a silent no-op).
pub fn check() -> Result<Option<UpdateInfo>, String> {
    let latest = latest_tag()?;
    match is_newer(&latest) {
        Some(true) => Ok(Some(UpdateInfo {
            current: CURRENT.to_string(),
            latest,
        })),
        _ => Ok(None),
    }
}

/// Best CPU architecture token for the tarball name, matching the release
/// workflow's asset suffixes.
fn arch_token() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    }
}

/// Download the latest tarball + `SHA256SUMS` + `SHA256SUMS.minisig`, verify
/// them, unpack, and stage the prebuilt binaries so the installer can reuse
/// them. Returns the unpacked tarball directory (containing `install.sh`).
pub fn download_and_verify() -> Result<PathBuf, String> {
    let latest = latest_tag()?;
    let arch = arch_token();
    let tarname = format!("{REPO}-{latest}-{arch}-linux.tar.gz");
    let base = format!("https://github.com/{OWNER}/{REPO}/releases/download/v{latest}");

    // A private scratch dir under the user's runtime/tmp dir.
    let workdir = std::env::temp_dir().join(format!("wg-update.{}", std::process::id()));
    let _ = fs::remove_dir_all(&workdir);
    fs::create_dir_all(&workdir).map_err(|e| format!("mkdir: {e}"))?;

    curl_to(&format!("{base}/{tarname}"), &workdir.join(&tarname))?;
    curl_to(&format!("{base}/SHA256SUMS"), &workdir.join("SHA256SUMS"))?;
    curl_to(
        &format!("{base}/SHA256SUMS.minisig"),
        &workdir.join("SHA256SUMS.minisig"),
    )?;

    verify_release(&workdir)?;

    // Unpack the verified tarball.
    let st = Command::new("tar")
        .args(["-xzf", &tarname])
        .current_dir(&workdir)
        .status()
        .map_err(|e| format!("tar spawn: {e}"))?;
    if !st.success() {
        return Err("failed to unpack the verified tarball".into());
    }
    let tardir = workdir.join(format!("{REPO}-{latest}-{arch}-linux"));
    if !tardir.is_dir() {
        return Err("unexpected tarball layout".into());
    }

    // install.sh reuses target/release/<bin> when WG_USE_PREBUILT=1 (skipping a
    // compile), so stage the unpacked binaries there.
    let rel = tardir.join("target").join("release");
    fs::create_dir_all(&rel).map_err(|e| format!("mkdir: {e}"))?;
    for bin in ["wireguard-gui", "wg-helper"] {
        let src = tardir.join(bin);
        if src.exists() {
            fs::rename(&src, rel.join(bin)).map_err(|e| format!("stage {bin}: {e}"))?;
        }
    }

    Ok(tardir)
}

/// Authenticate the downloaded artifact against the baked-in trust anchor.
/// `workdir` must contain the tarball, `SHA256SUMS`, and `SHA256SUMS.minisig`.
pub fn verify_release(workdir: &Path) -> Result<(), String> {
    ensure_minisign()?; // installs via PM once, else Err -> caller refuses to apply

    // 1) Write the *baked-in* pubkey to a temp file (never an on-disk one, and
    //    never the minisign.pub shipped in the release).
    let pubfile = workdir.join("trusted.pub");
    fs::write(&pubfile, MINISIGN_PUB).map_err(|e| e.to_string())?;

    // 2) Verify SHA256SUMS against the trusted pubkey. minisign exits nonzero on
    //    any mismatch.
    let st = Command::new("minisign")
        .arg("-V")
        .arg("-p")
        .arg(&pubfile)
        .arg("-m")
        .arg(workdir.join("SHA256SUMS"))
        .current_dir(workdir)
        .status()
        .map_err(|e| format!("minisign spawn: {e}"))?;
    if !st.success() {
        return Err("signature verification FAILED — refusing to update".into());
    }

    // 3) SHA256SUMS is now trusted; verify the tarball's digest is listed in it.
    //    --ignore-missing lets the single downloaded tarball line match even
    //    though SHA256SUMS lists every release asset.
    let st = Command::new("sha256sum")
        .args(["-c", "--ignore-missing", "--strict", "SHA256SUMS"])
        .current_dir(workdir)
        .status()
        .map_err(|e| format!("sha256sum spawn: {e}"))?;
    if !st.success() {
        return Err("checksum mismatch on downloaded tarball — refusing".into());
    }
    Ok(())
}

/// Install the verified update by re-running the tarball's own `install.sh`.
/// With `WG_USE_PREBUILT=1` set, install.sh reuses the staged prebuilt binaries
/// (no compile).
///
/// Privilege escalation depends on how we're running:
///   * already root — run `bash install.sh` directly;
///   * otherwise, if `pkexec` is on PATH — escalate via pkexec, which pops a
///     graphical polkit dialog (no controlling tty needed, unlike install.sh's
///     bare `sudo`). We hand the invoking user to install.sh via `WG_REAL_USER`
///     so the per-user helper grant still targets the human;
///   * otherwise — fail with a clear message (no non-interactive way to escalate
///     from a tty-less desktop session).
pub fn apply(tardir: &Path) -> Result<(), String> {
    let installer = tardir.join("install.sh");
    if !installer.exists() {
        return Err("verified tarball has no install.sh".into());
    }

    let st = if unsafe { libc::geteuid() } == 0 {
        // Already root: run install.sh directly, in the tarball dir so $HERE
        // resolves to the staged target/release/ binaries.
        Command::new("bash")
            .arg(&installer)
            .env("WG_USE_PREBUILT", "1")
            .current_dir(tardir)
            .status()
            .map_err(|e| format!("install.sh spawn: {e}"))?
    } else if which("pkexec") {
        // Escalate via pkexec: it shows a graphical polkit auth dialog (no tty
        // required) and runs install.sh as root with a sanitized environment.
        // pkexec resets cwd, so pass the ABSOLUTE installer path (so install.sh's
        // $HERE resolves to tardir and finds the staged target/release/). The env
        // vars are passed through `env KEY=VAL` so they survive pkexec's env
        // sanitization. WG_REAL_USER keeps the per-user helper grant on the human.
        let user = invoking_user();
        let installer_abs =
            fs::canonicalize(&installer).map_err(|e| format!("resolve install.sh path: {e}"))?;
        Command::new("pkexec")
            .arg("env")
            .arg(format!("WG_REAL_USER={user}"))
            .arg("WG_USE_PREBUILT=1")
            .arg("bash")
            .arg(&installer_abs)
            .status()
            .map_err(|e| format!("pkexec spawn: {e}"))?
    } else {
        return Err("cannot escalate privileges automatically — run install.sh \
                    manually to finish the update"
            .into());
    };

    if !st.success() {
        return Err(format!("install.sh exited with {st}"));
    }
    Ok(())
}

/// The login name of the user who invoked us, for `WG_REAL_USER` so the
/// installer's per-user helper grant targets the human. Resolves via
/// `getpwuid(getuid())`, falling back to `$USER`.
fn invoking_user() -> String {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if !pw.is_null()
            && !(*pw).pw_name.is_null()
            && let Ok(name) = std::ffi::CStr::from_ptr((*pw).pw_name).to_str()
            && !name.is_empty()
        {
            return name.to_string();
        }
    }
    std::env::var("USER").unwrap_or_default()
}

/// Whether `prog` is on `$PATH`.
fn which(prog: &str) -> bool {
    Command::new("sh")
        .arg("-c")
        .arg(format!("command -v {prog} >/dev/null 2>&1"))
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Make sure the `minisign` CLI is available, attempting a single best-effort
/// install via the detected package manager. Fail-closed: if it still isn't
/// present, refuse to update.
fn ensure_minisign() -> Result<(), String> {
    if which("minisign") {
        return Ok(());
    }
    try_install_minisign();
    if which("minisign") {
        Ok(())
    } else {
        Err(
            "`minisign` is not installed and could not be installed; cannot \
             verify the update — aborting (run install.sh to update manually)"
                .into(),
        )
    }
}

/// Best-effort single attempt to install `minisign` via the same package
/// managers install.sh supports. Silently gives up on any failure.
fn try_install_minisign() {
    // Root-escalation prefix, mirroring backend::init()'s choice.
    let sudo: &[&str] = if unsafe { libc::geteuid() } == 0 {
        &[]
    } else if which("sudo") {
        &["sudo", "-n"]
    } else {
        return; // no non-interactive way to gain privilege
    };
    let attempts: &[&[&str]] = &[
        &["apt-get", "install", "-y", "minisign"],
        &["dnf", "install", "-y", "minisign"],
        &["pacman", "-Sy", "--noconfirm", "minisign"],
        &["zypper", "--non-interactive", "install", "minisign"],
        &["apk", "add", "--no-cache", "minisign"],
    ];
    for cmd in attempts {
        if !which(cmd[0]) {
            continue;
        }
        let mut full: Vec<&str> = sudo.to_vec();
        full.extend_from_slice(cmd);
        let _ = Command::new(full[0]).args(&full[1..]).status();
        if which("minisign") {
            return;
        }
    }
}

/// Path of a per-app state file under $XDG_CONFIG_HOME (or ~/.config)
/// /wireguard-gui/<name>. Mirrors `mode_state_path()` in main.rs.
fn state_path(name: &str) -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("wireguard-gui").join(name))
}

/// Whether the startup update check is disabled. `WG_NO_UPDATE_CHECK` disables
/// at runtime; a persistent `no-update` file disables it across runs.
pub fn disabled() -> bool {
    if std::env::var_os("WG_NO_UPDATE_CHECK").is_some() {
        return true;
    }
    state_path("no-update").is_some_and(|p| p.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_json_string_basic() {
        let j = r#"{"name":"v1.2.3","tag_name":"v1.8.0","draft":false}"#;
        assert_eq!(
            extract_json_string(j, "tag_name").as_deref(),
            Some("v1.8.0")
        );
    }

    #[test]
    fn extract_json_string_whitespace() {
        let j = "{ \"tag_name\" :  \"1.9.0\" }";
        assert_eq!(extract_json_string(j, "tag_name").as_deref(), Some("1.9.0"));
    }

    #[test]
    fn extract_json_string_missing_key() {
        let j = r#"{"name":"x"}"#;
        assert_eq!(extract_json_string(j, "tag_name"), None);
    }

    #[test]
    fn extract_json_string_rejects_escapes() {
        // A backslash escape inside the value is refused (not a plain tag).
        // Literal JSON: {"tag_name":"v1\"2"}
        let j = "{\"tag_name\":\"v1\\\"2\"}";
        assert_eq!(extract_json_string(j, "tag_name"), None);
    }

    #[test]
    fn parse_valid_triple() {
        assert_eq!(parse("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse("10.0.7"), Some((10, 0, 7)));
    }

    #[test]
    fn parse_rejects_four_components() {
        assert_eq!(parse("1.2.3.4"), None);
    }

    #[test]
    fn parse_rejects_non_numeric() {
        assert_eq!(parse("1.2.x"), None);
        assert_eq!(parse("abc"), None);
    }

    #[test]
    fn is_newer_compares() {
        // CURRENT is the compiled-in version; compare against synthetic values.
        let (a, b, c) = parse(CURRENT).unwrap();
        let older = format!("{a}.{b}.{}", c.saturating_sub(1).min(c));
        // A clearly-higher major is always newer.
        assert_eq!(is_newer(&format!("{}.0.0", a + 1)), Some(true));
        // Equal is not newer.
        assert_eq!(is_newer(CURRENT), Some(false));
        // Unparseable -> None.
        assert_eq!(is_newer("not.a.version"), None);
        // Lower patch (when CURRENT has a nonzero patch) is not newer.
        if c > 0 {
            assert_eq!(is_newer(&older), Some(false));
        }
    }
}
