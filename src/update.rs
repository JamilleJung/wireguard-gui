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
//! Before anything is downloaded the updater works out how this copy was
//! installed. Only copies installed by `install.sh` (recorded in its
//! `install-manifest`) are updated in place, with the same prefix and
//! authorization mode; copies owned by a distro package, `cargo install` or an
//! unknown method are left to that method.
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
    setup: InstallerSetup,
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
    // Refuse before any download when another method owns this copy.
    let setup = detect_installation()?;
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
        setup,
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
/// (no compile). The prefix and authorization mode recorded at install time
/// are passed on explicitly, so the update keeps them.
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
            .args(installer_args(&update.setup))
            .arg("--allow-root")
            .env("WG_USE_PREBUILT", "1")
            .env("PREFIX", &update.setup.prefix)
            .env_remove("WG_PREBUILT_DIR")
            .env_remove("WG_INSTALL_INJECT_FAILURE")
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
            .arg(format!("PREFIX={}", update.setup.prefix.display()))
            .arg("bash")
            .arg(&installer_abs)
            .args(installer_args(&update.setup))
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
/// `getpwuid_r(getuid())`; fail if the account cannot be resolved.
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
        // Never refresh pacman's databases without a full upgrade (-Sy):
        // that risks a partial upgrade. Install against the synced databases.
        &["pacman", "-S", "--needed", "--noconfirm", "minisign"],
        &["zypper", "--non-interactive", "install", "minisign"],
        &["apk", "add", "--no-cache", "minisign"],
        &["xbps-install", "-Sy", "minisign"],
        &["eopkg", "install", "-y", "minisign"],
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

// ---------------------------------------------------------------------------
// INSTALLATION METHOD
// ---------------------------------------------------------------------------

/// The installer's record, next to the helper: `<prefix>/lib/<app>/install-manifest`.
const MANIFEST_NAME: &str = "install-manifest";
/// Executable name, used for the duplicate-copy check.
const APP_BIN: &str = "wireguard-gui";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthMode {
    Sudoers,
    Polkit,
    /// A root-only install without a passwordless helper grant.
    None,
}

/// An installer-managed copy and the choices an update must keep.
#[derive(Debug, Clone, PartialEq, Eq)]
struct InstallerSetup {
    prefix: PathBuf,
    /// `None` for an install made before the manifest existed: install.sh
    /// then keeps whatever grant it finds on disk.
    auth: Option<AuthMode>,
    /// The first install was forced next to a distro package copy.
    force: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Manifest {
    Installer(InstallerSetup),
    /// Shipped by the distro recipes so the updater never touches their files.
    Package {
        manager: String,
    },
}

/// Parse the `key=value` manifest. Unknown keys are ignored so later
/// installers can add fields; anything the updater relies on is validated.
fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let mut fields = std::collections::HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("malformed install manifest line: {line}"))?;
        fields.entry(key.trim()).or_insert(value.trim());
    }
    let field = |key: &str| fields.get(key).copied().unwrap_or("");
    if field("format") != "1" {
        return Err("unsupported install manifest format".into());
    }
    if field("app") != REPO {
        return Err(format!("install manifest belongs to '{}'", field("app")));
    }
    let flag = |key: &str| match field(key) {
        "" | "0" => Ok(false),
        "1" => Ok(true),
        other => Err(format!("invalid install manifest {key}: {other}")),
    };
    match field("method") {
        "installer" => {
            let prefix = field("prefix");
            let safe = prefix.starts_with('/')
                && prefix.len() > 1
                && !prefix.ends_with('/')
                && prefix
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"/_.-".contains(&b))
                && prefix
                    .split('/')
                    .skip(1)
                    .all(|part| !part.is_empty() && part != "." && part != "..");
            if !safe {
                return Err(format!("invalid install manifest prefix: {prefix}"));
            }
            let auth = match field("auth") {
                "sudoers" => AuthMode::Sudoers,
                "polkit" => AuthMode::Polkit,
                "none" => AuthMode::None,
                other => return Err(format!("invalid install manifest auth mode: {other}")),
            };
            Ok(Manifest::Installer(InstallerSetup {
                prefix: PathBuf::from(prefix),
                auth: Some(auth),
                force: flag("force")?,
            }))
        }
        "package" => Ok(Manifest::Package {
            manager: field("manager").to_string(),
        }),
        other => Err(format!("unknown installation method '{other}'")),
    }
}

/// Read the manifest only if root wrote it: it decides which privileged
/// grant the installer re-creates.
fn read_manifest(path: &Path) -> Result<Option<Manifest>, String> {
    use std::os::unix::fs::MetadataExt;
    let md = match std::fs::symlink_metadata(path) {
        Ok(md) => md,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("install manifest {}: {e}", path.display())),
    };
    if !md.file_type().is_file()
        || md.uid() != 0
        || md.mode() & 0o022 != 0
        || md.nlink() != 1
        || md.len() > 16 * 1024
    {
        return Err(format!(
            "install manifest {} is not a root-owned, protected file - refusing to update",
            path.display()
        ));
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("install manifest {}: {e}", path.display()))?;
    parse_manifest(&text).map(Some)
}

/// `<prefix>` for an executable at `<prefix>/bin/<name>`.
fn install_prefix(exe: &Path) -> Option<PathBuf> {
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    bin.parent().map(Path::to_path_buf)
}

/// Decide how the running copy was installed; `Err` explains which other
/// method owns it and how to update through that method instead.
fn detect_installation() -> Result<InstallerSetup, String> {
    let exe = std::env::current_exe().map_err(|e| format!("locate the running program: {e}"))?;
    let exe = std::fs::canonicalize(&exe).unwrap_or(exe);
    if let Some((manager, package)) = package_owner(&exe) {
        return Err(package_refusal(manager, &package));
    }
    let prefix = install_prefix(&exe).ok_or_else(unknown_installation)?;
    let setup = match read_manifest(&prefix.join("lib").join(REPO).join(MANIFEST_NAME))? {
        Some(Manifest::Package { manager }) => return Err(package_refusal(&manager, REPO)),
        Some(Manifest::Installer(setup)) => {
            if setup.prefix != prefix {
                return Err(format!(
                    "the install manifest describes {} but this copy runs from {}; \
                     reinstall with install.sh",
                    setup.prefix.display(),
                    prefix.display()
                ));
            }
            setup
        }
        None => legacy_installation(prefix)?,
    };
    if let Some(copy) = duplicate_copy(&setup, |path| path.exists()) {
        return Err(format!(
            "another copy of {REPO} is installed at {}; an update here would keep both. \
             Remove one of them (or reinstall with install.sh --force to keep both)",
            copy.display()
        ));
    }
    Ok(setup)
}

/// A copy installed by an installer that predates the manifest: a root-owned
/// helper next to the executable. `cargo install` roots are not ours.
fn legacy_installation(prefix: PathBuf) -> Result<InstallerSetup, String> {
    use std::os::unix::fs::MetadataExt;
    if prefix.join(".crates.toml").exists() || prefix.join(".crates2.json").exists() {
        return Err(format!(
            "{REPO} was installed with `cargo install`; update it the same way: \
             cargo install --locked --force --git https://github.com/{OWNER}/{REPO} \
             (then re-run install.sh from that release for the privileged helper)"
        ));
    }
    let helper = prefix.join("lib").join(REPO).join("wg-helper");
    match std::fs::symlink_metadata(&helper) {
        Ok(md) if md.file_type().is_file() && md.uid() == 0 => Ok(InstallerSetup {
            prefix,
            auth: None,
            force: false,
        }),
        _ => Err(unknown_installation()),
    }
}

fn unknown_installation() -> String {
    format!(
        "cannot tell how this copy of {REPO} was installed (no install manifest and \
         no package owns it); update it the way you installed it, or install the \
         signed release with its install.sh"
    )
}

/// A distro-package copy next to an installer copy elsewhere, unless the
/// user deliberately forced both at install time.
fn duplicate_copy(setup: &InstallerSetup, exists: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    if setup.force || setup.prefix == Path::new("/usr") {
        return None;
    }
    [
        PathBuf::from("/usr/bin").join(APP_BIN),
        PathBuf::from("/usr/lib").join(REPO).join("wg-helper"),
    ]
    .into_iter()
    .find(|path| exists(path))
}

/// Installer flags that reproduce the recorded setup.
fn installer_args(setup: &InstallerSetup) -> Vec<&'static str> {
    let mut args = vec!["--update"];
    match setup.auth {
        Some(AuthMode::Sudoers) => args.push("--sudoers"),
        Some(AuthMode::Polkit) => args.push("--polkit"),
        Some(AuthMode::None) | None => {}
    }
    if setup.force {
        args.push("--force");
    }
    args
}

/// Ask each available package database who owns `path`.
fn package_owner(path: &Path) -> Option<(&'static str, String)> {
    let queries: [(&str, &str, &[&str]); 5] = [
        ("dpkg", "dpkg-query", &["-S"]),
        ("rpm", "rpm", &["-qf", "--queryformat", "%{NAME}\n"]),
        ("pacman", "pacman", &["-Qqo"]),
        ("apk", "apk", &["info", "--who-owns"]),
        ("xbps", "xbps-query", &["-o"]),
    ];
    queries.into_iter().find_map(|(manager, program, args)| {
        if !which(program) {
            return None;
        }
        let out = Command::new(program)
            .args(args)
            .arg(path)
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        parse_owner(manager, &String::from_utf8_lossy(&out.stdout)).map(|pkg| (manager, pkg))
    })
}

/// Extract the owning package from a package database query's output.
fn parse_owner(manager: &str, out: &str) -> Option<String> {
    fn first(text: &str) -> Option<&str> {
        text.lines().map(str::trim).find(|line| !line.is_empty())
    }
    let owner = match manager {
        "dpkg" => out
            .lines()
            .filter(|line| !line.starts_with("diversion"))
            .find_map(|line| line.split_once(": "))
            .and_then(|(packages, _)| packages.split(", ").next())
            .map(|package| package.split(':').next().unwrap_or(package)),
        "apk" => out
            .lines()
            .find_map(|line| line.split_once(" is owned by "))
            .map(|(_, package)| package),
        "xbps" => first(out)
            .and_then(|line| line.split_once(':'))
            .map(|(package, _)| package),
        _ => first(out),
    }?
    .trim();
    let valid = !owner.is_empty()
        && owner.len() <= 128
        && owner
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"+._@-".contains(&b));
    valid.then(|| owner.to_string())
}

/// `name-1.2.3-r0` (apk) / `name-1.2.3_1` (xbps) -> `name`.
fn strip_package_version(manager: &str, package: &str) -> String {
    let mut parts: Vec<&str> = package.rsplitn(3, '-').collect();
    parts.reverse();
    match manager {
        "apk" if parts.len() == 3 && parts[2].starts_with('r') => parts[0].to_string(),
        "xbps" => match package.rsplit_once('-') {
            Some((name, version)) if version.contains('_') => name.to_string(),
            _ => package.to_string(),
        },
        _ => package.to_string(),
    }
}

/// The refusal for a package-managed copy, naming the command that updates it.
fn package_refusal(manager: &str, package: &str) -> String {
    let name = strip_package_version(manager, package);
    let command = match manager {
        "dpkg" => format!("sudo apt update && sudo apt install --only-upgrade {name}"),
        "rpm" if which("zypper") => format!("sudo zypper update {name}"),
        "rpm" => format!("sudo dnf upgrade {name}"),
        "pacman" if pacman_foreign(&name) => format!(
            "sudo pacman -Syu, then rebuild the AUR package {name} (for example with your AUR helper)"
        ),
        "pacman" => "sudo pacman -Syu".to_string(),
        "apk" => format!("sudo apk upgrade {name}"),
        "xbps" => format!("sudo xbps-install -Su {name}"),
        _ => "your distribution's package manager".to_string(),
    };
    format!(
        "{REPO} is installed from the distribution package '{name}', so the package \
         manager owns its files and the in-app updater will not replace them. \
         Update it with: {command}"
    )
}

/// Whether pacman lists `name` as a foreign (AUR / locally built) package.
fn pacman_foreign(name: &str) -> bool {
    Command::new("pacman")
        .args(["-Qqm", name])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
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
            setup: InstallerSetup {
                prefix: "/usr/local".into(),
                auth: None,
                force: false,
            },
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

    use std::path::{Path, PathBuf};

    fn manifest(body: &str) -> String {
        format!("# comment\nformat=1\napp={REPO}\n{body}")
    }

    #[test]
    fn update_manifest_records_method_prefix_and_auth() {
        let parsed = parse_manifest(&manifest(
            "method=installer\norigin=release\nprefix=/usr/local\nauth=polkit\n\
             auth_user=alice\ndesktop=0\nforce=0\nversion=1.9.3\nfuture_key=x\n",
        ))
        .unwrap();
        let Manifest::Installer(setup) = parsed else {
            panic!("installer manifest parsed as {parsed:?}");
        };
        assert_eq!(setup.prefix, PathBuf::from("/usr/local"));
        assert_eq!(setup.auth, Some(AuthMode::Polkit));
        assert!(!setup.force);
        assert_eq!(installer_args(&setup), ["--update", "--polkit"]);
        let package = parse_manifest(&manifest("method=package\nmanager=pacman\n")).unwrap();
        assert_eq!(
            package,
            Manifest::Package {
                manager: "pacman".into()
            }
        );
    }

    #[test]
    fn update_preserves_auth_mode_and_force() {
        let mut setup = InstallerSetup {
            prefix: "/opt/wg".into(),
            auth: Some(AuthMode::Sudoers),
            force: true,
        };
        assert_eq!(installer_args(&setup), ["--update", "--sudoers", "--force"]);
        setup.auth = Some(AuthMode::None);
        setup.force = false;
        assert_eq!(installer_args(&setup), ["--update"]);
        setup.auth = None;
        assert_eq!(installer_args(&setup), ["--update"]);
    }

    #[test]
    fn update_manifest_rejects_unsafe_or_foreign_records() {
        for body in [
            "method=installer\nprefix=relative\nauth=sudoers\n",
            "method=installer\nprefix=/usr/../tmp\nauth=sudoers\n",
            "method=installer\nprefix=/usr/local/\nauth=sudoers\n",
            "method=installer\nprefix=/usr/lo cal\nauth=sudoers\n",
            "method=installer\nprefix=/usr/local\nauth=everyone\n",
            "method=curl-pipe\n",
            "garbage line\n",
        ] {
            assert!(parse_manifest(&manifest(body)).is_err(), "{body}");
        }
        assert!(parse_manifest("format=2\nmethod=installer\n").is_err());
        assert!(
            parse_manifest("format=1\napp=other\nmethod=installer\nprefix=/usr\nauth=none\n")
                .is_err()
        );
    }

    #[test]
    fn update_manifest_must_be_protected() {
        use std::os::unix::fs::PermissionsExt;
        let dir = private_workdir().unwrap();
        let path = dir.join("install-manifest");
        assert_eq!(read_manifest(&path), Ok(None));
        std::fs::write(
            &path,
            manifest("method=installer\nprefix=/usr/local\nauth=sudoers\n"),
        )
        .unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_manifest(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        // Only a root-written record may choose the privileged grant.
        assert_eq!(
            read_manifest(&path).is_ok(),
            unsafe { libc::geteuid() } == 0
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn update_detects_package_owners() {
        assert_eq!(
            parse_owner("dpkg", "wireguard-gui: /usr/bin/wireguard-gui\n").as_deref(),
            Some("wireguard-gui")
        );
        assert_eq!(
            parse_owner(
                "dpkg",
                "diversion by x from: /usr/bin/wireguard-gui\nwireguard-gui:amd64, other: /usr/bin/wireguard-gui\n"
            )
            .as_deref(),
            Some("wireguard-gui")
        );
        assert_eq!(
            parse_owner("rpm", "wireguard-gui\n").as_deref(),
            Some("wireguard-gui")
        );
        assert_eq!(
            parse_owner("pacman", "wireguard-gui\n").as_deref(),
            Some("wireguard-gui")
        );
        assert_eq!(
            parse_owner(
                "apk",
                "/usr/bin/wireguard-gui is owned by wireguard-gui-1.9.3-r0\n"
            )
            .as_deref(),
            Some("wireguard-gui-1.9.3-r0")
        );
        assert_eq!(
            parse_owner(
                "xbps",
                "wireguard-gui-1.9.3_1: /usr/bin/wireguard-gui (regular file)\n"
            )
            .as_deref(),
            Some("wireguard-gui-1.9.3_1")
        );
        assert_eq!(parse_owner("rpm", ""), None);
        assert_eq!(parse_owner("pacman", "bad name; rm -rf /\n"), None);
        assert_eq!(
            strip_package_version("apk", "wireguard-gui-1.9.3-r0"),
            "wireguard-gui"
        );
        assert_eq!(
            strip_package_version("xbps", "wireguard-gui-1.9.3_1"),
            "wireguard-gui"
        );
        let refusal = package_refusal("dpkg", "wireguard-gui");
        assert!(refusal.contains("apt install --only-upgrade wireguard-gui"));
        assert!(
            package_refusal("apk", "wireguard-gui-1.9.3-r0").contains("apk upgrade wireguard-gui")
        );
    }

    #[test]
    fn update_refuses_duplicate_package_copy() {
        let setup = InstallerSetup {
            prefix: "/usr/local".into(),
            auth: Some(AuthMode::Sudoers),
            force: false,
        };
        assert_eq!(duplicate_copy(&setup, |_| false), None);
        assert_eq!(
            duplicate_copy(&setup, |path| path == Path::new("/usr/bin/wireguard-gui")),
            Some(PathBuf::from("/usr/bin/wireguard-gui"))
        );
        let forced = InstallerSetup {
            force: true,
            ..setup.clone()
        };
        assert_eq!(duplicate_copy(&forced, |_| true), None);
        let usr = InstallerSetup {
            prefix: "/usr".into(),
            ..setup
        };
        assert_eq!(duplicate_copy(&usr, |_| true), None);
        assert_eq!(
            install_prefix(Path::new("/usr/local/bin/wireguard-gui")),
            Some(PathBuf::from("/usr/local"))
        );
        assert_eq!(
            install_prefix(Path::new("/src/target/release/wireguard-gui")),
            None
        );
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
