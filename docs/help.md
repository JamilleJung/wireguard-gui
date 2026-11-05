# WireGuard, explained

A short guide to the current desktop application. Repository documentation may
include corrections made after a binary was released; a binary's Help text is
embedded when it is built.

## 1. Interface, peers and keys

A WireGuard interface is this machine's end of an encrypted network. It has a
private key, tunnel addresses and optional DNS settings. It can have one or more
peers: other machines identified by their public keys, AllowedIPs and, when
needed, an endpoint such as `vpn.example.com:51820` or `[2001:db8::2]:51820`.

Keep your private key secret. Give the server your public key and configure its
public key here. An optional preshared key must match at both ends. Importing a
provider's config is usually simpler than generating a new identity: a newly
generated public key must be registered on the other end before it can work.

AllowedIPs selects destinations for a peer. `0.0.0.0/0` covers all IPv4;
`::/0` covers all IPv6. `10.100.0.0/24` covers only that subnet. Persistent
keepalive is optional; 25 seconds is a common choice when maintaining a NAT
mapping is necessary, not a requirement for every connection.

## 2. Add and edit a tunnel

Use **Add Tunnel**:

- **Import tunnel(s) from file…** accepts `.conf` files. One file opens for
  review; several files are imported with unique names and invalid ones skipped.
- **Import from QR code…** reads a bounded PNG or JPEG image, not a live camera. All grids are checked;
  one valid distinct tunnel is required, and ambiguous images are refused.
- **Create tunnel…** opens a preset with a generated key. Choose Interface only,
  Full tunnel or Split tunnel, then fill in real addresses and peer details.

Name becomes the Linux interface and config filename: 1–15 ASCII letters,
digits, `_`, `.` or `-`, starting with a letter/digit, with no `..` or `.conf`
suffix. Use **Create** to save or **Create & Activate** to save and connect.
Existing tunnels use **Edit** then **Save**; changing Name renames the tunnel.

**Edit fields** shows common Interface/Peer fields. **Advanced fields** exposes
Listen port and MTU. Use Prev/Next, Add peer and Remove for multiple peers.
**Config text** keeps the raw file. Comments, repeated directives, unknown keys,
Table and SaveConfig remain in raw text so the form cannot discard them.
Inline `#` comments are accepted by config validation.

The helper rejects saving/renaming `PreUp`, `PostUp`, `PreDown` and `PostDown`
script hooks even if the editor shows a warning. Such commands would execute
as root through wg-quick. Administrator-managed files require separate review.

Saving an active tunnel attempts a live WireGuard sync. Address, DNS, MTU,
Table and routing changes still require reconnecting. Renaming stops an active
old interface; the renamed tunnel is not activated automatically.

If a save fails or the config does not validate, your draft stays in the editor
and a bar offers **Retry**, **Re-edit** or **Discard**. Closing the editor while a
save failed or is still running keeps the draft and asks first. A save that
timed out is checked afterwards: if the saved file matches your draft it counts
as saved, otherwise the draft stays open.

If the file was saved but applying it to the running interface failed, a banner
says that the saved file and the running interface now differ and offers **Retry
sync**, **Reconnect** or **Keep as is**. A FwMark change always needs a reconnect.

## 3. Day-to-day controls

Select a tunnel on the left. **Activate / Deactivate** brings its interface
up/down; afterwards the app re-checks that the interface really is up (or gone)
and reports a mismatch instead of trusting the exit status. Rename and remove are
checked the same way. Interface-up status is not proof of a handshake or working
routes. The app includes runtime-only peers and actual runtime settings, plus
transfer totals and throughput.

**Connection** health is shown for the tunnel and for each peer:

- **Healthy:** a handshake completed in the last 3 minutes.
- **Idle:** no recent traffic and no persistent keepalive. WireGuard only
  handshakes when there is traffic, so an old handshake here is normal.
- **No traffic:** nothing has been sent or received yet.
- **Connecting…:** a handshake is in progress (traffic started, or keepalive on).
- **Handshake stale:** persistent keepalive is on, yet the handshake expired.
- **Connection failed:** packets were sent for over 90 seconds without a reply,
  or a keepalive peer never completed a handshake.

Only the last two show a warning. If the system clock is also not NTP-synced,
the warning names the clock as one possible cause; an unsynced clock alone is
not reported as a connection problem.

Tunnels with more than one peer show peers five at a time: **Prev**/**Next**
page through them and **Filter peers** matches key, endpoint or AllowedIPs.

Click a copyable value or its trailing **⧉** icon to copy it. **Show QR** exposes
the full saved config, including its private key, for scanning on another device.
Use it only where screen access is trusted.

**Start on boot** enables a systemd `wg-quick@<name>` service for boot, not login.
It needs systemd; OpenRC/runit require their own service setup.

**Advanced ▾** on the tunnel card reveals:

- **Kill switch:** only one managed tunnel switch at a time. It requires an
  active interface and complete firewall tools, prefers atomic nft batches,
  and otherwise preflights IPv4/IPv6 restore transactions (atomic per family).
  Loopback/tunnel/marked-transport exceptions precede rejection. It affects host
  output, not server NAT; remote access can be disrupted. If its rules go
  missing (for example after a firewall reload) or no longer match the tunnel,
  a red banner says traffic is not protected and the app repairs it once
  automatically, retrying twice with a growing delay; **Repair now** retries by
  hand. An unreadable status is shown as unknown, never as off.
- **Running cfg:** copy live WireGuard configuration, including private keys.
- **Save live:** back up and write runtime state to disk with wg-quick save;
  this can replace hand-written file content.

There is no global Easy/Advanced mode. The expansion choice is remembered.
When upgrading from before 1.9.2, reapply an enabled kill switch once (off, then
on) so existing rules use the corrected ordering. Plan for the brief interval without
those rules. Restarting the app or activating alone does not reapply them.

## 4. Logs and backups

**Log** loads recent helper/wg-quick journal output on opening or Refresh. Use
Filter, This tunnel, Copy or Save. Click a row to copy one line. Clear only
clears the displayed buffer; it does not erase the system journal.

**Backup → New backup** archives all saved configs or reports a read error;
exports no longer silently omit unreadable tunnels. Archives are unencrypted,
0600, in the private shared wireguard/backups data directory. Completed exports
replace destinations atomically; failed writes keep previous data.

Restore preflights one owned single-link regular ZIP: at most 64 MiB compressed,
4096 entries, 1024 configs, 1 MiB per config and 16 MiB total config text. It
rejects duplicate names, unsafe paths, invalid content and root hooks, skipping
unrelated files. Saves then occur sequentially. If one fails, the tunnels already
written are rolled back (previous contents restored, newly created ones removed)
and the message lists exactly what was restored, removed, or could not be rolled
back. Existing extra tunnels remain and running state is not synced.

## 5. When something is wrong

**Diagnose** checks clock-sync status, interface/handshake health per peer,
endpoint ping and basic DNS. The clock is named only as a possible cause, and
only when handshakes are failing. It gives troubleshooting hints, not a conclusive server test;
ICMP can be blocked and routing/firewall issues still need investigation.
Check that the other peer knows your public key, endpoint and AllowedIPs.

A `DNS =` line needs a working resolvconf command. If that command is the
resolvectl shim, systemd-resolved must also be active. The startup Setup window
appears for critical missing tools/helper authorization. Show commands explains
fixes; automatic fixes can install prerequisites, create the config directory,
or enable a detected inactive resolver service with administrator authorization.
It does not install the helper or connect tunnels.

The menu button offers Help, Check for updates and About. Startup/manual update
checks contact GitHub. Disable startup checks with `WG_NO_UPDATE_CHECK=1` or a
`wireguard-gui/no-update` file under your XDG config directory; manual Updates
still checks. Update now verifies signed checksums, then runs the release
installer and requires restart. Package-managed installs should normally update
through their package manager.

Close hides the window only when a tray host is available; use tray Quit to exit.
Without a tray host, closing quits. Do not manage the same interface from both
this application and NetworkManager or another service.

## 6. Keyboard, themes and logging

Every control can be reached with Tab and pressed with Enter or Space.

- **Ctrl+1 / Ctrl+2 / Ctrl+3**: Tunnels / Log / Backup tab
- **Ctrl+O**: Import tunnel(s) from file
- **Ctrl+I**: Import from QR code
- **Ctrl+N**: Create tunnel
- **Ctrl+E**: Edit the selected tunnel
- **Ctrl+T**: Activate / deactivate the selected tunnel
- **Ctrl+D**: Diagnose the selected tunnel
- **Ctrl+R / F5**: Refresh the current tab
- **Ctrl+S**: Export all tunnels (Tunnels tab) / save the log (Log tab)
- **F1**: Help
- **Ctrl+U**: Check for updates
- **Tab / Shift+Tab**: Move between controls
- **Enter / Space**: Press the focused button or toggle
- **Up / Down**: Previous / next tunnel in the list
- **PageUp / PageDown**: Scroll the details, Help, log and changelog
- **Escape**: Close a dialog window or the editor

The editor saves with Ctrl+S and closes with Escape (a failed draft is kept).
Help, Updates, About, diagnostics and QR windows close with Escape.

Themes: start with `--theme NAME` or set `WG_GUI_THEME=NAME`. `default` is the
normal palette, `high-contrast` uses black text, strong borders and wider focus
rings, and `ascii` replaces symbols such as ⧉ ▾ → … with plain-text labels. Combine
them with `+`, for example `--theme high-contrast+ascii`. The command line wins
over the environment.

Set `WG_GUI_LOG=/path/to/file` to append structured `key=value` lines about
helper calls and operations (0600, never through a symlink). Private and
preshared keys are redacted before anything is written.

## Audited input and operation limits

Imported config files and QR images must be regular files (at most 1 MiB and
32 MiB). They are opened without following symlinks and without blocking, so
named pipes, sockets, devices and directories are rejected immediately; backups
are opened the same way.

Config fields are validated on every occurrence, including canonical key pad
bits, CIDR/keepalive bounds and exact section/field placement. Valid comments,
repeated fields, scoped IPv6 and supported Table/FwMark/SaveConfig options remain
available; unknown fields cannot be saved just because they remain in raw text.
QR import uses at most 32 MiB source files and 4096 × 4096 dimensions with a
configured 128 MiB decoder budget, and requires one distinct valid tunnel.

Release helpers must be trusted installed/adjacent executables with protected
ancestry; install a verified user-owned bundle before using its helper. Debug
helper overrides do not grant authorization. Background reads check polkit
without password dialogs; explicit mutations can use a desktop agent. A failed
status refresh preserves prior data rather than inventing an inactive state.

Mutations are serialized by a bounded helper lock. Failed/unknown stops retain
kill-switch protection and report errors; inspect state before retrying. A
90-second UI helper deadline bounds waiting, not rollback of already completed
network changes. Local key generation is bounded separately.

Verified update bundles own their private staging directory, check all required
assets before privilege escalation, reject a non-newer version at download time,
and clean staging after completion/failure. Success reports the actual verified
version. Restart after installing, and prefer the package manager for a packaged
installation.
