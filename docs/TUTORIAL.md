# 📖 wireguard-gui tutorial

This guide follows the current 1.9.2 application behavior: installing the desktop
client, importing or creating a tunnel, connecting, editing, backing up and
updating. It describes the GUI; the terminal sibling has its own controls and
CLI. Repository guide corrections do not change Help embedded in an already
published binary.

## 1. Before you start

You need a Linux graphical session, WireGuard tools (`wg`, `wg-quick`), suitable
kernel/network support and a config from a provider or your own server. This app
does not provide a VPN account or register a generated key on a remote server.
For upstream concepts and platform installation, see the
[WireGuard quick start](https://www.wireguard.com/quickstart/) and
[installation guide](https://www.wireguard.com/install/).

Treat `.conf` files as secrets: they usually contain your private key and may
also contain a preshared key. The same applies to QR codes, ZIP exports,
backups and full-config clipboard copies.

## 2. Choose an installation method

Get the current [1.9.2 release](https://github.com/JamilleJung/wireguard-gui/releases/tag/v1.9.2)
and [verify its signed checksum](../README.md#verify-a-download) before using a
binary package. The release offers x86_64 `.deb`/AppImage and x86_64/ARM64 Linux
tarballs. Choose the correct CPU and a compatible Linux runtime; tarballs are
not universal across every libc/distribution.

### Debian-family x86_64 package

```sh
sudo apt install ./wireguard-gui_1.9.2-1_amd64.deb
wireguard-gui
```

The package installs the application, helper, desktop assets and a polkit rule.
It does not enroll a user in the `wireguard` group. For a trusted local desktop
user, an administrator can grant membership:

```sh
sudo groupadd -f wireguard
sudo usermod -aG wireguard "$USER"
```

Log out and back in. The rule needs both group membership and an active local
session. It is not a passwordless grant for SSH sessions or every local user.
Group members can read keys and manage all tunnels.

### Extracted binary bundle

```sh
tar -xzf wireguard-gui-1.9.2-x86_64-linux.tar.gz
cd wireguard-gui-1.9.2-x86_64-linux
./install.sh
```

Use the ARM64 filename/directory on ARM64. The installer recognizes the adjacent
prebuilt application/helper, retains the supplied `packaging/` assets, stages
executable binaries, and skips Rust, compilers and development headers. Runtime
dependencies and authorization still need installation.

### Source checkout

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh
```

A source checkout has Cargo.toml, so the installer installs build dependencies
and builds from source. Use a normal user; the script escalates where needed.
Default installation is under `/usr/local` with a per-user helper sudoers grant.
`./install.sh --polkit` instead attempts group creation/user enrollment and
installs a group-restricted rule. A direct root login needs `--allow-root` or
`WG_ALLOW_ROOT=1`; only choose that deliberately.

The installer creates the optional `wg-gui` symlink. Launch `wireguard-gui` for
Cargo/native-package builds that do not provide this alias. The AppImage can
launch the UI but does not itself enroll your user for helper authorization;
use native/helper integration for routine privileged operations.

To uninstall a source/bundle install, run `./install.sh uninstall`. It leaves
saved tunnels under `/etc/wireguard`. Remove package-manager installations with
their package manager, and avoid mixing `/usr` and `/usr/local` installations.

## 3. Launch and resolve prerequisites

Launch from the desktop menu or `wireguard-gui`. Launching again raises the
existing per-user window. `--help`/`-h` and `--version`/`-V` print without opening
a window; this GUI has no `doctor`, `setup` or tunnel-management CLI commands.

If critical requirements are missing, a Setup window appears at startup. It
checks tools, known helper/rule paths, DNS compatibility, clock synchronization,
config-directory presence and systemd/journal availability. The helper probe
uses the backend-selected trusted executable. Polkit rule presence is an
unverified authorization warning, not proof your session/group matches it.
It does not check kernel-module availability.

- **Show commands** displays suggested fixes.
- **Fix automatically** uses `pkexec` for selected prerequisite commands. It may
  install WireGuard tools/a resolvconf provider, enable a detected inactive
  systemd-resolved service, or create `/etc/wireguard` with mode 0700.
- **Re-check** reruns the checks; **Skip for now** opens the main window.

Automatic fixes do not install/authorize the helper, import configs, connect a
tunnel, or enable boot-time tunnels. Install the helper through your chosen
installer/package path. There is no separate Setup entry in the main menu;
critical checks run again on the next launch.

## 4. Import a config or QR image

Imported config files must be regular files at most 1 MiB. Named pipes and
other special files are rejected without waiting for a writer.

Open **Add Tunnel → Import tunnel(s) from file…** and select your `.conf`.
A single file opens in the editor for review with a suggested unique name.
Multiple files are validated/imported separately; valid files receive unique
names and failures are skipped rather than overwriting existing tunnels.

For QR import, choose **Import from QR code…** and a PNG/JPEG image. This reads
an image file, not a camera. All detected grids are considered: broken/unrelated codes are skipped, one
valid distinct config opens for review, and multiple different tunnel configs
are refused. Limits are 32 MiB per image, 4096 × 4096 pixels and a configured
128 MiB decoder-allocation budget. A QR must contain WireGuard config text, not
just a provider webpage.

Tunnel Name becomes `/etc/wireguard/<name>.conf` and the Linux interface name.
Use 1–15 ASCII letters/digits/`_`/`.`/`-`, starting with a letter or digit. Do not
include `..`, path separators or the `.conf` suffix. Import suggestions sanitize
filenames; you can choose a valid name before creating a single import.

Review private/peer keys, addresses, AllowedIPs and endpoint. Inline `#` comments
are accepted. The helper blocks `PreUp`, `PostUp`, `PreDown` and `PostDown` on
save/rename; an editor warning does not make those script hooks saveable.

Choose **Create** to save, or **Create & Activate** to save then try activation.
If activation fails after creation, the saved config remains available to edit.

## 5. Create a tunnel and use the editor

**Add Tunnel → Create tunnel…** opens the Full tunnel preset with a generated
keypair. The preset buttons regenerate keys and replace the draft; choose a
preset before entering your real settings:

| Preset | Starting values to review |
|---|---|
| Interface only | Address `10.0.0.2/32`, no peer |
| Full tunnel | Address `10.0.0.2/32`, DNS `1.1.1.1`, AllowedIPs `0.0.0.0/0, ::/0`, example endpoint and keepalive 25 |
| Split tunnel | Address `10.0.0.2/32`, AllowedIPs `10.0.0.0/24`, example endpoint and keepalive 25 |

These values are templates, not provisioned VPN access. Supply the real peer
public key/endpoint and server-assigned address, and register your public key
at the other end. If key generation fails, obtain a valid key before saving.

**Edit fields** contains Private key, Address, DNS and Peer fields. **Advanced
fields** reveals Listen port and MTU. Navigate with **Prev / Next**, use **Add
peer**, and remove an extra peer with **Remove**. **Generate keypair** changes
your identity; **Generate PSK** changes a shared secret, which must also be set
on the other end. **Copy** by Public key copies the derived public key;
**Copy config** includes secrets.

Existing tunnels open as **Config text** so opening alone does not rewrite
them. Switch to **Edit fields** only when the form can preserve the config.
Comments, repeated keys, Table, SaveConfig, unknown sections/fields and
interface-only configs stay in raw text. Do not remove useful config content
just to force form mode.

For an existing tunnel, **Save** validates and writes the config. Changing Name
renames it; the helper stops the old interface if active and carries its systemd
enablement to the new name where possible. It does not reconnect the renamed
interface automatically.

Saving an active tunnel attempts `wg syncconf` for WireGuard settings. Address,
DNS, MTU, Table and route changes still need **Deactivate**, then **Activate**,
even if the status reports that a compatible live sync succeeded.

Config validation is shared by the frontend and privileged helper. It checks
every directive occurrence, known sections/field placement, canonical 32-byte
WireGuard base64 keys (including padding bits), numeric CIDR/port/keepalive
bounds, and required peer fields. Inline comments, repeated Interface sections,
repeated Address/DNS/AllowedIPs, scoped IPv6 endpoints, named/numeric Table,
FwMark hex/off and SaveConfig booleans are supported. Unknown fields or malformed
sections stay visible in raw text but cannot be saved until corrected. If a
PresharedKey directive is present it must contain a valid key; omit it to use
no PSK. The helper independently rejects all four root script-hook directives.

Save/import/connect/backup work is dispatched off the event loop; conflicting
operations are rejected while busy. Background results preserve newer drafts
and selections. A setup/authentication error is surfaced rather than replacing
a failed listing with an empty set of tunnels.

## 6. Connect and read status

Select a saved tunnel, then **Activate**. The app invokes `wg-quick up`; it
creates/configures the interface and uses the system's routes/DNS integration.
**Deactivate** invokes the down operation and attempts kill-switch cleanup.

The interface can be active before a peer handshakes. Recent-handshake health
uses a three-minute threshold; an idle peer can become stale even when a later
packet would reconnect normally. Transfer totals and current throughput describe
observed traffic, not a guarantee that every destination is reachable.

Click **Diagnose** for clock-sync status, interface/handshake state, an endpoint
ping and basic DNS. It tests the first available peer endpoint rather than every
peer. ICMP can be blocked while WireGuard works; DNS success alone does not
prove the tunnel's routes or resolver policy. Use the report with server-side
peer registration, endpoint, firewall and routing checks. Copy one report line
or **Copy all** when sharing redacted troubleshooting information.

## 7. Boot, advanced actions and tray

**Start on boot** enables/disables systemd `wg-quick@<name>`. It is not a desktop
login setting or a promise that a currently active tunnel survives a restart.
On non-systemd systems, configure boot activation through the distro's service
manager instead.

Expand **Advanced ▾** on the tunnel card for **Kill switch**, **Running cfg** and
**Save live**. There is no global Easy/Advanced mode; the disclosure preference
is remembered across launches.

- **Running cfg** copies `wg showconf` output, including private keys, rather
  than displaying the full wg-quick file with Address/DNS comments.
- **Save live** backs up and invokes `wg-quick save`. It can replace hand-written
  file formatting/settings with the runtime-derived form; use it deliberately.
- **Kill switch** uses the audited policy below.

Only **one helper-managed kill switch** can be active at a time across managed
tunnels. Turn the existing one off before enabling another. The helper requires
an already-active interface; it can assign an unused runtime WireGuard fwmark
without changing the saved config. It does not activate a stopped tunnel as a
side effect of enabling a switch.

The helper prefers nftables and requires a real hooked filter OUTPUT base chain.
It applies its complete rule set as one nft batch. The fallback requires
`iptables` and `iptables-restore`; when the kernel exposes IPv6 support it also
requires `ip6tables` and `ip6tables-restore`, even before a non-loopback IPv6
address is assigned. Both families are preflighted; commits are atomic **per
family**, not one transaction across IPv4 and IPv6. Failures are reported.

The rules allow loopback, the selected tunnel, its marked transport traffic,
and detected established SSH return traffic when `SSH_CONNECTION` reaches the
helper, then reject other host output ahead of existing allows. Exact comments
isolate tunnel names; status requires the complete expected rule set. This is
host-output control, not server forwarding/NAT or a general no-leak guarantee.
It may disrupt remote access or interact with another firewall manager.

Down/delete/rename retain protection if the interface is still active or its
state is unknown, and report stop/cleanup failures. A failed operation can
leave a tunnel stopped or partially changed; inspect the error and current
state before retrying. App exit does not clear rules. External service changes
may leave stale rules, and firewall reloads may remove them. No persistence
daemon is installed. After upgrading, reapply an existing switch (off/on) to
install current rules, planning for that brief unprotected interval and keeping
another administrative access path available.

With a working StatusNotifier/AppIndicator host, closing the window hides it;
the tray offers Show WireGuard, per-tunnel toggles, Deactivate all and Quit.
Without a tray host, closing quits. GNOME/KDE and other desktops differ in tray
support; the app does not install a desktop extension.

## 8. Logs, export and backup

**Log** fetches helper and `wg-quick@*` journal entries at startup, when opened,
and on **Refresh**. **Filter** and **This tunnel** affect the displayed rows.
Click a row to copy it; **Copy** and **Save** use the full loaded, unfiltered
log. **Clear** clears the in-app buffer, not the system journal. No periodic
log refresh replaces the text while you read it.

The bottom export-all icon writes every saved tunnel to a ZIP destination you
choose, or reports a read/list error instead of silently omitting tunnels.
**Backup → New backup** creates an archive in the shared user backup directory;
select one to **Restore**, **Export** or **Delete**. There is no external ZIP
restore picker.

User backups live under `$XDG_DATA_HOME/wireguard/backups` (an absolute XDG
path), or `~/.local/share/wireguard/backups`, shared by both applications.
Existing managed backup directories must belong to the current user and are
checked/tightened to `0700`; symlink directories are refused. ZIPs and exported
copies use `0600`. New backup names are unique even within one second and are
published without overwriting another archive.

Export reads every listed tunnel successfully or reports an error. It no longer
reports a successful incomplete archive after a helper/read failure. Exports
and saved logs are written to a completed private sibling file and then
published atomically: failed writes preserve the old destination, final
symlinks are refused, and replacing a hardlink does not modify its other names.

Restore accepts only valid, unique, top-level `.conf` entries. It rejects
malformed/duplicate archive names, invalid configs and hooks before its first
save. Limits are **64 MiB per ZIP**, **4,096 archive entries**, **1,024 configs**,
**1 MiB per config**, and **16 MiB total config text**. Unrelated files are
skipped. Backup reads require an owned regular file with one hard link and
refuse symlinks/FIFOs; copy a hardlinked archive to an ordinary file first.

After complete preflight, restore still saves sequentially. It overwrites
matching saved names, leaves other tunnels in place, and does not sync running
interfaces. A later operational failure can leave a partial restore; it is not
a multi-file transaction. Helper per-file backups remain a separate recovery
source under `/etc/wireguard/.backup/`. All these archives contain unencrypted
private keys.

## Updates

Open the menu button beside **Add Tunnel**, then **Check for updates…**. The
Updates window displays the installed version and the changelog embedded at
build time, checks GitHub immediately, and has **Check for updates** and
**Update now** actions. The startup background check shows a banner when a
newer release is found; offline startup failures are silent.

For package-managed installs, normally use the same package manager to upgrade.
The in-app updater is not package-aware: it downloads the architecture-specific
signed tarball and runs its `install.sh` with its normal `/usr/local` and
sudoers defaults. It does not update a package database or replace an AppImage
file in place, and can create a second installation next to a `/usr` package.

The update path needs `curl`, `tar`, `sha256sum`, `minisign` and, for a normal
GUI session, `pkexec`. Installing minisign automatically is only best-effort
with root/non-interactive sudo on supported managers; install it explicitly
when necessary. An administrator authentication dialog is expected for applying
the verified installer. No install occurs if signature/checksum verification
fails. Restart the application after installation.

To disable startup update checks:

```sh
WG_NO_UPDATE_CHECK=1 wireguard-gui
```

Or create the persistent opt-out file:

```sh
mkdir -p "${XDG_CONFIG_HOME:-$HOME/.config}/wireguard-gui"
touch "${XDG_CONFIG_HOME:-$HOME/.config}/wireguard-gui/no-update"
```

Opening Updates still performs a manual GitHub check. Update checks do not
upload tunnel configs, but they are network requests. Diagnostics also sends
network probes; QR/export/copy actions deliberately expose config data locally.

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

Verified updates retain ownership of their private download directory through
installation and clean it up on download, staging or installation failure and
normal completion. Before privilege escalation, the whole required bundle is
checked: regular single-link application/helper/installer/assets, no symlink
components, and a fresh exclusive staging tree. Missing/unsafe assets fail before
installation starts. HTTPS-only redirects are enforced. GUI automatic updates
support its published x86_64/ARM64 builds; unsupported CPUs fail explicitly.

The download step refuses a release that is no longer newer; success reports
the version actually verified. Restart to use the installed version.

## Troubleshooting and further reading

- DNS activation requires a working `resolvconf` command; the resolvectl shim
  additionally requires active systemd-resolved. See [distro notes](DISTROS.md).
- If helper access prompts/fails after a package install, check group/session
  membership and the exact installed path, not only Setup's rule-presence hint.
- Keep NetworkManager and other tools from managing the same interface at once.
- Historical [screenshots](../README.md#screenshots) show earlier layouts. Keys
  visible in published screenshots are public; never reuse them.
- Read [security](../SECURITY.md), [packaging](../packaging/PACKAGING.md),
  [release information](RELEASES.md) and the [changelog](../CHANGELOG.md).
