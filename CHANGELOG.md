# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.6.2] - 2026-06-18

### Added
- SSH auto-allowlist: kill switch auto-allows established SSH return traffic
  when `$SSH_CONNECTION` is detected.
- Terminal QR cell-aspect-ratio auto-detection (square vs 2:1 cells).
- TUI `+` key in Advanced mode to quick-add a `[Peer]` section via `$EDITOR`.
- Validation tests: tunnel name edge cases, max length, import sanitization.
- More helper tests: killswitch rule structure, iptables comment safety,
  SSH port parsing, FwMark detection, config validation edges.

### Changed
- GUI: `DetailRow` copy area uses clickable text + ⧉ icon instead of large
  "Copy" button; fixed text centering regression.
- GUI: `StatusDot` component extracted to `ui/components/statusdot.slint`.
- TUI: `popup_area` helper moved to `src/ui/helpers.rs`.
- Log limits increased to 1000 lines (was 300/200).

### Fixed
- Speed/throughput could get stuck at 0 when active detection from dump failed.
- `DetailRow` text centering caused by `TouchArea` default alignment.


## [1.6.0] - 2026-06-17

### Added
- nftables kill switch backend (preferred when `nft` is available; iptables/ip6tables fallback).
- SSH safety warning when enabling kill switch over an SSH connection.
- Kill switch rule-generation tests (nftables handle extraction, iptables rule numbering).

### Changed
- Speed display uses ↓ ↑ icons instead of "down"/"up" text.
- Log tab fetches up to 1000 journal lines (was 300/200).
- Log tab uses TextEdit read-only for native text selection and scrolling.
- Copy buttons replaced with clickable value text + trailing ⧉ icon.
- Throughput polling no longer gates on active flag (more reliable on first sample).

### Fixed
- Speed/throughput could get stuck at 0 when active detection from dump failed.
- Tunnel active detection now uses `wg show interfaces` list (reliable), fallback to dump.

## [1.5.5] - 2026-06-17

### Added
- Rust `wg-helper` binary with the same fixed verb contract as the previous
  helper.
- Advanced-mode kill switch toggle backed by helper-managed iptables/ip6tables
  rules for active tunnels.
- Multi-peer structured editor support for common Interface/Peer fields.
- aarch64 Linux release tarballs and Alpine/Void packaging templates.
- Easy Mode can create a tunnel from scratch with Interface-only, full-tunnel,
  and split-tunnel presets.

### Changed
- Reframed project docs and package metadata around the small native Linux
  WireGuard client model: plain `/etc/wireguard` configs, `wg`/`wg-quick`, no
  NetworkManager layer, and no mandatory runtime core.
- Copy buttons now use stable trailing actions and separate normalized
  single-field payloads from raw config/log copy payloads.

### Security
- The helper now performs a second, privileged-boundary config shape check before
  save/rename, in addition to frontend validation.
- Helper writes now make a best-effort `sync -f` before atomic rename.
- Runtime helper command execution moved out of shell into Rust argv-based
  process calls with fixed tool lookup and timeouts.
- CI now runs negative helper-name validation tests.
- CI now runs installer sanity checks for helper paths, sudoers validation, and
  non-root build handoff.

### Fixed
- Copying public keys, addresses, DNS, endpoints, and allowed IPs no longer
  carries accidental leading/trailing whitespace or display newlines.

## [1.5.4] - 2026-06-17

### Fixed
- **Installer now finds `sbin` tools after the root re-exec.** A normal user's
  `PATH` (which `su` carries into the as-root re-exec) usually omits `/usr/sbin`
  and `/sbin`, so `visudo` and the `resolvconf` probe silently failed - the
  passwordless drop-in was skipped ("sudoers validation failed") and a present
  `openresolv` was misreported as missing. The installer now puts sbin on `PATH`,
  so the sudoers drop-in is written and resolvconf is detected correctly.
- Clearer hint when the drop-in is skipped: re-run `./install.sh --polkit`.

## [1.5.3] - 2026-06-17

### Added
- **"No usable sudo -> install sudo and set it up."** When you can't use sudo
  (e.g. a Debian box where you aren't in the sudoers file), the installer re-runs
  itself as root with a **single** ROOT-password prompt (instead of one per step),
  installs `sudo` if it's missing, and writes a passwordless drop-in scoped to the
  helper for *your* user - which makes the app work as a normal user (no root, no
  prompt) even though you aren't in the `sudo` group.

### Security
- **Don't trust `$USER` when writing the sudoers rule.** The target user is now
  taken from the real uid (`id -un`), validated against a strict username pattern
  and confirmed to exist, so a crafted `$USER` can't inject a wider sudoers spec
  (e.g. `NOPASSWD: ALL`) - `visudo -cf` alone does not catch that.
- **The build no longer runs as root.** `cargo`/`rustup` run as the invoking user
  via `runuser`, so dependency build scripts never execute with root privilege and
  the toolchain/artifacts stay in that user's home.

### Fixed
- Clear, actionable error when the app can't gain root (no passwordless sudo and
  no `pkexec`) instead of a cryptic `spawn failed: No such file or directory`.
- Installer fails loudly if it can't set up any privilege path, rather than
  reporting success and leaving a non-working install.

## [1.5.2] - 2026-06-17

### Fixed
- **Install works when `sudo` is present but you're not a sudoer.** The installer
  previously assumed that having the `sudo` binary meant you could use it - so on
  a Debian server where the login user isn't in the sudoers file (`<user> is not
  in the sudoers file`) it aborted. It now probes real sudo usability (`sudo -n` /
  admin-group membership) and falls back to `su` (the ROOT password), and
  auto-switches the helper authorization to a polkit rule. Tip: run `su -` first,
  then `./install.sh`, to be prompted once instead of per step.

## [1.5.1] - 2026-06-17

### Added
- **DNS / resolvconf check.** The Setup window now reports whether a resolvconf
  provider is available for tunnels that use a `DNS =` line, and **Fix
  automatically** installs one (`openresolv`; systemd-resolved also counts). This
  is the fix for minimal Debian, where such tunnels failed with
  `resolvconf: command not found`.
- **Per-distro guide** ([docs/DISTROS.md](docs/DISTROS.md)): what to install, what
  to set up, what survives a reboot, and when - only as a server/gateway - you need
  firewall and IP-forwarding changes.

### Changed
- **Works without `sudo`.** The installer falls back to `su` (the ROOT password)
  on Debian-minimal where `sudo` isn't present, and auto-switches the helper
  authorization from a sudoers drop-in to a polkit rule when there's no `sudo`.
- Activation failures are now explained in plain language (e.g. the missing
  resolvconf provider) instead of the raw `wg-quick` output.

### Fixed
- Tunnels with a `DNS =` line no longer fail on systems without a resolvconf
  provider: the installer best-effort installs `openresolv` when neither it nor
  systemd-resolved is present.

## [1.5.0] - 2026-06-17

### Added
- **First-run Setup wizard.** On launch, the app runs a read-only system check
  (WireGuard tools, the privileged helper + its authorization, `/etc/wireguard`,
  systemd, journald). If everything critical is OK it goes straight to Easy mode;
  if not, a friendly wizard explains what's missing in plain language with
  **Fix automatically** (installs `wireguard-tools` via your package manager
  through pkexec, and creates `/etc/wireguard` - with confirmation), **Show
  commands**, **Re-check** and **Skip for now**. It never connects tunnels,
  enables start-on-boot, or touches existing configs.
- **Beginner-friendly empty state.** With no tunnels, the app shows "No tunnels
  yet" and **Import .conf / Import QR image** buttons (plus **New tunnel** in
  Advanced mode) instead of a blank pane.

### Notes
- The app **does not bundle WireGuard kernel modules or `wg`/`wg-quick`** - it
  uses your system's `wireguard-tools`, and helps you install them.

## [1.4.1] - 2026-06-17

### Added
- **Distro packaging**: an **AUR `PKGBUILD`** (Arch) and an **RPM spec for COPR**
  (Fedora/RHEL/Rocky), plus `packaging/PACKAGING.md`. (A Flatpak manifest is
  included but documented as experimental — the sandbox is a poor fit for a
  privileged system VPN manager.)

### Changed
- Documentation now uses plain ASCII hyphens instead of em dashes.

## [1.4.0] - 2026-06-17

### Added
- **Live throughput + connection health** in the Interface card: real-time
  down/up speed and a handshake-based health line (OK / stale / waiting).
- The **system-tray tooltip** now shows the active tunnels *and* live throughput.
- **`wireguard-gui --version` / `--help`** - print and exit without opening a window.

### Changed
- **Removed demo mode** (`WGGUI_DEMO`) - the app always talks to real tunnels.
- The privileged helper now **bounds every `wg`/`wg-quick` call with a timeout**,
  so a hang (DNS, a stuck `PostUp`, a wedged interface) can't lock up the app.
- CI now runs a **smoke test** (`--version`/`--help` start and exit cleanly).

## [1.3.5] - 2026-06-17

### Fixed
- The toolbar **Remove (delete) button** is no longer crowded off the bar: the
  Easy/Advanced toggle moved out of the top toolbar to the **bottom action bar,
  next to Edit**, where it's always reachable (even with no tunnel selected).

## [1.3.4] - 2026-06-17

### Added
- **Easy mode** (default) for everyday users: a toolbar toggle that hides expert
  tools (Export, Running cfg, Save live, *Add empty tunnel*), leaving the
  everyday surface - Add (import/QR), Activate/Deactivate, Edit, Remove, Show QR,
  Start on boot. Click **Advanced mode** to reveal everything; the choice is
  remembered (`~/.config/wireguard-gui/mode`).

## [1.3.3] - 2026-06-16

### Fixed
- The form editor now matches config keys **exactly**: a directive like
  `PrivateKeyFile` or `EndpointBackup` is no longer mistaken (by prefix) for
  `PrivateKey`/`Endpoint`, which could otherwise let the form open for - and then
  rewrite - a config it can't actually represent. Covered by a regression test.

### Changed
- The privileged helper is also discovered **next to the binary**, so an
  extracted release tarball / AppImage works without first running `install.sh`.
- CI now **hard-fails on `shellcheck` warnings** for `wg-helper` and `install.sh`.

## [1.3.2] - 2026-06-16

### Fixed
- **A bracketed-IPv6 `Endpoint`** (e.g. `[2001:db8::1]:51820`) is now accepted by
  config validation again. The stricter endpoint check added in 1.3.1 wrongly
  rejected it, which blocked saving/importing IPv6-endpoint tunnels. Covered by a
  regression test.

### Changed
- **Privileged helper portability.** `wg-helper` no longer relies on GNU
  `find -printf` (it uses a pure-bash glob, so it works with BusyBox `find` on
  Alpine/Void) and filters listed tunnels to valid names. Start-on-boot now
  detects `systemctl` first and fails with a clear message on non-systemd
  systems (`is-enabled` reports "unknown" so the UI shows "off"); the log view
  explains when `journalctl` isn't available.
- **Helper-path override hardening.** `$WG_HELPER` is honoured freely in debug
  builds, but in release builds it is ignored unless `WG_ALLOW_UNSAFE_HELPER=1`
  is set *and* the target is an absolute, root-owned, non-world-writable file.

### Added
- Unit tests for config parsing, validation, name sanitisation and the form
  representability check; a CI step that shell-syntax-checks `wg-helper` and
  `install.sh`.
- README cross-links the terminal sibling (`wireguard-tui`) and explains the
  `wg-quick` (not NetworkManager) model, the init-system limitation, and the
  QR/private-key warning.

## [1.3.1] - 2026-06-16

### Fixed
- **Form editor no longer drops config it can't represent.** Editing an existing
  tunnel that has a second `[Peer]`, `PostUp`/`PreUp`/`PostDown`/`PreDown`,
  `Table`, or other unmapped keys could silently strip them on Save. The form now
  refuses to open for such configs (keeping them in raw-text mode, with a notice)
  and never overwrites a config it can't faithfully round-trip.
- **Closing the window no longer strands the app** when there is no system-tray
  host (e.g. GNOME without the AppIndicator extension): with no tray to restore
  from, closing the window now quits instead of hiding into nothing.
- The live-status timer no longer overwrites the detail pane of a tunnel you just
  selected with a stale background reading.
- **Bulk file import** now validates each file and flags ones that run root
  scripts, matching the single-import path.
- Stricter config validation: endpoints require bracketed IPv6 and a valid host;
  addresses/AllowedIPs are parsed as real IP/CIDR; tunnel-name sanitisation always
  yields a helper-valid name (no stray leading symbol or trailing dot).
- `install.sh` header check now also works when only `pkgconf` (not `pkg-config`)
  is installed.
- `wg-helper`'s `sync` verifies `wg-quick strip` before applying, so a strip
  failure can't wipe peers off the live interface.
- The release workflow fails if the `.deb` or checksums are missing, instead of
  publishing a partial release.

## [1.3.0] - 2026-06-16

### Added
- **Structured editor (form view).** The tunnel editor now toggles between a
  **Config text** view (the raw `.conf`) and an **Edit fields** form with
  labelled Interface (Private key, Address, DNS, Listen port, MTU) and Peer
  (Public key, Preshared key, Allowed IPs, Endpoint, Persistent keepalive)
  fields. New tunnels open in the form; existing ones open in text view. Edits
  in either view stay in sync, and **Generate keypair/PSK** update both.
- **Minimal-install dependency check** in `install.sh`: before building it
  verifies the C toolchain, `pkg-config`, and the `fontconfig`/`xkbcommon`/
  `dbus-1` dev headers, and checks `wireguard-tools` at runtime - failing early
  with a clear message instead of a cryptic build error.

### Changed
- **Closing the window now minimizes to the tray** instead of quitting, matching
  the WireGuard for Windows client. The app keeps running; use the tray's
  **Show WireGuard** to reopen or **Quit** to exit.
- The main window opens larger (1000×720) for a roomier tunnel list and details.

## [1.2.0] - 2026-06-16

### Added
- **Generate preshared key** (`wg genpsk`) in the editor - pairs with the
  existing keypair generation.
- **Apply edits to a running tunnel without dropping peer sessions** - saving an
  active tunnel now uses `wg syncconf`; wg-quick-only fields (Address/DNS/MTU/
  Table) still prompt to reconnect.
- **Running cfg** - copy a live tunnel's running config (`wg showconf`).
- **Save live** - write the running state back to the `.conf` (`wg-quick save`).

## [1.1.1] - 2026-06-16

### Added
- The tray menu now shows a connection-status header, a hover tooltip listing
  active tunnels, a **Tunnels** submenu, and a **Deactivate all** action; it
  refreshes live.

### Fixed
- The **Edit** button's tooltip no longer overflows the right edge of the window.

## [1.1.0] - 2026-06-16

### Added
- **Keypair generation** - new tunnels open with a freshly generated private key
  and a live "Public key" field (like the WireGuard for Windows dialog); a
  "Generate keypair" button regenerates on demand.
- **QR codes** - *Show QR* renders a tunnel as a QR code to scan into the mobile
  app, and *Add Tunnel → Import from QR code…* imports from a QR image.
- **Export** all tunnels to a `.zip` (the export button in the bottom bar).
- **Copy** buttons for public keys, the full config, and the log.
- **Start on boot** toggle (enables/disables the `wg-quick@` systemd unit).
- **System-tray icon** with per-tunnel activate/deactivate, Show, and Quit
  (StatusNotifierItem; needs KDE or GNOME's AppIndicator extension).
- **Tooltips** on the toolbar/detail buttons, and an **About** window.

### Changed
- Live status now polls on a **background thread**, so the UI no longer stutters.
- Buttons are sized to their content (no more over-wide buttons).

## [1.0.0] - 2026-06-16

### Added
- **Log tab** (Tunnels / Log) showing recent activity - the app's audit log plus
  `wg-quick` service entries - with a Refresh button.

### Changed
- The **Activate/Deactivate** button now sits below the interface details and
  **Edit** sits at the bottom-right of the window, matching the WireGuard for
  Windows client.
- Updated dependencies (`rfd` 0.14 → 0.15) and pinned all GitHub Actions to
  their latest releases (checkout v6, rust-cache v2.9.1, gh-release v3) - also
  clears the Node 20 deprecation warning in CI.

### Fixed
- **Text inputs no longer render blank on a light background.** Root cause: in a
  dark OS color scheme the std-widgets used near-white field fills that vanished
  on white windows (visible only when focused). The app now forces the light
  palette (`Palette.color-scheme = light`, built with the `fluent-light` style),
  so the editor and inputs render correctly on the light theme.

## [0.2.0] - 2026-06-16

### Added
- Screenshots in the README.
- A demo mode (`WGGUI_DEMO=1`) with sample tunnels, for local development.
- Project docs: `CONTRIBUTING.md`, `SECURITY.md`, this changelog, and GitHub
  issue/PR templates.
- **Signed releases**: `SHA256SUMS` is now signed with minisign
  (`SHA256SUMS.minisig`), and the public key (`minisign.pub`) ships in the repo
  and each release. Verify with
  `minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG`.

### Changed
- The editor warns (amber) when a config contains `PostUp`/`PreUp`/`PostDown`/
  `PreDown` - directives `wg-quick` runs as root on activation.

### Security
- `wg-helper` now exports a fixed `PATH`, so a hijacked caller `PATH` can't
  redirect the commands it runs as root (defense-in-depth on distros without
  sudo `secure_path`).
- Supply-chain hardening in CI/release: all third-party GitHub Actions are
  pinned to commit SHAs, and `linuxdeploy` is pinned to a release with a
  verified SHA-256.

### Fixed
- The editor window now uses the light theme (matching the main window) by
  painting its backdrop with a sibling `Rectangle` behind the content, instead
  of a window/ancestor background that left the text inputs blank.

## [0.1.0] - 2026-06-16

### Added
- First release: a native Linux GUI for managing WireGuard tunnels, modelled on
  the WireGuard for Windows client (Rust + Slint).
- Tunnel list with a live active/inactive indicator.
- Interface card (status, public key, listen port, addresses, DNS) and Peer
  card(s) with **live** latest-handshake and transfer, polled every second.
- Activate / Deactivate (`wg-quick up`/`down`).
- Import one or many `.conf` files - single imports open the editor to name the
  tunnel; bulk imports auto-deduplicate names.
- Inline editor with **config validation** (keys, addresses, endpoint, …); rename
  and remove tunnels.
- **Hardened privileged helper** (`wg-helper`): fixed paths, strict tunnel-name
  validation (no path traversal), atomic config writes, timestamped backups
  before every overwrite/delete, and journald audit logging.
- Privilege backends: **sudoers** (default) or **polkit** (`--polkit`, used by
  the `.deb`); `pkexec` fallback.
- **Universal installer** (`install.sh`) supporting apt, dnf/yum, pacman, zypper,
  apk, xbps and eopkg - auto-installs all missing dependencies (including
  `wireguard-tools` and Rust via rustup), then builds and installs.
- Release pipeline (GitHub Actions): `.deb`, AppImage and a binary tarball with
  `SHA256SUMS`, plus CI running rustfmt, clippy and a release build.

[Unreleased]: https://github.com/JamilleJung/wireguard-gui/compare/v1.5.0...HEAD
[1.5.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.4.1...v1.5.0
[1.4.1]: https://github.com/JamilleJung/wireguard-gui/compare/v1.4.0...v1.4.1
[1.4.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.5...v1.4.0
[1.3.5]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.4...v1.3.5
[1.3.4]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.3...v1.3.4
[1.3.3]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.2...v1.3.3
[1.3.2]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.1...v1.3.2
[1.3.1]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.1.1...v1.2.0
[1.1.1]: https://github.com/JamilleJung/wireguard-gui/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/JamilleJung/wireguard-gui/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/JamilleJung/wireguard-gui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JamilleJung/wireguard-gui/releases/tag/v0.1.0
