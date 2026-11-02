# Security policy

## Reporting a vulnerability

Report exploitable issues privately through
[GitHub private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new)
or **izeystudio@gmail.com**. Include the version/commit, distro, reproduction
steps and impact; omit real private keys and production configs. Only the
latest release (**1.9.2**) and `main` receive fixes.

## Trust and scope

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

## Authorization and helper selection

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

A direct root install without an invoking normal user requires `--allow-root`
(or `WG_ALLOW_ROOT=1`). That override can permit source build scripts as root;
prefer a normal-user build or a verified complete prebuilt bundle.

## Root-managed file and command protections

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

## Config validation and root hooks

Config validation is shared by the frontend and privileged helper. It checks
every directive occurrence, known sections/field placement, canonical 32-byte
WireGuard base64 keys (including padding bits), numeric CIDR/port/keepalive
bounds, and required peer fields. Inline comments, repeated Interface sections,
repeated Address/DNS/AllowedIPs, scoped IPv6 endpoints, named/numeric Table,
FwMark hex/off and SaveConfig booleans are supported. Unknown fields or malformed
sections stay visible in raw text but cannot be saved until corrected. If a
PresharedKey directive is present it must contain a valid key; omit it to use
no PSK. The helper independently rejects all four root script-hook directives.

Hook rejection applies to managed save/import/rename/restore. An administrator
can maintain a hook-bearing file directly as root; up/down/save-live still use
wg-quick normally. Such existing hooks remain trusted root code, not a sandboxed
script interface. Redaction covers config-like secret lines, prefixed/equals
cases and malformed-key errors, but new arbitrary logging paths still require
review rather than an assumption that every possible secret string is detected.

## User backups, exports and editor data

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

Saved-name/export queries do not require a successful live `wg active` query,
so backing up valid files can still work when the VPN/kernel status query is
broken. File/list/read failures do not become a misleading successful subset.

The GUI editor holds drafts in application memory. Config text, Show QR,
Running cfg, config copies and generated-key views can expose private keys.
Clipboard history and screenshots can retain them. Keep the user's session and
exported files protected as well as `/etc/wireguard`.

QR image import caps source files at 32 MiB, dimensions at 4096 × 4096 and
configures a 128 MiB decoder-allocation budget. The decoder limit is a resource
budget, not a guarantee about total process overhead. All detected grids are
considered; one valid distinct tunnel config is required, with ambiguous images
refused. These bounds do not make arbitrary image decoders risk-free.

## Kill-switch scope and lifetime

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

## Signed releases and updates

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

Verified updates retain ownership of their private download directory through
installation and clean it up on download, staging or installation failure and
normal completion. Before privilege escalation, the whole required bundle is
checked: regular single-link application/helper/installer/assets, no symlink
components, and a fresh exclusive staging tree. Missing/unsafe assets fail before
installation starts. HTTPS-only redirects are enforced. GUI automatic updates
support its published x86_64/ARM64 builds; unsupported CPUs fail explicitly.

The verified bundle object is required by apply, so an arbitrary filesystem
path is not the public installation API. The installer still uses its normal
prefix/auth mode rather than updating a distro package database. Restart the
app after success and check current state after interrupted operations.

Startup/manual checks contact GitHub over HTTPS and include the app version in
the User-Agent; they do not upload tunnel configs. Diagnostics can ping/resolve
network hosts, and installers/builds contact package/crate sources. Disable
startup checking with `WG_NO_UPDATE_CHECK=1` or a `no-update` file under this
project's XDG config directory. Manual GUI update checks remain available.
Actions/release tools are pinned as described in the release guide; locked
builds improve traceability, not a bit-for-bit reproducibility guarantee.

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
inspecting its embedded binaries. A source `cargo auditable build --release --locked` also produces uncompressed auditable binaries.
