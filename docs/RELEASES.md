# 📦 Releases

Current published release: **[1.9.3 — 2026-11-02](https://github.com/JamilleJung/wireguard-gui/releases/tag/v1.9.3)**.
This page describes current release packaging and selected historical changes.
[CHANGELOG.md](../CHANGELOG.md) is the detailed version record. Documentation
changes on `main` do not rebuild or replace an already published release.

## Where to get releases

Use the project's [release list](https://github.com/JamilleJung/wireguard-gui/releases)
or [latest release](https://github.com/JamilleJung/wireguard-gui/releases/latest).
Release 1.9.3 uses the following artifact names; AppImage is optional:

| Artifact | Purpose |
|---|---|
| `wireguard-gui_1.9.3-1_amd64.deb` | Debian-family x86_64 application/helper, desktop assets and polkit rule |
| `wireguard-gui-1.9.3-x86_64-linux.tar.gz` | Native Linux x86_64 application/helper and installer assets |
| `wireguard-gui-1.9.3-aarch64-linux.tar.gz` | Native Linux ARM64 application/helper and installer assets |
| `wireguard-gui-1.9.3-x86_64.AppImage` | x86_64 AppImage; optional in the workflow, attached only when its optional build succeeds |
| `SHA256SUMS` | SHA-256 digests of binary/package artifacts |
| `SHA256SUMS.minisig` | minisign signature of the digest manifest |
| `minisign.pub` | Public signing key, also committed in the repository |

GitHub's generated source archives are separate from these binary bundles.
Tarballs retain adjacent executable binaries, `install.sh`, README/LICENSE and
`packaging/` desktop/icon/rule files. The installer recognizes a binary bundle
without Cargo.toml and skips Rust/compiler/development-header setup, while still
installing runtime dependencies and helper authorization.

Tarballs are native Linux builds and may require a different runtime/libc than
your host. Use source or a native package when incompatible. The `.deb` installs
under `/usr`; the source/bundle installer defaults to `/usr/local`. Packages
ship a group-restricted polkit rule but do not enroll your user. See
[installation](../README.md#install) and [packaging](../packaging/PACKAGING.md).

## Verify a download

Verify the manifest signature against the independently trusted project key,
then verify the **exact artifact** you downloaded against that signed manifest.
Use the commands in [README verification](../README.md#verify-a-download).
Do not treat a key downloaded beside the artifact as an independent trust anchor.
Signature/checksum failure means do not install that artifact.

From a checkout of this repository, `scripts/verify-release.sh` runs the same
verification the release workflow runs before publishing, against the
committed key (never the copy shipped beside the artifacts):

```sh
scripts/verify-release.sh --tag v1.9.3 --download             # fetch with gh, then verify
scripts/verify-release.sh --tag v1.9.3 ~/Downloads/wg-gui     # an existing download
```

It checks the signature and trusted comment, every checksum, that each
tarball holds exactly the expected files and modes, and (with `upx`
installed) that each binary is built for its architecture, uses the expected
glibc loader without RPATH/RUNPATH, that the privileged helper links only the
C runtime, and that the embedded package version equals the tag; with
`dpkg-deb` it also checks the `.deb` metadata. Releases published
before this check existed (1.9.3 and earlier) report
non-root tar ownership as a warning; `--strict` turns warnings into failures.

The workflow fails closed if signing is unavailable or signature verification
fails; it does not intentionally publish unsigned releases. AppImage creation
is optional, so another release may contain six assets rather than seven.

## Upgrading

Prefer the existing package manager for a package-managed installation. The
in-app updater downloads and verifies the matching x86_64/ARM64 tarball, then
runs its normal installer; it is not a `.deb`/AppImage package updater and can
create a `/usr/local` installation alongside a `/usr` package. It needs external
verification/escalation tools and a restart. See the [update tutorial](TUTORIAL.md#updates).

**Kill-switch upgrade step:** replacing binaries does not rewrite firewall rules
already loaded in the kernel. When upgrading from before 1.9.2, reapply an active
kill switch once (off, then on) to use exact comment matching and corrected ordering.
Plan for the brief interval without those rules. Restarting the app or merely
activating an interface does not reapply them.

## Relationship to the terminal sibling

[wireguard-tui](https://github.com/JamilleJung/wireguard-tui) is a separate
terminal application with a related file/helper model. Both currently publish
1.9.3, but their controls, CLI, supported artifacts and features differ. Read
each project's own changelog and instructions; a matching number is not a
promise of feature-identical behavior.

## How releases are built

Every pull request to `main` must pass the [CI workflow](../.github/workflows/ci.yml)
and the Dependabot-only compatibility job; the exact required checks and the
branch rules are in [REPO_SETTINGS.md](REPO_SETTINGS.md). CI runs rustfmt,
clippy, unit/property/fixture tests, a release build with CLI smoke checks,
the CLI contract test (`tests/release-regression.sh`) and a headless GUI
launch, ShellCheck of every script, installer sanity, the firewall
network-namespace test, the WireGuard + kill switch end-to-end test,
`cargo audit` with the shared [.cargo/audit.toml](../.cargo/audit.toml)
policy, version consistency (`scripts/check-version.sh`), the MSRV build and a
fuzz-crate build. Coverage (`cargo llvm-cov`, lcov artifact and job summary)
is informational. Scheduled workflows fuzz nightly, run mutation testing
weekly, and re-test the newest compatible dependencies weekly.

The [release workflow](../.github/workflows/release.yml) runs on a pushed
`vMAJOR.MINOR.PATCH` tag. Its publish job **depends on every gate below and
fails closed**: a failed, skipped or cancelled gate prevents publication, and
the publish job re-checks that all eleven gates reported success. Only the
AppImage is optional.

| Gate | What it checks |
|---|---|
| Version consistency | tag == Cargo.toml == Cargo.lock == top CHANGELOG release == PKGBUILD/RPM/APKBUILD/Void versions |
| rustfmt & clippy | formatting, `clippy -D warnings`, ShellCheck |
| Tests | fixtures, unit/property/fixture tests (20 000 property cases), installer sanity |
| Audit release dependencies | `cargo audit --deny warnings` with the documented exceptions |
| Firewall namespace test | `tests/firewall-namespace.sh` |
| Build aarch64 tarball | native ARM64 build with `cargo-auditable 0.7.7`, CLI contract, UPX, reproducible tarball (`scripts/stage-tarball.sh`), installer check of the tarball |
| Build x86_64 tarball, .deb & AppImage | native build, CLI contract before and after UPX, headless GUI launch, tarball, `.deb` (`cargo-deb 3.8.0`), optional AppImage from pristine copies |
| Smoke (QEMU) per architecture | each packed binary runs in a clean Ubuntu 24.04 userland (aarch64 under QEMU user-mode emulation): `--version` equals the tag, `--help`, `tests/helper-validation.sh`, `tests/release-regression.sh` |
| Kill switch e2e | `tests/killswitch-e2e.sh` with the shipped x86_64 helper |
| Checksums & signature | SHA256SUMS signed with `MINISIGN_KEY` in the `release` environment, verified against the committed key |
| Verify release artifacts | `scripts/verify-release.sh --strict --run`: signature, checksums, exact archive contents/modes/ownership, architecture, glibc loader and no RPATH, helper links only libc, embedded package version == tag, `.deb` fields and files, native CLI contract |

Publication uploads exactly the verified files after re-checking SHA256SUMS.
Third-party Actions are commit-pinned. The linuxdeploy binary used for
AppImage packaging is pinned and checksum-verified. Workflow tokens are
read-only except for the publishing job's content permission. Release tags
are protected against deletion and moves.

Before tagging, run the same release checks locally where possible:

```sh
scripts/check-version.sh --tag vX.Y.Z
bash tests/release-regression.sh --version X.Y.Z target/release/wireguard-gui target/release/wg-helper
```

Release binaries are built with embedded auditable dependency data. Tarball and
`.deb` executables are then UPX-packed; direct `cargo audit bin` cannot read the
packed ELF metadata. Decompress a copy first:

```sh
cp wireguard-gui wireguard-gui.audit
upx -d wireguard-gui.audit
cargo audit bin wireguard-gui.audit
```

The AppImage uses uncompressed copies internally; extract it before inspecting
its application/helper. Locked sources help repeat builds, but this is not a
promise of identical binary bytes across every toolchain or distribution.

## 1.9.3 behavior and upgrade checks

Release helper discovery uses this project's own standard `/usr/local/lib/`
and `/usr/lib/` paths, or an adjacent **trusted** executable. The executable must
be root-owned, executable and not group/other writable; its directory ancestry
must also be trusted. A normal user-owned extracted helper is not automatically
trusted: install the verified bundle first. Release builds no longer fall back
to the source build directory or another application's helper. Debug overrides
remain available for development; they do not create an authorization grant.

Background read-only requests use a noninteractive polkit check before pkexec,
so missing authorization produces an actionable error instead of repeated
password dialogs. Explicit mutating actions can still request authentication.
Helper IPC drains input/output concurrently, retains at most 4 MiB per output
stream, and has an overall 90-second deadline plus bounded pipe cleanup. Local
key-generation commands have a 10-second deadline. A privileged process that
the UI user cannot signal may finish independently after a UI-side timeout;
check actual tunnel state before retrying.

Before privileged writes or helper grants, `PREFIX` must be an absolute ASCII
path containing only letters, digits, `/`, `_`, `.` and `-`, with no empty,
`.` or `..` components or trailing slash. Existing directory ancestors must be
root-owned without group/other write permission; symlinks are refused. A
group-writable `/usr/local` can therefore fail these checks. An administrator
must choose or prepare a protected path; the installer does not take ownership
of caller directories. `--polkit` supports only `/usr` or `/usr/local`, matching
the bundled rule. Custom sudoers prefixes still require matching frontend helper
discovery configuration.

Installer options are validated; unknown options fail instead of being ignored.
Source builds use the lockfile. Switching sudoers/polkit modes installs and
validates the selected grant before removing the previous grant. Failed sudoers
validation or group enrollment reports failure and keeps prior authorization.
`--polkit` requires pkexec up front; it does not install a rule and then claim
success without the escalation tool. Packages still do not enroll users.

Repeating `--polkit` checks the existing `wireguard` group and account
membership before invoking group-management tools. An already configured
Alpine/BusyBox system can reinstall without optional Shadow `groupadd`/`usermod`.
Missing groups or membership must still be created and verified before the
previous authorization is removed.

Verified updates retain ownership of their private download directory through
installation and clean it up on download, staging or installation failure and
normal completion. Before privilege escalation, the whole required bundle is
checked: regular single-link application/helper/installer/assets, no symlink
components, and a fresh exclusive staging tree. Missing/unsafe assets fail before
installation starts. HTTPS-only redirects are enforced. GUI automatic updates
support its published x86_64/ARM64 builds; unsupported CPUs fail explicitly.

Download refuses a version that is no longer newer and reports the actual
verified version on success. Install the app/helper together, reapply existing
kill-switch rules deliberately, and review the current backup/QR limits before
using an archive created by another tool. Tests cover concrete failure paths;
passing them does not prove compatibility with every host firewall or distro.

## Version history

### 1.9.3 (2026-11-02)

Fix repeat `--polkit` installation on Alpine/BusyBox by reusing existing group
and membership state without requiring optional Shadow utilities. Missing setup
still has to succeed before previous authorization is removed.

### 1.9.2 (2026-11-02)

Deep audit fixes shared config/schema validation, safe complete exports and
bounded backup/QR handling, trusted helper selection and bounded IPC, helper
filesystem/command safety and serialized firewall transactions. The GUI keeps
heavy operations off its event loop and prevents conflicting actions; the TUI
fixes stale selection/log snapshots and editor/clipboard lifetimes. Verified
updates own and preflight the full bundle. See the current changelog and
security policy for exact limits and upgrade notes.

### 1.9.1 (2026-11-02)

Fixes kill-switch comment isolation/rule ordering, helper output deadlocks and
child timeouts, inline comments, backup preflight and size bounds, and private
signed-update staging. Restoring a backup validates all selected configs before
sequential saves, not as an all-or-nothing transaction. Binary bundles preserve
installer assets and support direct prebuilt installation without a compiler.
The previously unreleased 1.9.0 resolver-shim fix is included. See
[the full changelog](../CHANGELOG.md).

The following entries retain their historical wording and interface descriptions;
use the current README/tutorial for today's controls.

### 1.6.0 (2026-06-17)

- **nftables kill switch backend** - preferred when `nft` is available (`inet filter` table covers both IPv4+IPv6); iptables/ip6tables fallback.
- **SSH safety warning** - the helper warns (stderr) before enabling a kill switch over an SSH connection.
- **Copy UX** - click the value text or trailing ⧉ icon to copy; no more large "Copy" button.
- **Log tab** - `TextEdit` (read-only) for native text selection and scrolling; journal limits raised to 1000 lines.
- **Speed display** - uses ↓ ↑ icons instead of "down"/"up"; throughput calculation is more robust (no longer gated on active flag).
- **More tests** - kill switch rule generation tests: nftables handle extraction and iptables rule numbering.

## 1.6.3 (2026-06-18) - Clippy clean, QR fix, docs sync

- Zero clippy warnings across all targets (`-D warnings` enforced).
- Terminal QR simplified to half-block only (auto-detect caused false "too large").
- All markdown documentation audited and synchronized.

## 1.6.2 (2026-06-18) - SSH allowlist, 92 tests, copy/log UX

- **SSH auto-allowlist.** Kill switch now auto-allows established SSH traffic when `$SSH_CONNECTION` is set.
- **Copy UX.** Click the value text or ⧉ icon to copy - no more large "Copy" button.
- **Log tab.** 1000 journal lines, `TextEdit` read-only for text selection and scrolling.
- **Speed fix.** Reliable active detection via `wg show interfaces`; ↓ ↑ icons.
- **92 tests** across both repos (helper + main).

## 1.5.5 - Rust helper, kill switch, multi-peer editor, and aarch64 tarballs

**Highlights**

- The privileged runtime helper is now Rust (`src/bin/wg-helper.rs`) with the
  same fixed verb contract.
- Advanced mode can toggle a tunnel-scoped kill switch for active tunnels using
  helper-managed iptables/ip6tables rules.
- The structured editor now supports multiple peers for common Interface/Peer
  fields while keeping hooks, routing directives, and unknown keys in raw text.
- Easy Mode can now create a tunnel from scratch with Interface-only,
  full-tunnel, and split-tunnel presets instead of forcing users into Advanced
  Mode for first-run setup.
- Copy buttons now normalize single-field payloads separately from raw
  config/log copying, fixing accidental whitespace/newline copy artifacts.
- Release automation now builds portable x86_64 and aarch64 Linux tarballs.
- Alpine APKBUILD and Void template packaging starts are included for
  maintainers.

Newest first. Each entry lists the theme, what you actually get ("Highlights"),
and any upgrade notes worth knowing.

### 1.5.4 - Installer finds `sbin` tools after the root re-exec

**Highlights**

- Fixes a real-world install failure: a normal user's `PATH` (which `su` carries
  into the as-root re-exec) usually omits `/usr/sbin` and `/sbin`, so `visudo`
  and the `resolvconf` probe silently failed. The result was a skipped
  passwordless drop-in ("sudoers validation failed") and a present `openresolv`
  being misreported as missing. The installer now puts sbin on `PATH`, so the
  sudoers drop-in is written and resolvconf is detected correctly.
- Clearer guidance when the drop-in is skipped: it now tells you to re-run
  `./install.sh --polkit`.

**Upgrade notes:** if a previous install left you without passwordless privilege
(the app kept prompting via `pkexec`) or wrongly told you a resolvconf provider
was missing, re-run `./install.sh` with this version.

### 1.5.3 - Install even when there is no usable sudo

**Highlights**

- "No usable sudo -> install sudo and set it up." On a box where you cannot use
  sudo (for example a Debian machine where your login user is not in the sudoers
  file), the installer re-runs itself as root with a **single** ROOT-password
  prompt (instead of one prompt per step), installs `sudo` if it is missing, and
  writes a passwordless drop-in scoped to the helper for *your* user. The app
  then works as a normal user - no root, no prompt - even though you are not in
  the `sudo` group.
- The build no longer runs as root: `cargo` / `rustup` run as the invoking user
  via `runuser`, so dependency build scripts never execute with root privilege
  and the toolchain and artifacts stay in that user's home.
- Clear, actionable errors: if the app cannot gain root (no passwordless sudo and
  no `pkexec`) you get a readable message instead of a cryptic
  `spawn failed: No such file or directory`, and the installer fails loudly if it
  cannot set up any privilege path rather than reporting success and leaving a
  broken install.

**Security:** the sudoers rule no longer trusts `$USER`. The target user is taken
from the real uid (`id -un`), validated against a strict username pattern and
confirmed to exist, so a crafted `$USER` cannot inject a wider sudoers spec (such
as `NOPASSWD: ALL`) - something `visudo -cf` alone does not catch.

### 1.5.2 - Install works when sudo is present but you are not a sudoer

**Highlights**

- Fixes an abort on Debian-style servers where the `sudo` binary exists but your
  login user is not in the sudoers file (`<user> is not in the sudoers file`).
  The installer now probes real sudo usability (`sudo -n` / admin-group
  membership) instead of assuming the binary means you can use it, falls back to
  `su` (the ROOT password), and auto-switches the helper authorization to a
  polkit rule.

**Upgrade notes:** if install previously aborted in this situation, this version
should complete. Tip: run `su -` first, then `./install.sh`, so you are prompted
once rather than per step.

### 1.5.1 - DNS / resolvconf check and a per-distro guide

**Highlights**

- The first-run Setup window now reports whether a **resolvconf provider** is
  available for tunnels that use a `DNS =` line, and **Fix automatically**
  installs one (`openresolv`; systemd-resolved also counts). This is the fix for
  minimal Debian, where such tunnels failed with `resolvconf: command not found`.
- Activation failures are now explained in plain language (for example, the
  missing resolvconf provider) instead of the raw `wg-quick` output.
- New **per-distro guide** ([docs/DISTROS.md](DISTROS.md)): what to install, what
  to set up, what survives a reboot, and when - only as a server or gateway - you
  need firewall and IP-forwarding changes.

**Upgrade notes:** if your tunnel uses `DNS =` and it failed to come up before,
either re-run `./install.sh` or use the Setup window's **Fix automatically** to
install a resolvconf provider once. The fix persists - it is a normal package.

### 1.5.0 - First-run Setup wizard and a friendly empty state

**Highlights**

- A **first-run Setup wizard**. On launch the app runs a read-only system check
  (WireGuard tools, the privileged helper and its authorization, `/etc/wireguard`,
  systemd, journald). If everything critical is OK it goes straight to Easy mode;
  if not, a friendly window explains what is missing in plain language, with
  **Fix automatically** (installs `wireguard-tools` via your package manager
  through pkexec, and creates `/etc/wireguard`, with confirmation), **Show
  commands**, **Re-check** and **Skip for now**.
- The wizard is deliberately conservative: it never connects tunnels, never
  enables start-on-boot, and never touches existing configs.
- A **beginner-friendly empty state**: with no tunnels, the app shows "No tunnels
  yet" plus **Import .conf / Import QR image** buttons (and **New tunnel** in
  Advanced mode) instead of a blank pane.

**Note:** the app still does not bundle WireGuard kernel modules or `wg`/`wg-quick`
- it uses your system's `wireguard-tools`, and helps you install them.

### 1.4.1 - Distro packaging

**Highlights**

- An **AUR `PKGBUILD`** (Arch) and an **RPM spec for COPR** (Fedora / RHEL /
  Rocky), plus `packaging/PACKAGING.md`, so the app can be packaged the native
  way for those distros.
- A Flatpak manifest is included but documented as **experimental** - the sandbox
  is a poor fit for a privileged system VPN manager.
- Documentation switched to plain ASCII hyphens.

### 1.4.0 - Live throughput, tray speed, and CLI hygiene

**Highlights**

- **Live throughput and connection health** in the Interface card: real-time
  down/up speed and a handshake-based health line (OK / stale / waiting).
- The **system-tray tooltip** now shows the active tunnels *and* live throughput,
  so you can read your VPN state without opening the window.
- **`wireguard-gui --version` and `--help`** print and exit without opening a
  window - friendlier for scripts and packaging.
- The privileged helper now **bounds every `wg` / `wg-quick` call with a
  timeout**, so a hang (DNS, a stuck `PostUp`, a wedged interface) cannot lock up
  the app.

**Upgrade notes:** demo mode (`WGGUI_DEMO`) was **removed** - the app always
talks to real tunnels now. If you relied on the demo for screenshots or testing,
it is no longer available.

### 1.3.5 - Easy/Advanced toggle moved to the bottom action bar

**Highlights**

- The toolbar **Remove (delete) button** is no longer crowded off the bar: the
  Easy/Advanced toggle moved out of the top toolbar to the **bottom action bar,
  next to Edit**, where it is always reachable - even with no tunnel selected.

### 1.3.4 - Easy mode (default) for everyday users

**Highlights**

- A new **Easy mode**, on by default, hides expert tools (Export, Running cfg,
  Save live, *Add empty tunnel*) and leaves the everyday surface: Add
  (import / QR), Activate / Deactivate, Edit, Remove, Show QR, Start on boot.
- One click on **Advanced mode** reveals everything, and your choice is
  remembered (`~/.config/wireguard-gui/mode`).

### 1.3.3 - Exact config-key matching and tarball-friendly helper discovery

**Highlights**

- The form editor now matches config keys **exactly**: a directive like
  `PrivateKeyFile` or `EndpointBackup` is no longer mistaken (by prefix) for
  `PrivateKey` / `Endpoint`, which could otherwise let the form open for - and
  then rewrite - a config it cannot actually represent. Covered by a regression
  test.
- The privileged helper is also discovered **next to the binary**, so an
  extracted release tarball or AppImage works without first running `install.sh`.
- CI now **hard-fails on `shellcheck` warnings** for `wg-helper` and `install.sh`.

### 1.3.2 - IPv6-endpoint fix and helper portability

**Highlights**

- A bracketed-IPv6 `Endpoint` (for example `[2001:db8::1]:51820`) is accepted by
  config validation again; the stricter check added in 1.3.1 wrongly rejected it,
  which blocked saving and importing IPv6-endpoint tunnels. Covered by a
  regression test.
- Helper portability: `wg-helper` no longer relies on GNU `find -printf` (it uses
  a pure-bash glob, so it works with BusyBox `find` on Alpine / Void) and filters
  listed tunnels to valid names. Start-on-boot detects `systemctl` first and
  fails with a clear message on non-systemd systems; the log view explains when
  `journalctl` is not available.
- Helper-path override hardening: `$WG_HELPER` is honoured freely in debug builds,
  but in release builds it is ignored unless `WG_ALLOW_UNSAFE_HELPER=1` is set
  *and* the target is an absolute, root-owned, non-world-writable file.
- Added unit tests for config parsing, validation, name sanitisation and the form
  representability check, plus the CI shell-syntax check; the README cross-links
  the terminal sibling and documents the `wg-quick`-not-NetworkManager model.

### 1.3.1 - Form editor stops dropping config it cannot represent

**Highlights**

- The form editor no longer silently strips config it cannot represent. Editing a
  tunnel with a second `[Peer]`, `PostUp`/`PreUp`/`PostDown`/`PreDown`, `Table`,
  or other unmapped keys used to drop them on Save; the form now refuses to open
  for such configs (keeping them in raw-text mode with a notice) and never
  overwrites a config it cannot faithfully round-trip.
- Closing the window no longer strands the app when there is no system-tray host
  (for example GNOME without the AppIndicator extension): with no tray to restore
  from, closing now quits instead of hiding into nothing.
- The live-status timer no longer overwrites the detail pane of a tunnel you just
  selected with a stale background reading.
- Bulk file import now validates each file and flags ones that run root scripts,
  matching the single-import path.
- Stricter config validation (real IP/CIDR parsing, valid endpoint host) and a
  release workflow that fails on a missing `.deb` or checksums rather than
  publishing a partial release.

### 1.3.0 - Structured form editor and an earlier dependency check

**Highlights**

- A **structured editor**: the tunnel editor toggles between a **Config text**
  view (the raw `.conf`) and an **Edit fields** form with labelled Interface
  (Private key, Address, DNS, Listen port, MTU) and Peer (Public key, Preshared
  key, Allowed IPs, Endpoint, Persistent keepalive) fields. New tunnels open in
  the form; existing ones open in text view; edits in either view stay in sync,
  and **Generate keypair/PSK** updates both.
- **Closing the window now minimizes to the tray** instead of quitting, matching
  the WireGuard for Windows client - the app keeps running; use the tray's
  **Show WireGuard** to reopen or **Quit** to exit.
- A minimal-install dependency check in `install.sh` verifies the C toolchain,
  `pkg-config`, and the `fontconfig` / `xkbcommon` / `dbus-1` dev headers, and
  checks `wireguard-tools` at runtime, failing early with a clear message instead
  of a cryptic build error.
- The main window opens larger (1000x720) for a roomier list and details.

### 1.2.0 - Live apply, running config, and save-live

**Highlights**

- **Generate preshared key** (`wg genpsk`) in the editor, pairing with the
  existing keypair generation.
- **Apply edits to a running tunnel without dropping peer sessions**: saving an
  active tunnel uses `wg syncconf`; wg-quick-only fields (Address / DNS / MTU /
  Table) still prompt to reconnect.
- **Running cfg**: copy a live tunnel's running config (`wg showconf`).
- **Save live**: write the running state back to the `.conf` (`wg-quick save`).

### 1.1.1 - Richer tray menu

**Highlights**

- The tray menu gained a connection-status header, a hover tooltip listing active
  tunnels, a **Tunnels** submenu, and a **Deactivate all** action, refreshing
  live.
- Fixed the Edit button's tooltip overflowing the right edge of the window.

### 1.1.0 - Keypairs, QR codes, export, and the tray icon

**Highlights**

- **Keypair generation**: new tunnels open with a freshly generated private key
  and a live "Public key" field (like the WireGuard for Windows dialog), with a
  "Generate keypair" button to regenerate on demand.
- **QR codes**: *Show QR* renders a tunnel as a QR code to scan into the mobile
  app, and *Add Tunnel -> Import from QR code...* imports from a QR image.
- **Export** all tunnels to a `.zip`, and **Copy** buttons for public keys, the
  full config, and the log.
- **Start on boot** toggle (enables/disables the `wg-quick@` systemd unit).
- **System-tray icon** with per-tunnel activate/deactivate, Show, and Quit
  (StatusNotifierItem; needs KDE or GNOME's AppIndicator extension).
- Tooltips on the toolbar/detail buttons and an **About** window; live status now
  polls on a background thread so the UI no longer stutters.

### 1.0.0 - First stable release

**Highlights**

- A **Log tab** (Tunnels / Log) showing recent activity - the app's audit log
  plus `wg-quick` service entries - with a Refresh button.
- Layout matched to the WireGuard for Windows client: the Activate/Deactivate
  button sits below the interface details and **Edit** sits at the bottom-right.
- The headline fix: **text inputs no longer render blank on a light background.**
  In a dark OS color scheme the std-widgets used near-white field fills that
  vanished on white windows (visible only when focused); the app now forces the
  light palette (`Palette.color-scheme = light`, built with the `fluent-light`
  style) so the editor and inputs render correctly.

**Upgrade notes:** if an earlier build showed empty-looking edit fields on a
white window, 1.0.0 is the version that fixes it.

---

## 📜 Pre-1.0 history (for completeness)

- **0.2.0** - Added README screenshots, a now-removed demo mode (`WGGUI_DEMO=1`),
  project docs (`CONTRIBUTING.md`, `SECURITY.md`, the changelog, issue/PR
  templates), and the first **signed releases**: `SHA256SUMS` signed with minisign
  (`SHA256SUMS.minisig`), the public key (`minisign.pub`) shipped in the repo and
  each release. The editor began warning (amber) about `PostUp`/`PreUp`/
  `PostDown`/`PreDown`, and `wg-helper` was hardened to export a fixed `PATH`.
- **0.1.0** - First release: a native Linux GUI for managing WireGuard tunnels,
  modelled on the WireGuard for Windows client (Rust + Slint). Tunnel list with a
  live indicator, Interface and Peer cards with live handshake/transfer,
  Activate/Deactivate via `wg-quick`, import one or many `.conf` files, an inline
  editor with config validation, rename/remove, the hardened `wg-helper`, the
  sudoers/polkit/pkexec privilege backends, the universal `install.sh`, and the
  GitHub Actions release pipeline (`.deb`, AppImage, tarball, `SHA256SUMS`).

---

## 📜 Versioning policy

Versions use `MAJOR.MINOR.PATCH` and release tags use `v<version>`.
The changelog describes the behavior included in each release. GUI/TUI releases
may coordinate version numbers, but are maintained as separate applications
with different interfaces, artifact coverage and feature sets.
