# WireGuard, explained

A friendly guide to what WireGuard is, what every field means, and how to use this app. You don't need to be a network engineer: read the first three sections and you can set up a tunnel. The rest is there for when you need it.

> This Help is built into the app when it is compiled. The copy in the project repository may be a little newer than the one in your binary.

## The 60-second mental model

A WireGuard tunnel is an encrypted link between this machine and another one. Think of it as a private cable plugged in between you and a server: whatever goes through the cable is encrypted, and to the rest of the network it looks like it comes from the other end.

Every tunnel has two halves:

- The Interface is you, this machine. It has a private key, an address inside the tunnel and, optionally, DNS servers to use while connected.
- A Peer is the other machine, usually your VPN server. It has a public key, an address to reach it at (the endpoint) and a list of destinations that should go through it (AllowedIPs). A tunnel can have several peers.

When you activate a tunnel, the two halves do a quick handshake (a cryptographic hello) and traffic starts to flow. WireGuard is a quiet protocol: it only handshakes when there is something to send. An old handshake on a quiet tunnel is a nap, not a failure.

## The keys (this is the whole security model)

WireGuard uses public-key cryptography. Every machine has a key pair:

- Private key: your secret. Keep it on this machine and never share it. Generate keypair in the editor makes a new one.
- Public key: derived from the private key. You do share this one; it is how the server recognises you. The app shows it so you can copy it.

The rule is symmetric: the server must know your public key, and you must know the server's public key. If the server does not have your public key in its peer list, it quietly ignores your handshakes. That is a very common reason for "it won't connect".

- Preshared key (PSK): an optional extra shared secret on top. If your config has one, the other end must have exactly the same one or the handshake fails. Generate PSK makes one.

> Tip: you rarely need to create keys by hand. Download a config from your provider or your own server and import it: the keys are already filled in and already registered at the other end. A brand-new public key only works once it is registered there too.

## Every field, in plain words

### Interface (you)

- Name: the tunnel's label, for example home or work. It becomes the Linux interface name and the file /etc/wireguard/NAME.conf, so it must be 1–15 ASCII letters, digits, _, . or -, start with a letter or digit, and contain no .. and no .conf suffix.
- Private key: your secret (see above).
- Address: your IP address inside the tunnel, for example 10.100.0.17/24. The server side assigns it; it is not your normal LAN address.
- DNS: the DNS servers to use while the tunnel is up, for example 1.1.1.1. Optional, and it needs a working resolvconf on your system (see "When something is wrong").
- Listen port and MTU: advanced and usually left empty. Find them under Advanced fields in the editor.

### Peer (the other end)

- Public key: the other machine's public key.
- Preshared key: the optional shared secret (see above).
- Endpoint: where to reach the peer, as host:port, for example vpn.example.com:51820 or [2001:db8::2]:51820. This is the one address that travels over your normal connection.
- AllowedIPs: which destinations are routed into the tunnel. 0.0.0.0/0 covers all IPv4 and ::/0 all IPv6 (a full tunnel). 10.100.0.0/24 covers only that subnet (a split tunnel); everything else uses your normal connection.
- Persistent keepalive: send a tiny packet every N seconds to keep a NAT mapping open. 25 is a common choice when you are behind a home router. It is optional, not a requirement for every connection.

## Getting a tunnel in

Use Add Tunnel:

- Import tunnel(s) from file… reads .conf files, the usual way. One file opens in the editor for review with a suggested unique name, so you can rename it. Several files are imported in one go with unique names; invalid ones are skipped and nothing existing is overwritten.
- Import from QR code… reads a PNG or JPEG image of a QR code (an image file, not a camera). Every QR code in the image is checked: unreadable or unrelated ones are skipped, exactly one valid tunnel opens for review, and an image with several different tunnels is refused.
- Create tunnel… opens the Full tunnel preset with a brand-new key pair. The preset buttons Interface only, Full tunnel and Split tunnel each start over with a new key, so pick one before typing your real settings. Then fill in the real address, the peer's public key and endpoint, and register your public key at the other end.

Use Create to save, or Create & Activate to save and connect straight away. With no tunnels yet, the main window offers Import .conf, Import QR image and New tunnel buttons.

## Day to day

Select a tunnel on the left to see its Interface and Peer cards. Activate / Deactivate brings it up or down with wg-quick. Afterwards the app checks that the interface really is up (or really gone) and tells you if it is not, instead of trusting an exit code. Rename and delete are checked the same way. The cards show runtime-only peers and the live settings, plus transfer totals and current speed.

### Is it actually connected?

Connection health is shown for every peer and for the whole tunnel:

- Healthy: a handshake completed in the last 3 minutes.
- Connecting…: a handshake is in progress (traffic started, or keepalive is on) and still within WireGuard's retry window.
- Idle: no recent traffic and no persistent keepalive. WireGuard only handshakes when there is traffic, so an old handshake here is normal. Send something and it wakes up.
- No traffic: nothing has been sent or received yet.
- Handshake stale: persistent keepalive is on, yet the handshake has expired.
- Connection failed: packets were sent for over 90 seconds without a reply, or a keepalive peer never completed a handshake.

The tunnel shows its most serious peer state, so one failing peer is never hidden behind a healthy one. Only the last two show a warning banner, with a Diagnose button. If the system clock is also not NTP-synced, the warning names the clock as one possible cause; an unsynced clock on its own is never reported as a connection problem.

Interface up is not proof of a handshake or of working routes, and transfer numbers describe the traffic that was seen, not that every destination is reachable.

### Peers, copying and QR codes

Tunnels with more than one peer show peers five at a time: Prev and Next page through them, and Filter peers matches a key, endpoint or AllowedIPs.

Click a copyable value or its trailing ⧉ icon to copy it.

Show QR displays the full saved config, private key included, so the WireGuard app on your phone can scan it (WireGuard → + → Scan from QR code). Anyone who photographs it can use your tunnel, so only show it where nobody else can see your screen.

### Start on boot

Start on boot enables the systemd service wg-quick@NAME. It changes what happens at the next boot, not at desktop login, and not the tunnel's current state. It needs systemd; OpenRC, runit and other init systems need their own service set up.

### Advanced ▾

Advanced ▾ on the tunnel card reveals the expert tools. There is no global Easy/Advanced mode, and the app remembers whether you left it open.

- Kill switch: blocks traffic that would leave this machine outside the tunnel. It needs an active tunnel, and only one tunnel can own the kill switch at a time.
- Running cfg: copies the live WireGuard configuration (wg showconf), including private keys.
- Save live: backs up the file and writes the running state to disk with wg-quick save. This can replace hand-written file content, so use it deliberately.

## The kill switch

With the kill switch on, the only traffic allowed out of the machine is loopback, traffic going into the tunnel, the tunnel's own encrypted WireGuard packets, IPv6 neighbour discovery and MLD reports, DHCP, and, when the helper can see it, the return traffic of the SSH session you turned it on from. Everything else is rejected. Your own firewall rules still apply on top: its exceptions never override a deny rule of yours.

The helper prefers nftables and keeps the rules in a table of its own (inet wg_helper_killswitch); without nftables it uses a dedicated iptables chain (WGH-KILLSWITCH) and needs ip6tables too when the kernel has IPv6. It needs complete firewall tools and never edits another table.

Its status is one of these:

- enabled: complete rules that match the running tunnel. All good.
- disabled: off.
- missing: it was turned on, but its rules are gone or incomplete, for example after a firewall reload.
- mismatch: rules exist but are stale, for example the tunnel's firewall mark changed, the tunnel went down, or an older release built them.

When the rules go missing or stale on an active tunnel, a red banner says that traffic is not protected, and the app repairs the switch by itself: once straight away, again 15 seconds later, and a last time 90 seconds after that. Repair now tries again by hand at any time. If all three automatic attempts fail, the banner says so; use Repair now, or turn the kill switch off and on. A status that cannot be read is shown as unknown, never as off.

Good to know:

- The kill switch is temporary, NOT persistent. Quitting the app keeps it. Deactivating, deleting or renaming the tunnel turns it off, and activating again does not turn it back on: tick Kill switch again. A reboot clears it, even for a tunnel that starts on boot. While the app is not running, nothing repairs it after a firewall reload. To block traffic from boot or whenever no tunnel is up, use your own persistent firewall.
- Deactivating, deleting or renaming the tunnel keeps protection in place while the interface is still up or its state is unknown, and reports what went wrong.
- If the interface disappears without a disconnect, the rules keep blocking traffic (status mismatch) until you disconnect.
- Editing a connected tunnel keeps the kill switch working: the running firewall mark and listen port are kept, a FwMark change needs a reconnect, and the switch is checked and repaired afterwards.
- A kill switch built by 1.9.3 or older reports mismatch and is rebuilt in the current layout on an active tunnel.
- It controls this machine's own outgoing traffic. It is not server forwarding or NAT, not a general no-leak guarantee, and it can interrupt remote access.

## Editing safely

Select a tunnel and choose Edit, then Save. Changing Name renames the tunnel: an active old interface is stopped, its start-on-boot setting moves to the new name where possible, and the renamed tunnel is not started for you.

Edit fields shows the common Interface and Peer fields; Advanced fields adds Listen port and MTU. Use Prev / Next, Add peer and Remove for several peers. Config text is the raw file. Existing tunnels open as Config text, so just opening one never rewrites it. Comments, repeated directives, unknown keys, Table, SaveConfig and Interface-only configs stay in Config text, because the form could not keep them. Inline # comments are fine.

The helper refuses to save or rename a config with PreUp, PostUp, PreDown or PostDown, even if the editor only shows a warning: those commands would run as root through wg-quick. Files an administrator manages by hand need their own review.

### When a save goes wrong

Your draft is never thrown away for you. If the config does not validate or the save fails, the draft stays in the editor and a bar offers:

- Retry: save the same draft again.
- Re-edit: hide the bar and go back to editing, text unchanged.
- Discard: close the editor; nothing is saved.

Closing the editor while a save failed or is still running keeps the draft and asks first. A save that timed out is checked afterwards: if the saved file matches your draft it counts as saved, otherwise the draft stays open.

Saving an active tunnel also applies the change to the running interface. Address, DNS, MTU, Table and routing changes still need a reconnect. If the file was saved but applying it to the running interface failed, a banner says that the saved file and the running interface now differ and offers Retry sync, Reconnect or Keep as is. A FwMark change always needs a reconnect.

## Logs and backups

Log shows recent helper and wg-quick journal entries. It loads at startup, when you open the tab, and on Refresh; it never refreshes by itself while you read. Use Filter and This tunnel to narrow it down. Click a line to copy it; Copy and Save use the whole loaded log. Clear only empties this view, not the system journal.

Backup → New backup archives every saved tunnel, or reports a read error instead of quietly leaving one out. Select a backup to Restore, Export or Delete it. Backups are unencrypted ZIP files (mode 0600) in the private, shared wireguard/backups folder of your user data directory, so they contain your private keys. The export-all button at the bottom of the tunnel list writes every tunnel to a ZIP you choose. Finished exports replace their destination in one step; a failed write keeps the previous file.

Restore checks the whole archive first. Then it saves the tunnels one by one, and if one save fails, everything already written is rolled back (previous contents restored, newly created tunnels removed). The message lists exactly what was restored, removed or could not be rolled back. Tunnels that are not in the backup stay as they are, and running interfaces are not synced.

## When something is wrong

Press Diagnose. It checks clock sync, the interface and the handshake of every peer, pings the first peer endpoint and tries a DNS lookup, then lists likely causes. The clock is named only as a possible cause, and only when handshakes are failing. Treat the results as hints: a server can block ping while WireGuard works fine, and routing or firewall problems still need a look. Also check that the other end knows your public key and the right AllowedIPs.

A DNS = line needs a working resolvconf command. If that command is the resolvectl shim, systemd-resolved must be running too.

The Setup window opens at startup when something critical is missing, such as the WireGuard tools or helper authorization. Show commands explains the fixes. Fix automatically can install prerequisites, create /etc/wireguard or enable a detected but inactive resolver service, with administrator authorization. It never installs the helper, creates tunnels or connects them.

The menu button next to Add Tunnel offers Help, Check for updates and About. Update checks at startup and in the Updates window contact GitHub. Turn the startup check off with WG_NO_UPDATE_CHECK=1 or a wireguard-gui/no-update file in your XDG config directory; the Updates window still checks when you open it. Update now verifies the signed checksums first, then runs the release's installer with the prefix and authorization you installed with. Copies installed by a package manager or cargo install are left to that tool, and the app tells you the command. Restart the app afterwards.

With a system tray, closing the window only hides it; use Quit in the tray to exit. Without a tray, closing the window quits.

### Two gotchas worth knowing

- Active, but nothing loads: the interface is up but no handshake completes, so no real traffic flows. Usual suspects: the server does not have your public key, the endpoint is unreachable, or the system clock is far off. Diagnose helps you narrow it down.
- Don't manage the same tunnel from two places. If NetworkManager or another tool also handles this interface, they will fight. Pick one.

## Keyboard, themes and logging

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

The editor saves with Ctrl+S and closes with Escape (a failed draft is kept). Help, Updates, About, diagnostics and QR windows close with Escape.

Themes: start with --theme NAME or set WG_GUI_THEME=NAME. default is the normal palette, high-contrast uses black text, strong borders and wider focus rings, and ascii replaces symbols such as ⧉ ▾ → … with plain-text labels. Combine them with +, for example --theme high-contrast+ascii. The command line wins over the environment.

Set WG_GUI_LOG=/path/to/file to append structured key=value lines about helper calls and operations (mode 0600, never written through a symlink). Private and preshared keys are redacted before anything is written.

## Audited input and operation limits

The fine print, for the curious and the careful.

- Imported config files and QR images must be regular files of at most 1 MiB and 32 MiB. They are opened without following symlinks and without blocking, so named pipes, sockets, devices and directories are refused immediately. Backups are opened the same way.
- QR images are limited to 4096 × 4096 pixels with a 128 MiB decoder budget, and are decoded in a short-lived child process with a memory limit and a deadline, so a hostile image can only fail its own import. One distinct valid tunnel is required.
- Config fields are validated on every occurrence, including canonical key padding bits, CIDR and keepalive bounds, and exact section and field placement. Valid comments, repeated fields, scoped IPv6 endpoints and the supported Table, FwMark and SaveConfig options stay available; unknown fields cannot be saved just because they remain in raw text. A PresharedKey line must hold a valid key; leave it out for no PSK.
- Restore accepts one owned, single-link regular ZIP: at most 64 MiB, 4096 entries, 1024 configs, 1 MiB per config and 16 MiB of config text in total. Duplicate names, unsafe paths, invalid content and root hooks are refused before the first save; unrelated files are skipped.
- Release builds only use a trusted helper: this project's installed one or an adjacent, root-owned, protected executable with protected parent directories. Install a verified user-owned bundle before using its helper. Debug helper overrides do not grant authorization.
- Background reads check polkit without opening password dialogs; actions you start can ask through your desktop's authentication agent. A failed status refresh keeps the previous data instead of pretending the tunnel is inactive.
- Changes are serialized by a bounded helper lock. A failed or unknown stop keeps kill switch protection and reports the error; check the real state before retrying. The 90-second helper deadline bounds the wait, not the rollback of network changes that already happened. Local key generation has its own 10-second limit.
- Verified update bundles keep their private staging directory to themselves, check every required file before asking for privileges, refuse a version that is not newer than the running one, and clean up after success or failure. Success reports the version that was actually verified.
