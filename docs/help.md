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
- **Import from QR code…** reads a PNG or JPEG image, not a live camera.
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

## 3. Day-to-day controls

Select a tunnel on the left. **Activate / Deactivate** brings its interface
up/down. Interface-up status is not proof of a handshake or working routes.
The app displays recent handshake health, transfer totals and throughput;
an idle peer can have an old handshake without being broken.

Click a copyable value or its trailing **⧉** icon to copy it. **Show QR** exposes
the full saved config, including its private key, for scanning on another device.
Use it only where screen access is trusted.

**Start on boot** enables a systemd `wg-quick@<name>` service for boot, not login.
It needs systemd; OpenRC/runit require their own service setup.

**Advanced ▾** on the tunnel card reveals:

- **Kill switch:** host output firewall rules for an active tunnel. Loopback,
  tunnel output and its fwmark are allowed; other output can be blocked,
  including LAN traffic outside a split tunnel. SSH handling is conditional,
  not a guarantee against locking yourself out. Rules are not persistent and
  external service changes can leave them behind.
- **Running cfg:** copy live WireGuard configuration, including private keys.
- **Save live:** back up and write runtime state to disk with wg-quick save;
  this can replace hand-written file content.

There is no global Easy/Advanced mode. The expansion choice is remembered.
After upgrading to 1.9.1, reapply an enabled kill switch once (off, then on) so
existing rules use the corrected ordering. Plan for the brief interval without
those rules. Restarting the app or activating alone does not reapply them.

## 4. Logs and backups

**Log** loads recent helper/wg-quick journal output on opening or Refresh. Use
Filter, This tunnel, Copy or Save. Click a row to copy one line. Clear only
clears the displayed buffer; it does not erase the system journal.

**Backup → New backup** archives readable saved configs. ZIPs are unencrypted,
mode 0600, under `$XDG_DATA_HOME/wireguard/backups` or
`~/.local/share/wireguard/backups`, shared with the terminal sibling. Select one
to Restore, Export or Delete. The bottom export icon separately exports all
readable saved tunnel configs to a ZIP destination.

Restore accepts only valid, unique, flat `.conf` entries and preflights them all
before saving: no script hooks, at most 1 MiB per config, 16 MiB total config
text and 1,024 configs. It skips unrelated files. Matching saved tunnels are
overwritten, extra saved tunnels remain, and running interfaces are not synced.
A later save failure can leave earlier files restored; there is no rollback.
Protect all archives and clipboard copies as carefully as the private keys.

## 5. When something is wrong

**Diagnose** checks clock-sync status, interface/handshake state, endpoint ping
and basic DNS. It gives troubleshooting hints, not a conclusive server test;
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
