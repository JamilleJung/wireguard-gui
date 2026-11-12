# 🔐 Security policy

`wireguard-gui` manages tunnels under `/etc/wireguard`, which needs root. The
design goal is simple to say and fussy to do: keep the part that runs as root as
small and auditable as possible, and keep everything else unprivileged. This
page says exactly what that buys you, and what it doesn't.

## ⚡ TL;DR

| Question | Short answer |
|---|---|
| Found a vulnerability? | Report it **privately**: [GitHub private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new) or **izeystudio@gmail.com**. Not a public issue. |
| Which versions get fixes? | Only the latest release (**1.10.0**) and `main`. |
| Does the GUI run as root? | No. It runs as you; only the small `wg-helper` runs as root, one fixed verb per run. |
| What can the helper be asked to do? | A fixed set of verbs on validated tunnel names: no paths, no options, no shell. |
| Who can use it without a password? | Whoever the sudoers grant or the polkit rule (`wireguard` group + active local session) allows. So can **any program running as that user**. They can read **every** tunnel key and manage **every** tunnel; this is not isolation between users. See [shared hosts](#-shared-and-multi-user-hosts). |
| Can a config smuggle in root commands? | Not through the app: saves, imports, renames and restores refuse `PreUp`/`PostUp`/`PreDown`/`PostDown`. A hook-bearing file an administrator put in `/etc/wireguard` by hand still runs its hooks when activated; that stays the administrator's responsibility. |
| Where are secrets exposed? | `/etc/wireguard`, user backups and exports (unencrypted, `0600`), Show QR, Running cfg, config copies and the editor. Clipboard history and screenshots can keep them. |
| Is the kill switch a leak guarantee? | No. It is temporary host-output policy for one connected tunnel. **Not persistent:** deactivating the tunnel or rebooting turns it off and nothing turns it back on. Not server forwarding/NAT. See [what survives what](#-kill-switch-scope-and-lifetime). |
| Are updates verified? | Yes: minisign signature against the compiled-in key, then one exact checksum, then the verified installer. Nothing installs if a check fails. |
| What can't it protect against? | Root, another process running as you, a compromised kernel or WireGuard toolchain, your editor, display, terminal, screen capture or clipboard history. |

## 📧 Reporting a vulnerability

Please report exploitable issues **privately**, through
[GitHub private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new)
(Security → Report a vulnerability) or by email to **izeystudio@gmail.com**.

Include the version or commit, your distro, reproduction steps and the impact.
Leave out real private keys and production configs; the
[fake fixtures](tests/fixtures/configs/) are there for exactly this.
Coordinated disclosure is appreciated, and you'll be kept in the loop on the
fix.

Only the latest release (**1.10.0**) and `main` receive fixes. Running something
older? Please upgrade before reporting.

## 🎯 Trust and scope

The normal frontend runs unprivileged; a Rust helper manages files and network
state with root privilege. This is a constrained operations boundary, not
per-user tunnel isolation or a guarantee that no exploitable bugs exist.
An authorized helper user can read all managed private keys, replace/delete
configs, connect/disconnect interfaces, change boot settings and alter supported
firewall rules. Grant access only to trusted operators.

The app cannot isolate secrets from root, another process running as the same
user, a compromised kernel/WireGuard toolchain, editor/display/terminal, screen
capture or clipboard history. Existing root-managed files and system tools must
remain trustworthy. Validation and audits reduce specific risks; they do not
prove every dependency, config or firewall environment safe.

The architecture behind this boundary, with diagrams, is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md#privilege-boundary).

## 👥 Shared and multi-user hosts

> [!WARNING]
> The helper grant is **passwordless**. That is deliberate: the app checks
> tunnel status in the background, and a password prompt every few seconds
> would make it unusable. The cost is that the grant does not belong to
> `wireguard-gui` alone. **Any program running as the granted user** can call
> the helper without a password.

Whoever holds the grant, and any process running as that user, can:

- read every private and preshared key under `/etc/wireguard`, including
  tunnels that other people set up;
- replace or delete any managed config;
- activate or deactivate any managed tunnel. Tunnels are system-wide, so a
  full-tunnel config (`AllowedIPs = 0.0.0.0/0, ::/0`) carries **every user's**
  traffic, not just yours;
- turn on the kill switch, which blocks outgoing traffic for every user on
  the host;
- change which tunnels start on boot.

The helper does not know who owns a tunnel; there is no per-user isolation.

| Your host | Recommended setup |
|---|---|
| Personal laptop or desktop, one user | The default per-user sudoers grant is fine. |
| Several people log in, only you manage VPNs | Grant only yourself. Don't add other accounts to the `wireguard` group. |
| Several people manage VPNs | Use `--polkit` and add only trusted operators to `wireguard`. Treat each member as able to read all keys. |
| Shared server, jump host, CI runner, or a user account that runs untrusted code | Don't install the helper grant. Manage WireGuard as root with `wg`/`wg-quick` instead. |

Check and remove grants:

- See who holds a grant: `sudo cat /etc/sudoers.d/wireguard-gui` and
  `getent group wireguard`.
- Remove one user from the polkit group: `sudo gpasswd -d USER wireguard`
  (Alpine/BusyBox: `sudo deluser USER wireguard`); it takes effect at their
  next login.
- Remove everything the installer added: `./install.sh uninstall`. Your
  configs in `/etc/wireguard` stay.
- Review what the helper did: every mutating call writes one redacted
  record to the system log, tagged `wireguard-gui`
  (`journalctl -t wireguard-gui`).

## 🔑 Authorization and helper selection

The app uses direct execution when already root, otherwise probes scoped
`sudo -n`, then uses pkexec. The startup sudo probe is bounded. Background
read-only calls require an existing noninteractive polkit authorization;
explicit mutations may use an external authentication agent. The internal tty
agent is disabled so worker/raw-terminal operations do not wait on hidden
prompts. SSH users should arrange the scoped sudoers path or another reviewed
administrative setup.

Source/prebuilt `install.sh` defaults to `/usr/local/lib/wireguard-gui/wg-helper`
and a per-user sudoers grant. `--polkit` instead installs a rule requiring an
active **local** session and membership in `wireguard`. It requires pkexec and
successful group enrollment, not merely a copied rule. Log out/in after joining.
An SSH session alone does not satisfy that local-session rule. The `.deb` and
distro recipes install the rule under the documented `/etc` or `/usr/share`
path but have no user/group enrollment script.

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

Debug builds accept `WG_HELPER`. Release overrides additionally require
`WG_ALLOW_UNSAFE_HELPER=1` and the same executable/ownership/ancestry checks.
An override does not grant authorization for a different executable path.
Installer/setup/update paths can also run approved package-manager or installer
commands with administrator privilege; the runtime helper is not the only
privileged process involved in installing or repairing the system.

<details>
<summary><b>🧾 Installer prefixes, grants and root installs</b></summary>

Before privileged writes or helper grants, `PREFIX` must be an absolute ASCII
path containing only letters, digits, `/`, `_`, `.` and `-`, with no empty,
`.` or `..` components or trailing slash. Existing directory ancestors must be
root-owned without group/other write permission; symlinks are refused. A
group-writable `/usr/local` can therefore fail these checks. An administrator
must choose or prepare a protected path; the installer does not take ownership
of caller directories. `--polkit` supports only `/usr` or `/usr/local`, matching
the bundled rule. Custom sudoers prefixes still require matching frontend helper
discovery configuration.
Existing app/helper targets must be root-owned regular files with one link and
no group/other write bits; the installed helper must be executable before an
authorization grant is written.

Installer options are validated; unknown options fail instead of being ignored.
Source builds use the lockfile. Switching sudoers/polkit modes installs and
validates the selected grant before removing the previous grant. Failed sudoers
validation or group enrollment reports failure and keeps prior authorization.
`--polkit` requires pkexec up front; it does not install a rule and then claim
success without the escalation tool. Packages still do not enroll users.

Repeating `--polkit` checks the existing `wireguard` group and account
membership before invoking group-management tools. An already configured
Alpine/BusyBox system can reinstall without optional Shadow `groupadd`/`usermod`.
Missing groups or membership must still be created and verified before the
previous authorization is removed.

A direct root install without an invoking normal user requires `--allow-root`
(or `WG_ALLOW_ROOT=1`). That override can permit source build scripts as root;
prefer a normal-user build or a verified complete prebuilt bundle.

</details>

## 🧱 Root-managed file and command protections

The helper accepts fixed verbs for listing/status, config read/save/rename/delete,
activation, boot settings, sync/save-live, log and kill-switch operations.
Names are at most 15 ASCII bytes, start alphanumeric, then use alphanumeric or
`_`/`-`/`.`; surrounding whitespace, `..`, separators and `.conf` suffixes are
rejected. Caller-selected root destination paths are not accepted.

Existing root configs must be regular, effective-user-owned (root in production),
single-link files, at most 1 MiB, without group/other write permission. Symlinks,
hardlinks and FIFOs are refused; a root-owned read-only-to-others `0644` file
can still be read. Managed writes use `0600`, exclusive temporary files, file
sync, atomic publication and directory sync. Helper backups use exclusive
single-link names with timestamp/PID uniqueness and never clobber an earlier
backup. Failed staging is cleaned up.

<details>
<summary><b>🔒 Locking, rename rollback and process limits</b></summary>

Mutation verbs share `/etc/wireguard/.helper.lock`: a trusted `0600` single-link
lock in the private directory, with a 2-second busy deadline. Read-only calls
do not create directories. Directory mutation checks enforce trusted ownership
and private `0700` storage. Rename stages a validated non-overwriting target,
then stops/migrates the old tunnel and boot unit; rollback tries to preserve
config/enablement on failure. It is not a promise to restore all runtime state:
a failed rename can leave an interface stopped. Errors must be reviewed.

Tools are resolved from `/usr/sbin:/usr/bin:/sbin:/bin`; child environments use
fixed PATH and `LC_ALL=C`. Input, captured stdout/stderr and deadlines are
bounded; each captured stream is limited to 4 MiB. Pipe draining occurs while
commands run, including blocked-input/escaped-descendant cases. The leader
stays unreaped until pipe work ends if group cleanup may signal it, avoiding
PID reuse. Timeouts do not guarantee reversal of an operation already performed.

</details>

## 📄 Config validation and root hooks

Config validation is shared by the frontend and privileged helper. It checks
every directive occurrence, known sections/field placement, canonical 32-byte
WireGuard base64 keys (including padding bits), numeric CIDR/port/keepalive
bounds, and required peer fields. Inline comments, repeated Interface sections,
repeated Address/DNS/AllowedIPs, scoped IPv6 endpoints, named/numeric Table,
FwMark hex/off and SaveConfig booleans are supported. Unknown fields or malformed
sections stay visible in raw text but cannot be saved until corrected. If a
PresharedKey directive is present it must contain a valid key; omit it to use
no PSK. The helper independently rejects all four root script-hook directives.

> [!WARNING]
> Hook rejection applies to managed save/import/rename/restore. An administrator
> can maintain a hook-bearing file directly as root; up/down/save-live still use
> wg-quick normally. Such existing hooks remain trusted root code, not a
> sandboxed script interface.

Redaction covers config-like secret lines, prefixed/equals cases and
malformed-key errors, but new arbitrary logging paths still require review
rather than an assumption that every possible secret string is detected.

## 💾 User backups, exports and editor data

User backups live under `$XDG_DATA_HOME/wireguard/backups` (an absolute XDG
path), or `~/.local/share/wireguard/backups`, shared by both applications.
Existing managed backup directories must belong to the current user and are
checked/tightened to `0700`; symlink directories are refused. ZIPs and exported
copies use `0600`. New backup names are unique even within one second and are
published without overwriting another archive. **All these archives contain
unencrypted private keys.**

The GUI editor holds drafts in application memory. Config text, Show QR,
Running cfg, config copies and views of newly created keys can expose private keys.
Clipboard history and screenshots can retain them. Keep the user's session and
exported files protected as well as `/etc/wireguard`.

<details>
<summary><b>📏 Export, restore and QR import limits</b></summary>

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

After complete preflight, restore copies every saved config it is about to
overwrite, then saves sequentially. It overwrites matching saved names, leaves
other tunnels in place, and does not sync running interfaces. If a later save
fails (or times out), restore **rolls back**: overwritten tunnels get their
previous contents back and tunnels the restore created are removed again. The
report names every tunnel restored, removed or, if a rollback step itself
failed, left changed. Helper per-file backups remain a separate recovery
source under `/etc/wireguard/.backup/`.

Saved-name/export queries do not require a successful live `wg active` query,
so backing up valid files can still work when the VPN/kernel status query is
broken. File/list/read failures do not become a misleading successful subset.

Imported config files are limited to 1 MiB and must be regular files; they are
opened without following symlinks and without blocking, so FIFOs, sockets,
devices and directories are refused immediately.

QR image import caps source files at 32 MiB, dimensions at 4096 × 4096 and
configures a 128 MiB decoder-allocation budget. The decoder limit is a resource
budget, not a guarantee about total process overhead. Images are decoded in a
short-lived child process with a memory limit and a deadline, so a decoder
crash or hang fails only that import. All detected grids are considered; one
valid distinct tunnel config is required, with ambiguous images refused. These
bounds do not make arbitrary image decoders risk-free.

</details>

## 🛑 Kill-switch scope and lifetime

> [!WARNING]
> **The kill switch is not persistent.** It protects one connected session.
> It is not an "always block unless the VPN is up" setting, and nothing turns
> it back on for you after a disconnect or a reboot.

| When this happens | The kill switch afterwards | Traffic outside the tunnel |
|---|---|---|
| You quit the app (tray → Quit) | Rules stay in place | **Blocked**, but nothing repairs the rules while the app is not running |
| A firewall reload wipes the rules, app running | `missing`; the app repairs it (red banner, up to three tries) | **Leaks** until the repair finishes |
| A firewall reload wipes the rules, app not running | `missing` until you start the app again | **Leaks** |
| You deactivate, delete or rename the tunnel in the app | Turned off and its record cleared | **Not blocked** |
| You activate the tunnel again | Stays off; tick **Kill switch** again | **Not blocked** until you do |
| The interface disappears without an app disconnect (`wg-quick down` in a shell, a crash) | Rules stay, status `mismatch` | **Blocked** (fails closed) until you deactivate in the app |
| The machine reboots | Rules and record are gone; start-on-boot does not turn it back on | **Not blocked** |

If you need traffic blocked from boot onwards, or whenever no tunnel is up,
configure that in your own persistent firewall (nftables, firewalld, ufw).
This kill switch does not do that.

Only **one helper-managed kill switch** can be active at a time across managed
tunnels. Turn the existing one off before enabling another. The helper requires
an already-active interface; it can assign an unused runtime WireGuard fwmark
without changing the saved config. It does not activate a stopped tunnel as a
side effect of enabling a switch.

> [!IMPORTANT]
> This is host-output control, not server forwarding/NAT or a general no-leak
> guarantee. It may disrupt remote access. It is not persistent: the rules and
> the record that it was requested live in the kernel and under `/run`, so both
> are gone after a reboot. App exit does not clear rules.

The rules allow loopback, packets routed into the selected tunnel, its
transport traffic (identified by the tunnel's runtime fwmark), IPv6
router/neighbor solicitation and neighbor advertisement (hop limit 255) and
MLDv2 reports, DHCP client traffic, and established SSH return traffic when
`SSH_CONNECTION` reaches the helper; everything else leaving the host is
rejected. Without the Neighbor Discovery exception an IPv6 endpoint becomes
unreachable once its neighbor cache entry expires. The exceptions **never
override your deny rules**: nftables `accept` only ends the kill switch's own
chain, and the iptables chain `RETURN`s allowed traffic to `OUTPUT` instead of
accepting it, so later rules still apply.

<details>
<summary><b>🔬 Backends, status, live edits and failure handling</b></summary>

The helper prefers nftables. It keeps the kill switch in its **own table**,
`inet wg_helper_killswitch`, with its own OUTPUT base chain, and replaces that
table in one nft transaction. It never edits another table, so firewalld, a
distro ruleset or your own rules stay untouched. The fallback uses a dedicated
`WGH-KILLSWITCH` chain reached by one jump at the top of `OUTPUT`. It requires
`iptables` and `iptables-restore`; when the kernel exposes IPv6 support it also
requires `ip6tables` and `ip6tables-restore`. Both families are preflighted;
commits are atomic **per family**, not one transaction across IPv4 and IPv6.

Status is one of `enabled`, `disabled`, `missing` or `mismatch`. **Enabled**
requires complete rules whose mark equals the interface's current fwmark.
**Missing** means the switch was turned on but its rules are gone or
incomplete, for example after a firewall reload (`nft flush ruleset`,
`firewall-cmd --reload`) or when another rule is placed ahead of the iptables
jump in `OUTPUT`, which no longer counts as complete. **Mismatch** means rules
exist but are stale: the mark changed, the interface is down, an older release
built them, or rules of the other backend are left over. On an active tunnel
the app repairs both automatically (`wg-helper killswitch-repair`), once per
loss and with backoff, and reports the result. A status that cannot be read is
shown as unknown, never as off.

Editing a connected tunnel keeps the kill switch working. `wg syncconf` would
reset the fwmark that wg-quick set at runtime; the helper carries the running
fwmark and listen port into the synced config, refuses a config that changes
`FwMark` on a running tunnel (reconnect to apply it), verifies the mark
afterwards and repairs the kill switch if needed.

Down/delete/rename retain protection if the interface is still active or its
state is unknown, and report stop/cleanup failures. If the interface
disappears without a disconnect, the rules stay and keep blocking traffic
(status `mismatch`) until you disconnect. A switch built by an earlier release
(1.9.3 or older) is reported as `mismatch` and rebuilt in the current layout on
the next refresh.

</details>

## ✍️ Signed releases and updates

The workflow refuses publication when minisign signing/verification fails or
`MINISIGN_KEY` is absent. Verify a manifest against an independently trusted
project key, then check exactly the artifact to be installed. A key downloaded
alongside an artifact needs independent comparison; a checksum alone cannot
authenticate its origin. Use [release verification](docs/RELEASES.md).

The updater verifies the signed manifest against its **compiled-in** key,
requires one exact archive checksum and a version newer than the running app,
then reports the version actually verified. It never accepts a downloaded
public key as its trust anchor. Missing verification tools or invalid data
refuse the update; minisign installation is only best-effort.

<details>
<summary><b>🧐 Update staging, installation method and network use</b></summary>

Verified updates retain ownership of their private download directory through
installation and clean it up on download, staging or installation failure and
normal completion. Before privilege escalation, the whole required bundle is
checked: regular single-link application/helper/installer/assets, no symlink
components, and a fresh exclusive staging tree. Missing/unsafe assets fail before
installation starts. HTTPS-only redirects are enforced. GUI automatic updates
support the published x86_64, i686, aarch64 and armv7 builds; unsupported CPUs
fail explicitly.

The verified bundle object is required by apply, so an arbitrary filesystem
path is not the public installation API. Only copies that `install.sh` recorded
in its root-owned `install-manifest` (or an older installer's root-owned helper
next to the app) are updated in place, by re-running the verified installer
with the recorded prefix and authorization mode; the updater never touches a
distro package database. Copies owned by a package manager or `cargo install`,
or installed in an unknown way, are refused with the command that updates them
instead, and so is an update that would keep a second copy under `/usr`
(unless both copies were forced at install time). Restart the app
after success and check current state after interrupted operations.

Startup/manual checks contact GitHub over HTTPS and include the app version in
the User-Agent; they do not upload tunnel configs. Diagnostics can ping/resolve
network hosts, and installers/builds contact package/crate sources. Disable
startup checking with `WG_NO_UPDATE_CHECK=1` or a `no-update` file under this
project's XDG config directory. Manual GUI update checks remain available.
Actions/release tools are pinned as described in the release guide; locked
builds improve traceability, not a bit-for-bit reproducibility guarantee.

</details>

## 🧪 Dependency auditing

`cargo audit --deny warnings` runs in CI and release workflows. The shared
[.cargo/audit.toml](.cargo/audit.toml) accepts five unmaintained notices without
upstream fixed versions: `bincode`, `ttf-parser`, `zvariant`, `zbus_names` and
`zvariant_derive`. The zvariant family is still required through ksni/zbus 5 and
includes runtime dependencies. Vulnerabilities, unsoundness and new advisories
remain failures; remove exceptions as upstream migrations become available.

Releases are built with `cargo auditable`, then tarball and `.deb` binaries are
UPX-compressed. To inspect their embedded dependency data, decompress a **copy**:

```sh
cp wireguard-gui wireguard-gui.audit
upx -d wireguard-gui.audit
cargo audit bin wireguard-gui.audit
```

Direct auditing of the packed executable does not expose its auditable ELF
section. The AppImage is assembled from uncompressed copies; extract it before
inspecting its embedded binaries. A source `cargo auditable build --release --locked`
also produces uncompressed auditable binaries.
