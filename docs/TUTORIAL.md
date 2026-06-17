# wireguard-gui tutorial - from zero to a working VPN

This is a step-by-step guide to `wireguard-gui`, a desktop app for managing
WireGuard tunnels on Linux. It walks you through installing it, getting your
first tunnel in, connecting, reading the live status, putting a tunnel on your
phone, and the small print (start-on-boot, editing, renaming, exporting,
troubleshooting). No prior WireGuard experience is assumed.

Tested against version 1.5.4.

---

## 1. What this is and who it's for

`wireguard-gui` is a native Linux GUI (built in Rust with the Slint toolkit) for
plain WireGuard configs: a tunnel list on the left, an Interface/Peer detail
pane on the right, and direct Activate/Deactivate actions. It drives the
standard `wg-quick` path - your tunnels are plain `.conf` files in
`/etc/wireguard` - and deliberately bypasses NetworkManager.

It is for anyone who wants to use WireGuard on a Linux desktop without typing
`wg-quick` commands by hand. The overwhelming majority of users are **clients**:
you dial out to a VPN provider or to your own server. If that is you, you do not
touch the firewall and you do not enable IP forwarding.

> **Client or server? Firewall?** Whether you need a firewall port, IP
> forwarding or NAT depends entirely on whether your machine is a client (dials
> out) or a server/gateway (accepts incoming peers). The full answer, per distro,
> is in [docs/DISTROS.md](DISTROS.md). Read it if you are setting up a server; a
> plain client can skip it.

Prefer the terminal, or working over SSH on a headless box? The sibling
[wireguard-tui](https://github.com/JamilleJung/wireguard-tui) is the same tool
as a keyboard-driven terminal UI.

---

## 2. Requirements and install

### The one-command install

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh
```

Or as a single line:

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git && cd wireguard-gui && ./install.sh
```

### What the installer does

`./install.sh` is a universal installer. It:

1. **Detects your package manager** (`apt`, `dnf`/`yum`, `pacman`, `zypper`,
   `apk`, `xbps`, `eopkg`) and **auto-installs every missing dependency** - the C
   toolchain, `pkg-config`, the libraries Slint needs (`fontconfig`,
   `libxkbcommon`), an OpenGL runtime, and **`wireguard-tools`** itself.
2. **Installs a Rust toolchain** automatically (via [rustup](https://rustup.rs))
   if `cargo` is not already present.
3. **Builds** the release binary - as the invoking user, never as root, so build
   scripts never run with root privilege.
4. **Installs** the binary (`wireguard-gui`), the privileged helper (`wg-helper`),
   a `.desktop` launcher and an icon.
5. **Sets up passwordless privileged access** so the app never asks for your
   password at runtime (details below).

On an unrecognised distro it tells you exactly which packages to install
manually, then still builds and installs.

When it finishes, launch **WireGuard** from your application menu, or run:

```sh
wireguard-gui
```

### The no-sudo / passwordless explanation

WireGuard needs root to read `/etc/wireguard`, run `wg show`, and bring tunnels
up and down. So that the app never nags you for a password on every status poll,
all root actions go through **one small, auditable script, `wg-helper`**
(installed at `/usr/local/lib/wireguard-gui/wg-helper`). The installer
authorises **only that script** to run without a password:

- **Default:** a `sudoers` drop-in scoped to the helper.
- **`pkexec` fallback:** if no passwordless path is set up, the app falls back to
  `pkexec`, which prompts.

The installer copes even with awkward setups:

- **`sudo` present but you are not a sudoer** (a Debian box where your login user
  is not in the sudoers file): it detects that, falls back to the ROOT password
  via `su`, and switches the helper authorization to a polkit rule.
- **No usable `sudo` at all:** it re-runs itself as root with a **single**
  ROOT-password prompt, installs `sudo` if it is missing, and writes a
  passwordless drop-in scoped to the helper for *your* user - so the app works as
  a normal user even though you are not in the `sudo` group.

If you want to be prompted once instead of per step, run `su -` first, then
`./install.sh`.

### Choosing the auth backend: sudoers (default) or polkit

```sh
./install.sh            # sudoers drop-in (light and fast - the default)
./install.sh --polkit   # polkit rule instead (cleaner desktop integration)
```

Both make privileged tunnel control passwordless for your local session.
`sudoers` is simplest; `polkit` is the more "native desktop app" path (and is
what the prebuilt `.deb` uses automatically).

### Uninstall

```sh
./install.sh uninstall
```

(See section 12 for what this removes.)

> The app **does not bundle WireGuard**: no kernel modules, no vendored
> `wg`/`wg-quick`. It uses your distro's `wireguard-tools` (and helps install
> them). WireGuard has been in the mainline Linux kernel since 5.6 (early 2020),
> so on any current kernel there is no module to install.

---

## 3. First run

The first time you launch the app it runs a quick, **read-only** system check
(WireGuard tools, the privileged helper and its authorization, `/etc/wireguard`,
DNS/resolvconf, systemd, journald). It never connects tunnels, enables
start-on-boot, or touches existing configs.

- If everything critical is OK, the app goes straight to the main window in
  **Easy mode**.
- If something critical is missing, a friendly **Setup window** appears first and
  explains, in plain language, what is missing.

### The Setup window buttons

| Button | What it does |
|--------|--------------|
| **Fix automatically** | Installs the safely-automatable pieces - `wireguard-tools`, a resolvconf provider for `DNS =`, and `/etc/wireguard` - via `pkexec` (you enter your password in the dialog that appears). It never installs the helper or touches your configs. |
| **Show commands** | Reveals the exact commands for each item that is not OK, so you can run them by hand. |
| **Re-check** | Re-runs the system check. If everything critical now passes, the window closes and the main window opens. |
| **Skip for now** | Closes the wizard and opens the main window anyway. |

Note: **Fix automatically** does not install the privileged helper itself. If the
helper is what is missing, install it via the `.deb`, the AUR package, or
`./install.sh`, then click **Re-check**.

### Easy vs Advanced mode

The app opens in **Easy mode** (the default for new users). Easy mode keeps the
everyday surface and hides expert tools. The toggle is in the **bottom action
bar, next to Edit** - one click switches to **Advanced mode**, and your choice is
remembered across runs (saved to `~/.config/wireguard-gui/mode`).

| | Easy mode (default) | Advanced mode adds |
|---|---|---|
| Available | Activate/Deactivate, Add (Import .conf / Import QR), Edit, Remove, Show QR, Start on boot | Add empty tunnel (new from scratch), Generate keypair/PSK in the editor, Running cfg (copy live config), Save live, Export all to zip |

With **no tunnels yet**, the app shows a friendly "No tunnels yet" empty state
with **Import .conf** and **Import QR image** buttons (plus **New tunnel** in
Advanced mode) rather than a blank pane.

---

## 4. Get a tunnel in three ways

All three start from the **Add Tunnel** dropdown menu in the toolbar, and all
three end up at the same inline editor where you confirm and **Save**.

### (a) Import an existing provider `.conf`

If your VPN provider gave you a `.conf` file (or you exported one from another
device), this is the quickest path.

1. Click **Add Tunnel** -> **Import from file...**
2. Pick one (or several) `.conf` files in the file dialog.
3. **One file:** the editor opens pre-filled, with a suggested, collision-free
   tunnel name you can change. Review it, then click **Save**. (Importing one
   file at a time lets you **name the tunnel yourself**.)
4. **Multiple files:** they are imported in bulk with auto-deduplicated names
   (an import never overwrites an existing tunnel). Invalid files are skipped and
   reported; any config that runs root scripts on activation is flagged.

### (b) Import from a QR-code image

Handy when a provider or another device shows you a tunnel as a QR code and you
have a screenshot or photo of it.

1. Click **Add Tunnel** -> **Import from QR code...**
2. Pick a `.png`, `.jpg`, or `.jpeg` image of the QR code.
3. The app decodes it and opens the editor pre-filled with a suggested name.
   Review it, then click **Save**.

### (c) Create a new tunnel from scratch and generate keys

For building your own client config by hand. (This is **Advanced mode** - switch
with the toggle next to Edit if you do not see it.)

1. Click **Add Tunnel** -> **Add empty tunnel...**
2. The editor opens already populated with a starter template **and a freshly
   generated private key** - the public key shown live next to it is the one you
   give to your server/peer.
3. Fill in the rest (your `Address`, the peer's `PublicKey`, `AllowedIPs`,
   `Endpoint`, etc.). To regenerate keys at any time:
   - **Generate keypair** - inserts a new `PrivateKey` and updates the live public
     key.
   - **Generate PSK** - inserts a `PresharedKey` for the peer (you must have a
     `[Peer]` section first).
4. Give the tunnel a name and click **Save**. The config is **validated** before
   it is written (see section 8).

---

## 5. Connect and disconnect; reading status

1. Click a tunnel in the left-hand list to select it. Its Interface and Peer
   details, and live status, appear on the right.
2. **Activate** with the button in the Interface card (this runs `wg-quick up`).
   The list's dot turns **green** and the Interface card shows live status.
3. **Deactivate** with the same button (this runs `wg-quick down`). The dot turns
   **grey**.

You can also activate/deactivate per tunnel from the system-tray icon (see
section 10).

### Reading the status

The detail pane is polled live (about once per second, on a background thread so
the UI never stutters):

| Field | Where | What it means |
|-------|-------|---------------|
| **Active dot** | Tunnel list | Green = up, grey = down. |
| **Status / public key / listen port / addresses / DNS** | Interface card | The interface side of the tunnel. |
| **Latest handshake** | Peer card | How long ago the peer last completed a handshake. A recent handshake means the link is alive. |
| **Transfer** | Peer card | Total bytes received and sent over this peer. |
| **Live speed** | Interface card | Real-time throughput, shown as `down <rate>/s   up <rate>/s`, derived from successive samples. |
| **Connection health** | Interface card | A handshake-based summary: `OK (last handshake ... ago)` when the last handshake is under 3 minutes old, `stale (last handshake ... ago)` when older, or `waiting for handshake...` before the first one. |

If an activation fails, the app explains the cause in plain language (for
example a missing resolvconf provider) instead of dumping the raw `wg-quick`
output.

---

## 6. Put a tunnel on your phone with Show QR

To use the same tunnel in the WireGuard app on your phone, display it as a QR
code and scan it.

1. Select the tunnel in the list.
2. Click **Show QR**.
3. In the WireGuard mobile app, choose to add a tunnel from a QR code and point
   your camera at the screen.

> **Warning - the QR code contains the tunnel's private key.** Anyone who
> photographs it can use your tunnel. Only show a QR when it is safe for people
> nearby to see your screen, and never post a screenshot of it. The same applies
> to exported `.zip` archives (section 9) - keep them somewhere safe.

---

## 7. Start-on-boot

To have a tunnel come up automatically when the machine boots:

1. Select the tunnel.
2. Toggle **Start on boot** in the detail pane.

This enables (or disables) the systemd unit `wg-quick@<name>`. It is the one
piece of runtime state that survives a reboot - your `.conf` files and packages
are permanent on disk, but a tunnel only auto-connects if you turn this on.

> **Non-systemd limitation:** Start-on-boot uses systemd. On OpenRC (Alpine),
> runit (Void), and other non-systemd init systems, this one feature is
> unavailable - the toggle will show "off" and have no effect. Everything else
> works. On those systems, enable boot-time tunnels with the distro's own service
> manager (for example an OpenRC init script or a runit service that runs
> `wg-quick up <name>`). See [docs/DISTROS.md](DISTROS.md) for specifics.

---

## 8. Edit safely

Click **Edit** (bottom action bar) on a selected tunnel to open the inline
editor.

- **Validation before save.** When you click **Save**, the app validates the
  config first - keys, addresses, `AllowedIPs` (parsed as real IP/CIDR),
  endpoints (including bracketed IPv6 such as `[2001:db8::1]:51820`). If anything
  is wrong, the editor shows the error and does **not** write the file, so typos
  are caught here instead of at activation time. The helper also performs a
  second basic config-shape check before replacing files.
- **Live apply to a running tunnel.** If the tunnel is currently up, saving
  applies the change live with `wg syncconf` - peer sessions are **not** dropped.
  The status line confirms `Saved <name> (applied live)`. Some fields
  (`Address`, `DNS`, `MTU`, routes/`Table`) are wg-quick-only and cannot be synced
  live; in that case the app saves the file and tells you to reconnect to apply
  them.
- **Backups.** Every save (overwrite), rename, and delete first copies the
  current config to `/etc/wireguard/.backup/<name>.conf.<timestamp>` (mode 600),
  and the action is recorded in the audit log.
- **Script warning.** If a config contains `PostUp`/`PreUp`/`PostDown`/`PreDown`
  (commands `wg-quick` runs as root on activation), the editor shows an amber
  warning. Only save it if you trust the source.

---

## 9. Rename, remove, export

### Rename

Open **Edit**, change the **tunnel name** field, and **Save**. The app writes the
new name and removes the old one (deactivating it first if it was up). Names are
sanitised to a helper-valid form - one leading letter or digit, then up to 14
more characters from `[A-Za-z0-9_.-]` (15 total), matching the helper's
`^[A-Za-z0-9][A-Za-z0-9_.-]{0,14}$`; a name that already exists is rejected.

### Remove

Select the tunnel and click the **Remove** button (the X icon) in the bottom
action bar. The delete is **reversible**: the current config is backed up to
`/etc/wireguard/.backup/` (timestamped, mode 600) before it is removed.

### Export

In **Advanced mode**, click the **Export** button (the download icon) in the
bottom action bar
to write **all** tunnels to a `.zip` archive (you choose the path; the default
file name is `wireguard-tunnels.zip`).

> **Warning:** the exported `.zip` contains the tunnels' **private keys**. Store
> it somewhere safe and delete copies you no longer need.

---

## 10. Full reference

### Toolbar and actions

| Control | Where | What it does | Mode |
|---------|-------|--------------|------|
| **Add Tunnel** (dropdown) | Top toolbar | Menu: *Import from file...*, *Import from QR code...*, *Add empty tunnel...* (Advanced), *About...* | All |
| Tunnel list | Left pane | Click to select; live green/grey active dot | All |
| **Activate / Deactivate** | Interface card | `wg-quick up` / `down` for the selected tunnel | All |
| **Show QR** | Detail pane | Display the tunnel as a QR code for the mobile app | All |
| **Start on boot** | Detail pane | Toggle the `wg-quick@<name>` systemd unit | All |
| **Copy** (public key) | Detail pane | Copy the interface public key to the clipboard | All |
| **Edit** | Bottom action bar | Open the inline editor (form or config text) | All |
| **Easy / Advanced** toggle | Bottom action bar, next to Edit | Switch between Easy and Advanced; remembered across runs | All |
| **Remove** (X icon) | Bottom action bar | Delete the selected tunnel (backed up first) | All |
| **Export** (download icon) | Bottom action bar | Export all tunnels to a `.zip` | Advanced |
| **Running cfg** | Detail pane | Copy the live running config (`wg showconf`) to the clipboard | Advanced |
| **Save live** | Detail pane | Write the running state back to the `.conf` (`wg-quick save`) | Advanced |
| **Tunnels / Log** tabs | Top of the window | Switch between the tunnel view and the activity log (`journalctl -t wireguard-gui` plus `wg-quick` entries); the Log tab has a Refresh button | All |
| System-tray icon | Desktop tray | Per-tunnel activate/deactivate, a connection-status header, a *Tunnels* submenu, *Deactivate all*, *Show WireGuard*, and *Quit*; tooltip shows active tunnels and live throughput. Closing the window minimizes to the tray so tunnels keep running. | Where the desktop supports the tray |

> Tray note: the tray uses the StatusNotifierItem standard. It appears on KDE and
> most trays out of the box; on **GNOME it needs the AppIndicator extension**. If
> there is no tray host, closing the window quits the app (so it is never stranded
> with no window and no way to quit).

### Inside the editor: the Form <-> Config-text toggle

The editor has two views, and a toggle to switch between them:

- **Edit fields (form view)** - labelled Interface fields (Private key, Address,
  DNS, Listen port, MTU) and a single Peer (Public key, Preshared key, Allowed
  IPs, Endpoint, Persistent keepalive).
- **Config text** - the raw `.conf` text.

New tunnels open in the **form**; existing tunnels open in **config text** (so a
hand-tuned config is never silently rewritten on open). Edits in either view stay
in sync, and **Generate keypair** / **Generate PSK** update both.

The form only maps a single peer and a fixed set of keys. For configs it cannot
faithfully represent - a **second `[Peer]`**, `PostUp`/`PreUp`/`PostDown`/`PreDown`,
`Table`, `SaveConfig`, or any other unmapped key - the editor stays in **config
text** and refuses to switch to the form, telling you why, so it never drops the
parts it cannot show. Edit those configs as raw text.

Other editor buttons: **Copy config** (copy the whole config to the clipboard),
**Generate keypair**, **Generate PSK**, **Save** (validate then write), and
**Cancel**.

### Command-line interface

The GUI has no subcommands - it is launched as a windowed app. The only CLI is:

```sh
wireguard-gui            # launch the app
wireguard-gui --version  # print the version and exit
wireguard-gui --help     # show usage and exit
```

(There is no `doctor` or `setup` subcommand; that is the terminal sibling
`wg-tui`. In the GUI, the first-run Setup window plays that role.)

---

## 11. Troubleshooting

### "resolvconf: command not found" when activating (DNS = gotcha)

If your tunnel's `[Interface]` has a `DNS =` line, `wg-quick` calls `resolvconf`
to apply it. On minimal Debian (and bare Arch/Alpine/Void) there is no
`resolvconf` binary, so activation aborts with something like:

```
/usr/bin/wg-quick: line 32: resolvconf: command not found
```

Install a provider once:

```sh
sudo apt install openresolv        # Debian/Arch/etc.
# or, with no sudo (Debian-minimal):
su root -c 'apt install openresolv'
```

If **systemd-resolved** is active (the default on Ubuntu and Fedora), it already
satisfies this and you need nothing. The first-run Setup window's **Fix
automatically** also installs a provider for you. Full per-distro detail:
[docs/DISTROS.md](DISTROS.md).

### "<user> is not in the sudoers file"

This appears when you are not a sudoer. The installer (1.5.2 and later) handles
it: it detects that `sudo` is unusable, falls back to the ROOT password via `su`,
and switches the helper authorization to a polkit rule. If you hit this during
install, re-run `./install.sh` (or run `su -` first, then `./install.sh`, to be
prompted once).

### Helper authorization / pkexec prompts every time

If the app prompts for a password on every action, the passwordless drop-in was
not set up. Re-create it:

```sh
./install.sh            # re-create the sudoers drop-in
./install.sh --polkit   # ...or use a polkit rule instead
```

If the installer reported "sudoers validation failed" or said a present
`openresolv` was missing, update to **1.5.4** - that release puts `/usr/sbin` and
`/sbin` on `PATH` after the root re-exec so it can find `visudo` and the
`resolvconf` probe. The hint when the drop-in is skipped is to re-run
`./install.sh --polkit`.

### Cryptic spawn error / cannot gain root

A `spawn failed: No such file or directory` (older builds) means the app could
**not escalate to root** - there is no passwordless sudo and no `pkexec`. Fix it
by either:

- running the app as root (then it talks to the helper directly), or
- re-running `./install.sh`, which sets up a passwordless path (sudoers or polkit)
  and, if `sudo` is unusable, installs `sudo` and writes a per-user drop-in.

Newer builds (1.5.3+) replace that cryptic message with a clear, actionable one.

### A tunnel will not activate

Open **Edit** - the validator flags bad keys, addresses or endpoints. Also
confirm it works in a terminal:

```sh
wg-quick up <name>
```

Check the audit log for what happened:

```sh
journalctl -t wireguard-gui
```

### Blank window / no GPU

Make sure an OpenGL runtime is installed (the installer handles this). On
headless or odd setups, try the software backend:

```sh
SLINT_BACKEND=winit-software wireguard-gui
```

### Still stuck on a specific distro?

[docs/DISTROS.md](DISTROS.md) covers, per distro, what to install, what to set
up, what survives a reboot, the init-system limitation, and (only for a
server/gateway) firewall and IP-forwarding changes.

---

## 12. Uninstall

```sh
cd wireguard-gui
./install.sh uninstall
```

This removes the installed binary, the `wg-helper` privileged helper, the
`.desktop` launcher and icon, and the passwordless authorization (the sudoers
drop-in or polkit rule) that `install.sh` added.

Your tunnel `.conf` files in `/etc/wireguard` (and their timestamped backups in
`/etc/wireguard/.backup/`) are **not** touched - remove those by hand if you want
them gone:

```sh
sudo rm -rf /etc/wireguard        # deletes all tunnel configs and backups
```

`wireguard-tools` and `openresolv` are normal packages the installer may have
added; remove them with your package manager if you no longer need WireGuard at
all.
