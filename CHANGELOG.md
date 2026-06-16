# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.3.2] - 2026-06-16

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
  `dbus-1` dev headers, and checks `wireguard-tools` at runtime — failing early
  with a clear message instead of a cryptic build error.

### Changed
- **Closing the window now minimizes to the tray** instead of quitting, matching
  the WireGuard for Windows client. The app keeps running; use the tray's
  **Show WireGuard** to reopen or **Quit** to exit.
- The main window opens larger (1000×720) for a roomier tunnel list and details.

## [1.2.0] - 2026-06-16

### Added
- **Generate preshared key** (`wg genpsk`) in the editor — pairs with the
  existing keypair generation.
- **Apply edits to a running tunnel without dropping peer sessions** — saving an
  active tunnel now uses `wg syncconf`; wg-quick-only fields (Address/DNS/MTU/
  Table) still prompt to reconnect.
- **Running cfg** — copy a live tunnel's running config (`wg showconf`).
- **Save live** — write the running state back to the `.conf` (`wg-quick save`).

## [1.1.1] - 2026-06-16

### Added
- The tray menu now shows a connection-status header, a hover tooltip listing
  active tunnels, a **Tunnels** submenu, and a **Deactivate all** action; it
  refreshes live.

### Fixed
- The **Edit** button's tooltip no longer overflows the right edge of the window.

## [1.1.0] - 2026-06-16

### Added
- **Keypair generation** — new tunnels open with a freshly generated private key
  and a live "Public key" field (like the WireGuard for Windows dialog); a
  "Generate keypair" button regenerates on demand.
- **QR codes** — *Show QR* renders a tunnel as a QR code to scan into the mobile
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
- **Log tab** (Tunnels / Log) showing recent activity — the app's audit log plus
  `wg-quick` service entries — with a Refresh button.

### Changed
- The **Activate/Deactivate** button now sits below the interface details and
  **Edit** sits at the bottom-right of the window, matching the WireGuard for
  Windows client.
- Updated dependencies (`rfd` 0.14 → 0.15) and pinned all GitHub Actions to
  their latest releases (checkout v6, rust-cache v2.9.1, gh-release v3) — also
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
  `PreDown` — directives `wg-quick` runs as root on activation.

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
- Import one or many `.conf` files — single imports open the editor to name the
  tunnel; bulk imports auto-deduplicate names.
- Inline editor with **config validation** (keys, addresses, endpoint, …); rename
  and remove tunnels.
- **Hardened privileged helper** (`wg-helper`): fixed paths, strict tunnel-name
  validation (no path traversal), atomic config writes, timestamped backups
  before every overwrite/delete, and journald audit logging.
- Privilege backends: **sudoers** (default) or **polkit** (`--polkit`, used by
  the `.deb`); `pkexec` fallback.
- **Universal installer** (`install.sh`) supporting apt, dnf/yum, pacman, zypper,
  apk, xbps and eopkg — auto-installs all missing dependencies (including
  `wireguard-tools` and Rust via rustup), then builds and installs.
- Release pipeline (GitHub Actions): `.deb`, AppImage and a binary tarball with
  `SHA256SUMS`, plus CI running rustfmt, clippy and a release build.

[Unreleased]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.2...HEAD
[1.3.2]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.1...v1.3.2
[1.3.1]: https://github.com/JamilleJung/wireguard-gui/compare/v1.3.0...v1.3.1
[1.3.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.2.0...v1.3.0
[1.2.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.1.1...v1.2.0
[1.1.1]: https://github.com/JamilleJung/wireguard-gui/compare/v1.1.0...v1.1.1
[1.1.0]: https://github.com/JamilleJung/wireguard-gui/compare/v1.0.0...v1.1.0
[1.0.0]: https://github.com/JamilleJung/wireguard-gui/compare/v0.2.0...v1.0.0
[0.2.0]: https://github.com/JamilleJung/wireguard-gui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JamilleJung/wireguard-gui/releases/tag/v0.1.0
