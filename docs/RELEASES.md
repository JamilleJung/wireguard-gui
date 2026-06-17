# Releases

This page explains where to get wireguard-gui, how the downloads are signed and
verified, how each release is built, and what changed in every version.

If you have never touched WireGuard before, that is fine: the short version is
"download a build, check the signature, run it, and let the first-run Setup
window walk you through anything that is missing". The detail is below.

---

## Where to get releases

Every version is published on the GitHub **Releases** page:

- <https://github.com/JamilleJung/wireguard-gui/releases>
- The newest build is always at <https://github.com/JamilleJung/wireguard-gui/releases/latest>

Each tagged release is built by GitHub Actions (no hand-built binaries) and
ships the following artifacts:

| Artifact | For |
|----------|-----|
| `wireguard-gui_*_amd64.deb` | Debian / Ubuntu / Mint - `sudo apt install ./wireguard-gui_*_amd64.deb` (sets up the polkit rule automatically) |
| `wireguard-gui-*-x86_64.AppImage` | Any distro - `chmod +x *.AppImage && ./*.AppImage` |
| `wireguard-gui-*-x86_64-linux.tar.gz` | Portable binary bundle plus `install.sh` |
| `SHA256SUMS` | Checksums for all of the above |
| `SHA256SUMS.minisig` | The minisign signature over `SHA256SUMS` |
| `minisign.pub` | The minisign public key (also committed in the repo) |

Prebuilt binaries are **x86_64 only**. Other architectures can build from
source with `./install.sh` (see the README). Note that the `.deb`, AppImage and
tarball do **not** bundle WireGuard itself - they still need your distro's
`wireguard-tools` present (the `.deb` is the smoothest because it pulls in the
desktop and polkit integration).

---

## How to verify a download (recommended)

Releases are signed so you can confirm a download is authentic and untampered.
The chain is: a `SHA256SUMS` file lists the SHA-256 of every artifact, and that
`SHA256SUMS` file is itself signed with [minisign](https://jedisct1.github.io/minisign/).

Download the artifact you want plus `SHA256SUMS` and `SHA256SUMS.minisig` into
the same folder, then:

```sh
# 1) Confirm the artifact matches its checksum.
sha256sum -c SHA256SUMS --ignore-missing

# 2) Confirm the checksum file itself was signed by the project key.
#    Needs `minisign` installed; the public key also ships as minisign.pub.
minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG
```

Both commands must succeed. Step 1 proves the bytes you downloaded are the bytes
that were checksummed; step 2 proves the checksum list came from the project and
was not swapped out. If you prefer, you can point minisign at the key file
instead of the inline key:

```sh
minisign -Vm SHA256SUMS -p minisign.pub
```

The release workflow refuses to publish a partial release: if the `.deb` or the
checksums are missing, the job fails rather than shipping something unverifiable.

---

## Version-aligned with the terminal sibling

wireguard-gui has a sibling, **[wireguard-tui](https://github.com/JamilleJung/wireguard-tui)**
(binary `wg-tui`) - the same tool as a keyboard-driven terminal UI for SSH and
headless servers. The two apps are deliberately kept **feature-identical and
version-aligned**: a given version number means the same capabilities and the
same fixes in both. When you read a version below, the matching `wg-tui` release
covers the same ground (with the obvious interface differences - menus and a tray
on the GUI side, key bindings on the TUI side). The current version of both is
**1.5.4**.

---

## How releases are built

There are two GitHub Actions workflows.

**Continuous integration (every push / pull request).** Before anything can be
released, the code has to pass:

- `cargo fmt --check` - formatting is enforced, not suggested.
- `cargo clippy` - lint warnings are treated as failures.
- `cargo test` - unit tests run on every push and pull request.
- a release **build** of the binary.
- a **smoke test** that `wireguard-gui --version` and `--help` start and exit
  cleanly without opening a window.
- `shellcheck` on the privileged helper (`wg-helper`) and on `install.sh` - CI
  **hard-fails on shellcheck warnings**, because those scripts run as root.
- negative helper tests that prove traversal-style tunnel names are rejected
  before any filesystem access.

**Tag-triggered release.** Pushing a version tag (for example `v1.5.4`) kicks off
the release workflow, which runs the same smoke and shell validation, builds the
`.deb`, the AppImage and the portable tarball, generates `SHA256SUMS`, signs it with minisign to produce
`SHA256SUMS.minisig`, attaches `minisign.pub`, and publishes a GitHub Release.
Supply-chain hygiene is built in: third-party GitHub Actions are pinned to commit
SHAs, the AppImage tool (`linuxdeploy`) is pinned to a release with a verified
SHA-256, and the job aborts if the `.deb` or checksums are missing.

---

## Version history

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

## Pre-1.0 history (for completeness)

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

## Versioning policy

wireguard-gui follows [Semantic Versioning](https://semver.org/spec/v2.0.0.html)
(`MAJOR.MINOR.PATCH`):

- **MAJOR** - incompatible changes to how you interact with the app or its files.
- **MINOR** - new, backward-compatible features (for example 1.4.0's live
  throughput, or 1.5.0's Setup wizard).
- **PATCH** - backward-compatible bug fixes and hardening (for example the
  1.5.1-1.5.4 install fixes).

**GUI and TUI move in lockstep.** wireguard-gui and its terminal sibling
`wireguard-tui` share a version number on purpose: the same number means the same
feature set and the same fixes in both apps, differing only where the interface
itself differs (windows, menus and a tray icon here; key bindings and the
`wg-tui doctor`/`setup` subcommands there). When one app releases a version, the
other releases the matching version. Tags are `v<version>` (for example
`v1.5.4`), and the changelog ([CHANGELOG.md](../CHANGELOG.md)) is the
authoritative per-version record.
