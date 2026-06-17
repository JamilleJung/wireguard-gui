<div align="center">

# wireguard-gui

A native Linux desktop client for WireGuard.

Built with Rust + Slint. No Electron, no WebView, no NetworkManager. It works
with plain `/etc/wireguard/*.conf` tunnels through `wg` and `wg-quick`, and
keeps privileged operations behind a small auditable helper.

![Screenshot of the active tunnel view](docs/screenshot.png)

[![CI](https://github.com/JamilleJung/wireguard-gui/actions/workflows/ci.yml/badge.svg)](https://github.com/JamilleJung/wireguard-gui/actions/workflows/ci.yml)
[![Releases](https://img.shields.io/badge/Releases-latest-2ea44f)](https://github.com/JamilleJung/wireguard-gui/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
![Rust](https://img.shields.io/badge/built%20with-Rust-orange.svg)
![Platform: Linux](https://img.shields.io/badge/platform-Linux-success.svg)

</div>

> Prefer the terminal? The sibling **[wireguard-tui](https://github.com/JamilleJung/wireguard-tui)**
> is the same project philosophy in a keyboard-driven TUI.

## Design philosophy

This project is intentionally small.

It does not try to become a WireGuard platform, daemon, or configuration
database. It stays close to the Linux WireGuard workflow: plain `.conf` files in
`/etc/wireguard`, `wg`, `wg-quick`, `wg show`, `wg showconf`, `wg syncconf`,
`wg-quick save`, and systemd `wg-quick@<name>` units where available.

The GUI and TUI are separate first-class tools. Install the one you want. Hack
the one you want. They share code where useful, but there is no mandatory
runtime core or hidden platform layer.

The goal is a native client that is easy to use, easy to inspect, and easy to
fork.

## Why this exists

Linux already ships the primitives needed to manage WireGuard well. The missing
piece for many desktop users is a small native client that presents those
primitives clearly without routing everything through NetworkManager.

`wireguard-gui` keeps the Linux-native workflow and gives it a desktop face:
plain configs, a live tunnel list, detail panes, safe editing, and a small
privilege boundary. NetworkManager was the original spark, not the whole
philosophy.

## What it does

- Lists tunnels from `/etc/wireguard`.
- Shows active/inactive status and live handshake/transfer details.
- Activates and deactivates tunnels with `wg-quick up` / `wg-quick down`.
- Imports `.conf` files and QR images.
- Creates new tunnels with a generated keypair.
- Supports raw config editing and a structured single-peer form.
- Validates before save, writes atomically, and keeps backups before overwrite.
- Renames and removes tunnels with helper-backed privilege checks.
- Shows a tunnel as QR, copies public keys, copies configs, and exports all
  tunnels to a zip.
- Uses `wg showconf`, `wg syncconf`, and `wg-quick save` for live state.
- Toggles start-on-boot with systemd `wg-quick@<name>` when systemd is present.
- Shows recent activity in a log tab.
- Runs a first-run Setup wizard when critical prerequisites are missing; it
  checks WireGuard tools, the helper, helper authorization, `/etc/wireguard`,
  systemd, journald, and DNS support.
- Offers Easy / Advanced mode and a tray icon where the desktop supports it.

## What it deliberately does not do

- No NetworkManager layer.
- No Electron or WebView.
- No mandatory daemon or background service.
- No hidden config database.
- No bundled WireGuard kernel module or `wg` binaries.
- No root UI.

## Screenshot

| Tunnel detail (active) | Inactive tunnel |
|:---:|:---:|
| ![active](docs/screenshot.png) | ![inactive](docs/screenshot-inactive.png) |
| **Config editor** | **Show QR** |
| ![editor](docs/screenshot-editor.png) | ![qr](docs/screenshot-qr.png) |

## Install

### Prebuilt packages

The release page normally includes:

- `wireguard-gui_*_amd64.deb`
- `wireguard-gui-*-x86_64-linux.tar.gz`
- `wireguard-gui-*-x86_64.AppImage` when the AppImage job succeeds
- `SHA256SUMS`, `SHA256SUMS.minisig` when signing is configured, and `minisign.pub`

On Debian / Ubuntu / Mint, the `.deb` is the simplest path because it installs
the helper and sets up the polkit rule.

### From source

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh
```

`install.sh` detects your package manager, installs build dependencies and
`wireguard-tools`, builds the release binary, installs the helper and desktop
files, and configures passwordless helper access for the active local user.

Supported package managers:

| Distro family | Package manager |
|---|---|
| Debian / Ubuntu / Mint | `apt` |
| Fedora / RHEL / Rocky | `dnf` / `yum` |
| Arch / Manjaro / EndeavourOS | `pacman` |
| openSUSE | `zypper` |
| Alpine | `apk` |
| Void | `xbps-install` |
| Solus | `eopkg` |

Uninstall:

```sh
./install.sh uninstall
```

Auth backend choice:

```sh
./install.sh           # sudoers drop-in (default)
./install.sh --polkit  # polkit rule instead
```

## Verify releases

Use the checksum file and minisign signature from the release page:

```sh
sha256sum -c SHA256SUMS --ignore-missing
minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG
```

## Usage

Launch `wireguard-gui` from the application menu or from a terminal.

The main window is split into a tunnel list on the left and an interface / peer
detail view on the right. The bottom bar switches between Easy and Advanced
mode. The Add Tunnel menu handles file import, QR import, new tunnel creation,
and About. The Log tab shows recent journal entries and helper activity.

## Security model

The GUI runs as a normal user. Privileged operations go through one small shell
helper, `packaging/wg-helper`, which is installed as
`/usr/local/lib/wireguard-gui/wg-helper` or `/usr/lib/wireguard-gui/wg-helper`
when packaged.

The helper exposes fixed verbs only:

`list`, `read`, `dump`, `up`, `down`, `save`, `rename`, `delete`, `enable`,
`disable`, `is-enabled`, `sync`, `showconf`, `persist`, and `log`.

Hardening in the helper:

- Fixed `PATH` and fixed `/etc/wireguard` path.
- Tunnel names must match `^[A-Za-z0-9][A-Za-z0-9_.-]{0,14}$`.
- No path traversal, no caller-controlled root destination, no shell eval of
  user input.
- Atomic config writes with backups before overwrite, delete, or rename.
- Audit log entries via `logger` / journald.
- Start-on-boot changes are kept separate from file writes.

Authorization is passwordless sudoers by default, or a polkit rule with
`--polkit`. If neither is set up, the app falls back to `pkexec`.

QR export and zip export contain the private key. Treat them like the config
file itself.

## Hacking on it

This repo is MIT licensed. Fork it and change it.

### Codebase map

| Path | Purpose |
|---|---|
| `ui/app.slint` | All Slint UI layout and bindings |
| `src/main.rs` | App startup and UI wiring |
| `src/backend.rs` | WireGuard orchestration, validation, helper client |
| `src/doctor.rs` | Read-only setup checks |
| `packaging/wg-helper` | Privileged helper |
| `install.sh` | Distro-aware installer |
| `packaging/` | Desktop entries, icon, packaging metadata |
| `.github/workflows/` | CI and release automation |

### Build and test

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
```

### Run from source

```sh
cargo run --release
```

In development, the binary looks for the in-tree helper first. You can point it
at a local helper with `WG_HELPER=/path/to/wg-helper`. In release builds that
override is only honored for a root-owned, non-world-writable file when
`WG_ALLOW_UNSAFE_HELPER=1` is set.

### Add a feature safely

If you touch privileged behavior, keep the helper verb-based, validate names
before filesystem access, and keep writes atomic. If you touch the UI, keep the
normal-user flow intact and surface failures plainly.

## Troubleshooting

- `wg-quick` fails with `resolvconf: command not found`: install a resolvconf
  provider such as `openresolv`, or use a system with `systemd-resolved`.
- The helper keeps prompting: run `./install.sh` or `./install.sh --polkit`
  so the helper path is authorized.
- Start-on-boot is unavailable: your system does not have `systemctl`.
- The tray icon does not appear: your desktop needs a StatusNotifier / App
  Indicator host.
- QR export or zip export contains secrets: that is expected. Treat the output
  like the tunnel config itself.
- No window, or invisible text inputs: use the packaged build or the default
  `fluent-light` style; this project does not ship a dark WebView stack.

## Known limitations

- Start-on-boot is systemd-only.
- Prebuilt binaries are x86_64 only.
- The GUI is desktop-session dependent; it is not designed for SSH-only use.
- The tray icon depends on desktop support for StatusNotifier / AppIndicator.
- QR and zip exports include private keys.
- The helper remains a shell script for now; the privileged surface is small,
  but not yet Rust.

## Roadmap

- Rust helper for the privileged operations.
- Better multi-peer editor support.
- More packaged architectures where the release workflow supports them.
- Additional tests around rename and import edge cases.

## License

MIT. WireGuard is a registered trademark of Jason A. Donenfeld. This project is
an independent, unofficial client and is not affiliated with or endorsed by the
WireGuard project.

---

## 🩹 Troubleshooting

- **A tunnel won't activate** - check the config with `Edit`; the validator flags
  bad keys/addresses/endpoints. Also confirm `wg-quick up <name>` works in a terminal.
- **"Couldn't open editor" / pkexec prompts every time** - re-run `./install.sh`
  to (re)create the `sudoers` drop-in.
- **Blank window / no GPU** - ensure an OpenGL runtime is installed (the installer
  handles this); on headless/odd setups try `SLINT_BACKEND=winit-software wireguard-gui`.

---

## ⚠️ Known limitations

- **Built & tested on x86_64, GNOME/Wayland.** It should work on other desktops
  and X11, but those are less tested. Prebuilt binaries are x86_64 only - other
  architectures can build from source.
- **The system-tray icon** uses the StatusNotifierItem standard. It shows on KDE
  and most trays out of the box; on **GNOME it needs the AppIndicator extension**.
- **Multiple peers** are shown (one card each) and editable via the raw config,
  but there's no dedicated add/remove-peer UI yet.
- **AppImage privileged actions** work best with a system helper present - run
  `install.sh` once, or use the `.deb`, for passwordless control.

---

## 🧗 The story behind it - pain points & fixes

I built this because the existing Linux options either hide WireGuard behind
NetworkManager (which bit me hard) or don't look/behave like the clean Windows
client. Getting there meant fighting through some genuinely nasty issues - written
up here in case they save you the days they cost me.

### 1. NetworkManager silently ate my WireGuard peer
The original symptom that started everything: the VPN just stopped working -
"DNS not resolving", no internet when the tunnel was up. The cause turned out to
be **NetworkManager dropping the entire `[Peer]` section** of the connection (a
long-standing cross-distro bug, usually triggered by editing/saving WireGuard in
a GUI). With no peer, NM still set a default route into the empty tunnel and
black-holed *all* traffic, DNS included.
**Fix:** stop managing the tunnel through NetworkManager entirely - run it
standalone via `wg-quick` + systemd, and mark `wg*` as `unmanaged` in NM so it
can never touch (and re-break) the interface again. That experience is exactly
why this app talks to `wg-quick` directly instead of going through NM.

### 2. Slint text inputs rendered *blank* on a light window - the big one
The editor's `LineEdit`/`TextEdit` showed up **completely empty** on a white
background, visible only when focused. It happened with **every** renderer
(femtovg, software, *and* Skia), so it wasn't GPU/glyph-cache specific - which
ruled out the obvious suspects and cost the most time.
The real cause turned out to be **contrast, not rendering**: with the OS in dark
mode, Slint's std-widgets pick a dark palette where the unfocused input fill is
white-at-6%-opacity with white text. Put that over a white window and it's
white-on-white - invisible; the focused state uses a near-black fill, which is
why only the focused field showed. (Confirmed against the Fluent style source.)
**Fix:** force the **light** palette so the inputs use dark-text-on-light fills
that match the white window - `Palette.color-scheme = ColorScheme.light` plus the
`fluent-light` style in `build.rs`. (Related gotcha ruled out along the way: an
explicit `min-height` on a `TextEdit` can also suppress its text on femtovg,
[Slint #6896](https://github.com/slint-ui/slint/issues/6896).)

### 3. Running privileged operations without nagging for a password
`wg`/`wg-quick` need root, but I didn't want a password prompt on every status
poll. **Fix:** a single small, auditable `wg-helper` script with a fixed verb set
and strict tunnel-name validation, whitelisted (and *only* it) in a `sudoers`
drop-in - with a `pkexec` fallback when that isn't set up.

### 4. "It should just install" on any distro
Build-from-source on Linux means a C toolchain, `pkg-config`, fontconfig +
libxkbcommon headers, an OpenGL runtime, `wireguard-tools`, and Rust - and every
distro names them differently (Debian and Arch in particular love to be missing
*something*). **Fix:** `install.sh` detects the package manager, maps the right
package names, installs whatever's missing (Rust included, via rustup), then
builds and installs - one command, anywhere.

### Small stuff that still mattered
- Status banners that never went away → auto-dismiss after a few seconds.
- Imports clobbering same-named tunnels → single imports open the editor to name
  them; bulk imports auto-deduplicate.
- Config typos only surfacing at activation time → validate the config on Save.

---

## 🤝 Contributing

Issues and PRs welcome! The code is small and tidy:

| Path | Purpose |
|------|---------|
| `ui/app.slint` | The entire UI (Slint markup). |
| `src/backend.rs` | Privilege handling, `wg` orchestration, config parse + validation. |
| `src/main.rs` | Wires UI callbacks to the backend. |
| `packaging/wg-helper` | The single privileged entry point. |
| `install.sh` | Universal build + install. |

---

## ⭐ Star this project

If wireguard-gui is useful to you, **please give it a star on GitHub** - it
genuinely helps other people discover the project and motivates further work.

👉 **[Star wireguard-gui on GitHub](https://github.com/JamilleJung/wireguard-gui)** ⭐

You can also **watch** the repo for releases and **fork** it to hack on your own ideas.

---

## ☕ Buy me a coffee

This is a free, open-source project built in spare time. If it saved you some
trouble and you'd like to say thanks, a coffee is hugely appreciated 💛

<div align="center">

[![Buy Me A Coffee](https://img.shields.io/badge/Buy%20Me%20A%20Coffee-support-FFDD00?style=for-the-badge&logo=buy-me-a-coffee&logoColor=black)](https://www.buymeacoffee.com/jamillejung)

**[☕ buymeacoffee.com/jamillejung](https://www.buymeacoffee.com/jamillejung)**

</div>

---

## 📄 License

[MIT](LICENSE). WireGuard is a registered trademark of Jason A. Donenfeld; this
is an independent, unofficial client and is not affiliated with or endorsed by
the WireGuard project.
