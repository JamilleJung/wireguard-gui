<div align="center">

# wireguard-gui

A native Linux desktop client for WireGuard.

Built with Rust + Slint. No Electron. No WebView. No NetworkManager layer. It
manages plain `/etc/wireguard/*.conf` tunnels through `wg` and `wg-quick`, and
keeps root operations behind a small auditable Rust helper.

![Historical active tunnel view](docs/screenshot.png)

[![CI](https://github.com/JamilleJung/wireguard-gui/actions/workflows/ci.yml/badge.svg)](https://github.com/JamilleJung/wireguard-gui/actions/workflows/ci.yml)
[![Releases](https://img.shields.io/badge/Releases-latest-2ea44f)](https://github.com/JamilleJung/wireguard-gui/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)
![Platform: Linux](https://img.shields.io/badge/platform-Linux-success.svg)

</div>

Prefer the terminal? The sibling
[`wireguard-tui`](https://github.com/JamilleJung/wireguard-tui) provides the same
project philosophy as a keyboard-driven terminal UI.

## 💡 Design philosophy

`wireguard-gui` manages plain `/etc/wireguard/<name>.conf` files with `wg` and
`wg-quick`. It is a Linux desktop application, not a VPN provider, daemon,
configuration database, or NetworkManager plugin. Obtain a config from your
provider or configure both ends of your own tunnel first.

The GUI and [terminal sibling](https://github.com/JamilleJung/wireguard-tui)
share a file-based approach and some operations. They have separate interfaces,
packaging and feature sets; matching version numbers do not imply feature parity.

<a id="screenshots"></a>

## 🖼️ Screenshots

| Active tunnel | Inactive tunnel |
|---|---|
| ![active tunnel](docs/screenshot.png) | ![inactive tunnel](docs/screenshot-inactive.png) |

| Config editor | QR view |
|---|---|
| ![config editor](docs/screenshot-editor.png) | ![qr view](docs/screenshot-qr.png) |

These are historical interface examples; controls and layout differ in 1.9.1.
Values visible in these published config/QR screenshots are public; never reuse
their keys. The current controls are described below and in the
[tutorial](docs/TUTORIAL.md).

## ✨ What it does

- Lists saved tunnels, activates/deactivates them, and displays interface/peer
  details, recent handshake health, transfer totals and throughput.
- Imports one or several `.conf` files, or a PNG/JPEG QR-code image. A single
  import opens for review; multiple-file import saves valid files with unique
  names and skips failures.
- Creates an Interface-only, full-tunnel or split-tunnel preset with generated
  keys. Review and supply your own addresses, peer key and endpoint before use.
- Edits raw configs or simple multi-peer forms; comments, repeated directives,
  routing directives and unknown fields remain in raw text.
- Shows a complete config as a QR code, exports configs to ZIP, and manages
  private local backup archives.
- Offers systemd start-on-boot, connection diagnostics, journal viewing and
  helper-managed firewall kill-switch rules.
- Provides a tray menu where the desktop supports StatusNotifier items, an
  in-app Help guide, and a signed-release update flow.

<a id="install"></a>

## 📦 Install

Current release: **[1.9.1](https://github.com/JamilleJung/wireguard-gui/releases/tag/v1.9.1)**.
Choose a package for your CPU and Linux environment. WireGuard tools and the
required system libraries are not bundled into the tarballs.

| Download | Purpose |
|---|---|
| `wireguard-gui_1.9.1-1_amd64.deb` | Debian-family x86_64 package with GUI, helper, desktop entry, icon and polkit rule |
| `wireguard-gui-1.9.1-x86_64-linux.tar.gz` | Linux x86_64 binaries and installer assets |
| `wireguard-gui-1.9.1-aarch64-linux.tar.gz` | Linux ARM64 binaries and installer assets |
| `wireguard-gui-1.9.1-x86_64.AppImage` | x86_64 AppImage; optional in the workflow and present in 1.9.1 |
| `SHA256SUMS`, `SHA256SUMS.minisig`, `minisign.pub` | Artifact checksums, signature and public key |

### Verify a download

Download your artifact, `SHA256SUMS` and `SHA256SUMS.minisig` into one directory.
Install `minisign` and the system's SHA-256 tools, then verify the signed manifest
against the project key before checking the selected file:

```sh
asset=wireguard-gui-1.9.1-x86_64-linux.tar.gz
minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG
awk -v file="$asset" '$2 == file { print; n++ } END { if (n != 1) exit 1 }' SHA256SUMS > selected.sha256 &&
sha256sum --check --strict selected.sha256
```

Both signature and checksum checks must succeed. Change `asset` to the exact
filename you downloaded. A public key downloaded alongside an artifact is not
an independent trust anchor; compare it with the key committed in this repo or
use the inline key above. Releases fail rather than publish unsigned artifacts.

### Debian-family package

```sh
sudo apt install ./wireguard-gui_1.9.1-1_amd64.deb
wireguard-gui
```

The package installs under `/usr`; it **does not create the `wireguard` group or
add users to it**. For passwordless polkit access from an active local desktop
session, an administrator can grant the intended user membership:

```sh
sudo groupadd -f wireguard
sudo usermod -aG wireguard "$USER"
```

Log out and back in. Group members can manage all tunnels and read their keys;
only grant membership to trusted users. Without the passwordless rule matching,
`pkexec` may ask for administrator authentication or be denied by local policy.

### Extracted binary bundle

After verification, extract the tarball for your CPU, enter its directory and
run the installer as your normal user:

```sh
tar -xzf wireguard-gui-1.9.1-x86_64-linux.tar.gz
cd wireguard-gui-1.9.1-x86_64-linux
./install.sh
```

The installer recognizes the adjacent prebuilt application/helper when no Cargo
manifest is present, stages them with executable permissions, and skips Rust,
compiler and development-header setup. It still installs runtime dependencies
and helper authorization. An incomplete bundle is rejected.

You can launch the adjacent binary directly to try the portable bundle, but
privileged operations require a usable helper and an authorization path.
AppImage users likewise need helper authorization; an installed helper provides
the usual system integration. Use your package manager for package-managed
upgrades rather than mixing installation prefixes.

### Build from source

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh
```

A source checkout builds with Rust and Slint development libraries. The installer
supports `apt-get`, `dnf`/`yum`, `pacman`, `zypper`, `apk`, `xbps-install` and
`eopkg`; it installs dependencies, builds as the invoking user when that user
can be determined, then installs under `/usr/local` by default.

```sh
./install.sh             # default: per-user sudoers grant for the helper
./install.sh --polkit    # group-restricted polkit rule instead
./install.sh uninstall   # leaves /etc/wireguard configs in place
```

The script creates `wg-gui` as an alias for `wireguard-gui`. Cargo builds and the
provided distro-package recipes do not create that alias. A direct root login
requires explicit `--allow-root` (or `WG_ALLOW_ROOT=1`); normal-user installation
is the recommended path. See [distro notes](docs/DISTROS.md) and
[packaging details](packaging/PACKAGING.md).

## 🎮 Usage

```sh
wireguard-gui            # launch; a second launch raises the existing window
wireguard-gui --version  # also -V; no window
wireguard-gui --help     # also -h; no window
```

There are no GUI `doctor`, `setup` or command-line tunnel-management subcommands.
An unknown first argument exits with status 2.

- **Tunnels:** select a tunnel; use **Activate**, **Deactivate**, **Diagnose**,
  **Show QR**, **Start on boot** or **Edit**. The bottom bar provides **Add
  Tunnel**, delete and export-all. The menu button opens **Help…**, **Check for
  updates…** and **About…**.
- **Advanced ▾** on the selected tunnel card reveals **Kill switch**, **Running
  cfg** and **Save live**. There is no global Easy/Advanced mode. The expansion
  preference is saved, not a separate preference per tunnel.
- **Edit fields / Config text:** common Interface and Peer fields, peer
  navigation/add/remove, keypair/PSK generation, and copy actions. **Advanced
  fields** reveals Listen port and MTU. New entries use **Create** or **Create &
  Activate**; existing entries use **Save**. Changing Name renames the tunnel.
- **Log:** refresh, substring filter, **This tunnel**, save, clear the displayed
  buffer, or copy. Clearing does not delete the system journal.
- **Backup:** **New backup**, select an archive, then restore, export or delete.

Saved active-tunnel edits attempt `wg syncconf`. Changes to Address, DNS, MTU,
Table or routes still need deactivation/reactivation; a successful live sync
only applies WireGuard runtime settings. Renaming stops an active old interface
and preserves its systemd enablement where available; it does not activate the
new name automatically.

### Backups and sensitive data

Backup-tab ZIPs live in `$XDG_DATA_HOME/wireguard/backups`, falling back to
`~/.local/share/wireguard/backups`. This location is shared with the TUI.
Archives are written `0600`; newly created backup directories use `0700`.
The helper separately keeps per-file backups in `/etc/wireguard/.backup` before
overwrite, rename, delete or Save live.

Restore processes only top-level `.conf` entries with valid, unique tunnel
names. It preflights every selected config and rejects script hooks, malformed
configs, entries over **1 MiB**, more than **1,024 configs**, or over **16 MiB**
of total config text before the first save. Unrelated files are skipped.
Restore overwrites matching saved names, does not remove extra tunnels or sync
running interfaces, and is **not transactional**: a later save/authorization
failure can leave earlier saves applied. The Backup tab restores files from
its own backup directory; there is no external-ZIP import picker.

QR codes, ZIP exports, backups, Config text and running-config copies include
private keys. They are not encrypted. Protect the screen, clipboard and files.

### Kill switch and start-on-boot

The kill switch uses nftables when `nft` exists, otherwise iptables/ip6tables.
It inserts output rejection before pre-existing accept rules, with exceptions
for loopback, the selected tunnel and its WireGuard fwmark. If `SSH_CONNECTION`
reaches the helper, established SSH return traffic on that session's port is
also allowed. The helper can set a missing fwmark at runtime; it does not add a
FwMark directive to the saved file. With non-loopback IPv6 present, the iptables
fallback refuses to proceed without `ip6tables`.

These rules affect host outbound traffic, including destinations outside a
split tunnel. They are runtime rules, with no daemon or automatic persistence.
Helper deactivation/delete/rename attempts cleanup; external service changes
can leave rules behind. Restarting the app or activating the tunnel does not
rebuild them. After upgrading to 1.9.1, turn an active kill switch off and on to
apply the corrected rules; plan for the brief period without those rules.

**Start on boot** enables/disables `wg-quick@<name>` through systemd. It is a
boot-time setting, not desktop login autostart. Other init systems require their
own service configuration.

### Setup, diagnostics and updates

The Setup window opens at startup when a critical prerequisite is missing. It
checks tools, helper/auth, config directory, DNS integration, clock-sync status,
systemd and journald availability. Polkit authorization is a rule-presence hint, not
proof of group/session membership or of the helper later selected by the backend.
**Show commands** gives suggested fixes;
**Re-check** checks again; **Skip for now** opens the main window. It does not
probe kernel-module availability.

**Fix automatically** runs selected system commands through `pkexec`: install
WireGuard tools or a DNS provider, enable an inactive systemd-resolved service
when its compatibility shim is detected, and create `/etc/wireguard` if needed.
It does not install/authorize the helper, create tunnel configs or connect them.
Read the proposed commands before applying system changes.

**Diagnose** checks clock sync, interface/handshake state, endpoint ping and
basic DNS. Results are hints, not proof that a VPN endpoint, routing or firewall
is correctly configured; blocked ICMP alone does not prove a server is down.

Startup and manual update checks contact GitHub using `curl`. To disable the
startup check, set `WG_NO_UPDATE_CHECK=1` or create
`${XDG_CONFIG_HOME:-$HOME/.config}/wireguard-gui/no-update`. Opening the Updates
window still checks manually. **Update now** verifies the architecture-specific
tarball with the compiled-in minisign key and its exact signed checksum, then
runs its installer. It needs `curl`, `tar`, `sha256sum`, `minisign` and normally
`pkexec`; automatic minisign installation is only best-effort. The installer
uses its normal prefix/auth defaults, not package-manager integration. Restart
the app after installation. See the [tutorial](docs/TUTORIAL.md#updates).

## 🛡️ Security and privilege model

Run the GUI as a normal user. The helper client chooses direct execution if
already root, otherwise a working `sudo -n` grant, then `pkexec`. Install/setup
and update actions can also run installer/package commands with administrator
authorization. A passwordless helper grant permits control of all managed
tunnels and access to their keys, not isolation between users.

The helper uses fixed tool locations and verbs, validates names, writes saved
configs through a `0600` temporary file with sync and rename, and records
mutating actions with secret redaction. It rejects `PreUp`, `PostUp`, `PreDown`
and `PostDown` on save/rename. Configs placed directly by an administrator remain
that administrator's responsibility: activating one runs `wg-quick` normally.
See [SECURITY.md](SECURITY.md) for the boundary and accepted dependency notices.

## 🔧 Hacking on it

See [CONTRIBUTING.md](CONTRIBUTING.md) for prerequisites, source layout, checks,
helper development and documentation conventions. A quick Linux check is:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
shellcheck -S warning install.sh tests/helper-validation.sh tests/installer-sanity.sh
bash tests/helper-validation.sh target/release/wg-helper
bash tests/installer-sanity.sh
```

## 🐛 Troubleshooting

- **Active, but no traffic:** use Diagnose and check server peer registration,
  endpoint, routing, firewall, DNS and clock sync. An up interface does not prove
  a handshake or useful traffic; idle peers can also have an old handshake.
- **DNS activation fails:** `DNS =` needs a working `resolvconf` compatibility
  command. If it is the `resolvectl` shim, systemd-resolved must be active.
- **Helper prompts or fails:** check the installed helper path, sudoers grant or
  active-local `wireguard` group membership; see [distro notes](docs/DISTROS.md).
- **No tray icon:** your desktop needs a StatusNotifier/AppIndicator host.
  Closing quits when no tray host is available; otherwise use tray **Quit**.
- **Blank inputs/window:** check graphics/runtime libraries. On proprietary
  NVIDIA + Wayland with `DISPLAY` available, the app prefers XWayland; set
  `WG_FORCE_WAYLAND=1` to opt out of that workaround.
- **Missing logs/boot control:** journal viewing needs `journalctl`; the boot
  toggle needs systemd. Avoid managing the same interface in another tool.

## ⭐ Star this project

If `wireguard-gui` is useful to you, **please give it a star on GitHub** - it
genuinely helps other people discover the project and motivates further work.

👉 **[Star wireguard-gui on GitHub](https://github.com/JamilleJung/wireguard-gui)** ⭐

You can also **watch** the repo for releases and **fork** it to hack on your own ideas.

## ☕ Buy me a coffee

This is a free, open-source project built in spare time. If it saved you some
trouble and you'd like to say thanks, a coffee is hugely appreciated 💛

<div align="center">

[![Buy Me A Coffee](https://img.shields.io/badge/Buy%20Me%20A%20Coffee-support-FFDD00?style=for-the-badge&logo=buy-me-a-coffee&logoColor=black)](https://www.buymeacoffee.com/jamillejung)

**[☕ buymeacoffee.com/jamillejung](https://www.buymeacoffee.com/jamillejung)**

</div>

## 📄 License

MIT. WireGuard is a registered trademark of Jason A. Donenfeld. This is an
independent, unofficial client and is not affiliated with or endorsed by the
WireGuard project.
