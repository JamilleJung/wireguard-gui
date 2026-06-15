# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

[Unreleased]: https://github.com/JamilleJung/wireguard-gui/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/JamilleJung/wireguard-gui/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/JamilleJung/wireguard-gui/releases/tag/v0.1.0
