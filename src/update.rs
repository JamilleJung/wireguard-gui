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
            "--proto-redir",
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
            "--proto-redir",
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
    let tag = extract_json_string(&body, "tag_name")
        .ok_or_else(|| "no tag_name in release JSON".to_string())?;
    let version = tag.strip_prefix('v').unwrap_or(&tag);
    parse(version).ok_or_else(|| "invalid release version".to_string())?;
    Ok(version.to_string())
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
        if bytes[idx] == b'\\' || bytes[idx].is_ascii_control() {
            return None; // refuse escapes -> not a plain tag
        }
        idx += 1;
    }
    if idx == bytes.len() {
        return None; // unterminated string
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
    let mut it = v.strip_prefix('v').unwrap_or(v).split('.');
    let component = |part: &str| {
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let a = component(it.next()?)?;
    let b = component(it.next()?)?;
    let c = component(it.next()?)?;
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
fn arch_token() -> Result<&'static str, String> {
    architecture_token(std::env::consts::ARCH)
}

fn architecture_token(arch: &str) -> Result<&'static str, String> {
    match arch {
        "x86_64" => Ok("x86_64"),
        "aarch64" => Ok("aarch64"),
        _ => Err(format!("no release build for this architecture ({arch})")),
    }
}

/// Owns the private download directory until the installer has finished.
/// Failed downloads, failed staging and cancelled/failed installs all clean up.
pub struct VerifiedUpdate {
    workdir: PathBuf,
    directory: PathBuf,
    version: String,
}

impl VerifiedUpdate {
    pub fn version(&self) -> &str {
        &self.version
    }
}

fn validate_update_version(version: &str) -> Result<(), String> {
    match is_newer(version) {
        Some(true) => Ok(()),
        Some(false) => Err(format!(
            "release {version} is not newer than installed {CURRENT}; update refused"
        )),
        None => Err("invalid update version".into()),
    }
}

impl Drop for VerifiedUpdate {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.workdir);
    }
}

fn stage_bundle(update: &mut VerifiedUpdate, application: &str) -> Result<(), String> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    let metadata =
        std::fs::symlink_metadata(&update.directory).map_err(|e| format!("update bundle: {e}"))?;
    if !metadata.file_type().is_dir() {
        return Err("unexpected tarball directory".into());
    }
    let required = [
        application.to_string(),
        "wg-helper".to_string(),
        "install.sh".to_string(),
        format!("packaging/{REPO}.desktop"),
        format!("packaging/{REPO}.svg"),
        format!("packaging/49-{REPO}.rules"),
    ];
    for relative in required {
        // Check every component as well as the leaf; staging must not follow a
        // tar entry symlink through packaging/, target/ or the bundle root.
        let mut current = update.directory.clone();
        for component in Path::new(&relative).components() {
            current.push(component);
            let md = std::fs::symlink_metadata(&current)
                .map_err(|e| format!("missing update asset {relative}: {e}"))?;
            if md.file_type().is_symlink() || (!md.is_file() && !md.is_dir()) {
                return Err(format!("invalid update asset: {relative}"));
            }
        }
        let md = std::fs::symlink_metadata(&current).map_err(|e| e.to_string())?;
        if !md.is_file() || md.nlink() != 1 {
            return Err(format!("invalid update asset: {relative}"));
        }
    }
    let target = update.directory.join("target");
    // Release bundles have no Cargo build directory. Reject a pre-existing
    // target rather than copying through archive-provided links or stale files.
    std::fs::create_dir(&target).map_err(|e| format!("stage target: {e}"))?;
    let release = target.join("release");
    std::fs::create_dir(&release).map_err(|e| format!("stage release: {e}"))?;
    for bin in [application, "wg-helper"] {
        let dest = release.join(bin);
        std::fs::copy(update.directory.join(bin), &dest)
            .map_err(|e| format!("stage {bin}: {e}"))?;
        std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("stage permissions: {e}"))?;
    }
    Ok(())
}

/// Download the latest tarball + `SHA256SUMS` + `SHA256SUMS.minisig`, verify
/// them, unpack, and stage the prebuilt binaries so the installer can reuse
/// them. Returns an owner that retains the verified bundle until installation ends.
pub fn download_and_verify() -> Result<VerifiedUpdate, String> {
    if !cfg!(target_os = "linux") {
        return Err("automatic release updates are supported on Linux only".into());
    }
    let latest = latest_tag()?;
    validate_update_version(&latest)?;
    let arch = arch_token()?;
    let tarname = format!("{REPO}-{latest}-{arch}-linux.tar.gz");
    let base = format!("https://github.com/{OWNER}/{REPO}/releases/download/v{latest}");

    // A private scratch dir under the user's runtime/tmp dir.
    let mut update = VerifiedUpdate {
        workdir: private_workdir()?,
        directory: PathBuf::new(),
        version: latest.clone(),
    };
    let workdir = &update.workdir;

    curl_to(&format!("{base}/{tarname}"), &workdir.join(&tarname))?;
    curl_to(&format!("{base}/SHA256SUMS"), &workdir.join("SHA256SUMS"))?;
    curl_to(
        &format!("{base}/SHA256SUMS.minisig"),
        &workdir.join("SHA256SUMS.minisig"),
    )?;

    verify_release(workdir, &tarname)?;

    // Unpack the verified tarball.
    let st = Command::new("tar")
        .args(["-xzf", &tarname])
        .current_dir(workdir)
        .status()
        .map_err(|e| format!("tar spawn: {e}"))?;
    if !st.success() {
        return Err("failed to unpack the verified tarball".into());
    }
    let tardir = workdir.join(format!("{REPO}-{latest}-{arch}-linux"));
    if !tardir.is_dir() {
        return Err("unexpected tarball layout".into());
    }

    update.directory = tardir;
    stage_bundle(&mut update, "wireguard-gui")?;
    Ok(update)
}

/// Authenticate the downloaded artifact against the baked-in trust anchor.
/// `workdir` must contain the tarball, `SHA256SUMS`, and `SHA256SUMS.minisig`.
pub fn verify_release(workdir: &Path, tarball: &str) -> Result<(), String> {
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

    // 3) Check exactly this archive. A suffix match or --ignore-missing can
    // accidentally accept a manifest that only names a different file.
    let sums = std::fs::read_to_string(workdir.join("SHA256SUMS")).map_err(|e| e.to_string())?;
    let checksum = tarball_checksum(&sums, tarball)?;
    let selected = workdir.join("selected.sha256");
    std::fs::write(&selected, checksum).map_err(|e| e.to_string())?;
    let st = Command::new("sha256sum")
        .args(["-c", "--strict"])
        .arg(&selected)
        .current_dir(workdir)
        .status()
        .map_err(|e| format!("sha256sum spawn: {e}"))?;
    if !st.success() {
        return Err("checksum mismatch on downloaded tarball - refusing".into());
    }
    Ok(())
}

fn private_workdir() -> Result<PathBuf, String> {
    use std::os::unix::ffi::{OsStrExt, OsStringExt};
    let template = std::env::temp_dir().join("wireguard-gui-update-XXXXXX");
    let mut name = std::ffi::CString::new(template.as_os_str().as_bytes())
        .map_err(|e| format!("temp dir: {e}"))?
        .into_bytes_with_nul();
    let result = unsafe { libc::mkdtemp(name.as_mut_ptr().cast()) };
    if result.is_null() {
        return Err(format!("temp dir: {}", std::io::Error::last_os_error()));
    }
    name.pop();
    Ok(PathBuf::from(std::ffi::OsString::from_vec(name)))
}

fn tarball_checksum(sums: &str, tarball: &str) -> Result<String, String> {
    let mut matching = sums.lines().filter_map(|line| {
        let (hash, name) = line.split_once("  ").or_else(|| line.split_once(" *"))?;
        (name == tarball).then_some((hash, line))
    });
    let (hash, line) = matching
        .next()
        .ok_or("downloaded tarball is not listed in SHA256SUMS - refusing")?;
    if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()) || matching.next().is_some()
    {
        return Err("invalid or duplicate tarball checksum - refusing".into());
    }
    Ok(format!("{line}\n"))
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
pub fn apply(update: &VerifiedUpdate) -> Result<(), String> {
    let tardir = &update.directory;
    let installer = tardir.join("install.sh");
    if !installer.exists() {
        return Err("verified tarball has no install.sh".into());
    }

    let st = if unsafe { libc::geteuid() } == 0 {
        // Already root: run install.sh directly, in the tarball dir so $HERE
        // resolves to the staged target/release/ binaries.
        Command::new("bash")
            .arg(&installer)
            .arg("--allow-root")
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
        let user = invoking_user()?;
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
        return Err(
            "cannot escalate privileges automatically — install pkexec/polkit or \
                    install the signed release manually"
                .into(),
        );
    };

    if !st.success() {
        return Err(format!("install.sh exited with {st}"));
    }
    Ok(())
}

/// The login name of the user who invoked us, for `WG_REAL_USER` so the
/// installer's per-user helper grant targets the human. Resolves via
/// `getpwuid(getuid())`, falling back to `$USER`.
fn invoking_user() -> Result<String, String> {
    // getpwuid returns a shared static buffer and can race other GUI threads.
    // Resolve the real invoking uid into caller-owned storage instead.
    let mut size = 4096;
    while size <= 1024 * 1024 {
        let mut buffer = vec![0u8; size];
        let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
        let mut result = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                &mut entry,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if rc == libc::ERANGE {
            size *= 2;
            continue;
        }
        if rc == 0 && !result.is_null() && !entry.pw_name.is_null() {
            let name = unsafe { std::ffi::CStr::from_ptr(entry.pw_name) }
                .to_str()
                .map_err(|_| "invoking account name is not UTF-8".to_string())?;
            if !name.is_empty() {
                return Ok(name.to_string());
            }
        }
        break;
    }
    Err("cannot resolve the invoking account for update authorization".into())
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
        .filter(|path| path.is_absolute())
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
    #[test]
    fn downloaded_release_cannot_silently_downgrade() {
        let current = super::parse(super::CURRENT).unwrap();
        assert!(super::validate_update_version(super::CURRENT).is_err());
        assert!(super::validate_update_version("0.0.0").is_err());
        assert!(super::validate_update_version("bad").is_err());
        assert!(super::validate_update_version(&format!("{}.0.0", current.0 + 1)).is_ok());
        let update = bundle_fixture();
        assert_eq!(update.version(), "1.9.2");
    }

    #[test]
    fn unsupported_gui_cpu_has_no_mismatched_update() {
        assert_eq!(super::architecture_token("x86_64").unwrap(), "x86_64");
        assert_eq!(super::architecture_token("aarch64").unwrap(), "aarch64");
        assert!(super::architecture_token("arm").is_err());
        assert!(super::architecture_token("x86").is_err());
        assert!(super::architecture_token("riscv64").is_err());
    }

    #[test]
    fn invoking_account_resolves_without_environment_fallback() {
        assert!(!super::invoking_user().unwrap().is_empty());
    }

    fn bundle_fixture() -> VerifiedUpdate {
        let workdir = private_workdir().unwrap();
        let directory = workdir.join("bundle");
        std::fs::create_dir(&directory).unwrap();
        std::fs::create_dir(directory.join("packaging")).unwrap();
        for file in [
            "wireguard-gui".to_string(),
            "wg-helper".into(),
            "install.sh".into(),
            format!("packaging/{REPO}.desktop"),
            format!("packaging/{REPO}.svg"),
            format!("packaging/49-{REPO}.rules"),
        ] {
            std::fs::write(directory.join(file), b"fixture").unwrap();
        }
        VerifiedUpdate {
            workdir,
            directory,
            version: "1.9.2".into(),
        }
    }

    #[test]
    fn update_requires_complete_bundle_before_staging() {
        let mut update = bundle_fixture();
        let path = update.workdir.clone();
        std::fs::remove_file(update.directory.join("wg-helper")).unwrap();
        assert!(stage_bundle(&mut update, "wireguard-gui").is_err());
        assert!(!update.directory.join("target").exists());
        drop(update);
        assert!(!path.exists());
    }

    #[test]
    fn update_staging_and_lifetime_cleanup() {
        use std::os::unix::fs::PermissionsExt;
        let mut update = bundle_fixture();
        let path = update.workdir.clone();
        stage_bundle(&mut update, "wireguard-gui").unwrap();
        for bin in ["wireguard-gui", "wg-helper"] {
            let file = update.directory.join("target/release").join(bin);
            assert_eq!(std::fs::read(&file).unwrap(), b"fixture");
            assert_eq!(
                std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
                0o755
            );
        }
        drop(update);
        assert!(!path.exists());
    }

    #[test]
    fn update_rejects_archive_links_and_existing_build_tree() {
        let mut update = bundle_fixture();
        std::fs::remove_file(update.directory.join("wg-helper")).unwrap();
        std::os::unix::fs::symlink("install.sh", update.directory.join("wg-helper")).unwrap();
        assert!(stage_bundle(&mut update, "wireguard-gui").is_err());
        let mut update = bundle_fixture();
        std::os::unix::fs::symlink("packaging", update.directory.join("target")).unwrap();
        assert!(stage_bundle(&mut update, "wireguard-gui").is_err());
    }
    use super::*;

    #[test]
    fn malformed_release_strings_are_rejected() {
        assert_eq!(
            extract_json_string("{\"tag_name\":\"v1.9.1", "tag_name"),
            None
        );
        assert_eq!(
            extract_json_string("{\"tag_name\":\"v1.9\n.1\"}", "tag_name"),
            None
        );
        for invalid in ["../1.2.3", "+1.2.3", "vv1.2.3", "1.2.3 ", "1.2.3/evil"] {
            assert_eq!(parse(invalid), None, "{invalid}");
        }
    }

    #[test]
    fn checksum_requires_one_exact_archive_name() {
        let hash = "a".repeat(64);
        let name = "wireguard-gui-1.9.1-x86_64-linux.tar.gz";
        let line = format!("{hash}  {name}\n");
        assert_eq!(tarball_checksum(&line, name).unwrap(), line);
        assert!(tarball_checksum(&format!("{hash}  prefix-{name}\n"), name).is_err());
        assert!(tarball_checksum(&format!("{line}{line}"), name).is_err());
        assert!(tarball_checksum(&format!("invalid  {name}\n"), name).is_err());
        assert!(tarball_checksum("", name).is_err());
    }

    #[test]
    fn private_update_directories_are_exclusive_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let first = private_workdir().unwrap();
        let second = private_workdir().unwrap();
        assert_ne!(first, second);
        assert_eq!(
            std::fs::metadata(&first).unwrap().permissions().mode() & 0o777,
            0o700
        );
        std::fs::remove_dir(first).unwrap();
        std::fs::remove_dir(second).unwrap();
    }

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
