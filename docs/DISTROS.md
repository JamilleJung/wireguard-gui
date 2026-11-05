# 🐧 Linux installation and system integration

This guide describes the current installer and GUI behavior. Distro defaults,
package availability and local security policy vary; the package manager being
recognized does not guarantee every release binary works on every version.
Use the upstream [WireGuard installation guide](https://www.wireguard.com/install/)
for kernel/tool support on your specific distribution.

## 1. Choose source, native package or binary bundle

- The GitHub release supplies an x86_64 `.deb`, x86_64/ARM64 Linux tarballs and an
  optional x86_64 AppImage. See [downloads and verification](../README.md#install).
- `install.sh` in a source checkout installs C/Rust/graphics build prerequisites,
  builds the app/helper and installs under `/usr/local` by default.
- `install.sh` in an extracted binary bundle with adjacent binaries and no
  Cargo.toml reuses those binaries and installs runtime dependencies without a
  Rust compiler or development headers.
- AUR/RPM/APK/Void files are maintainer recipes, not a promise that a published
  repository package is current. Native packages install under `/usr`.

The GUI requires a graphical session and compatible font/input/OpenGL/EGL,
Wayland/X11 and D-Bus runtime libraries. Release tarballs are native Linux
builds, not static universal binaries; source/native packages may be necessary
on a different libc or older runtime. On headless hosts consider the
[terminal sibling](https://github.com/JamilleJung/wireguard-tui), whose supported
features and installation details are documented separately.

## 2. Package-manager paths used by the installer

These are the manager families recognized by `install.sh`. Runtime/build package
lists are maintained in that script; the commands below only install WireGuard
tools and require suitable administrator privileges/repositories.

| Family | Tools command | Installer manager |
|---|---|---|
| Debian / Ubuntu / Mint | `sudo apt-get install wireguard-tools` | `apt-get` |
| Fedora / RHEL-family | `sudo dnf install wireguard-tools` | `dnf` or `yum` |
| Arch-family | `sudo pacman -S wireguard-tools` | `pacman` |
| openSUSE | `sudo zypper install wireguard-tools` | `zypper` |
| Alpine | `apk add wireguard-tools` as root | `apk` |
| Void | `sudo xbps-install -S wireguard-tools` | `xbps-install` |
| Solus | `sudo eopkg install wireguard-tools` | `eopkg` |

The app does not bundle a kernel module or install a replacement kernel. The
Setup window does not probe module availability; a system can have `wg` tools
but still fail to create a WireGuard interface. Check your kernel/distro
support if interface creation fails.

## 3. Helper authorization

The source/bundle installer normally writes a user-specific sudoers grant for
`/usr/local/lib/wireguard-gui/wg-helper`. If usable sudo is unavailable, the
script can re-execute through `su` for system installation; do not assume the
presence of a sudo binary proves that your user can use it.

`./install.sh --polkit` instead installs a rule and attempts `wireguard` group
creation/user enrollment. Log out/in after membership changes. The `.deb` and
other native recipes ship the rule **without** enrollment hooks, so an
administrator must separately add trusted users:

```sh
sudo groupadd -f wireguard
sudo usermod -aG wireguard "$USER"
```

Use the distro's group tools if these commands are unavailable. Group members
can manage all tunnels and read their keys. Passwordless polkit access also
requires an active local session; an SSH session does not automatically qualify.
Normal local policy may request administrator authentication or deny access
when the rule does not match.

Source/bundle helpers live under `/usr/local/lib/wireguard-gui`; native-package
helpers under `/usr/lib/wireguard-gui`. The `.deb` installs the rule under
`/etc/polkit-1/rules.d`, while AUR/RPM/APK/Void recipes use
`/usr/share/polkit-1/rules.d`. See [packaging](../packaging/PACKAGING.md).
The installer labels and checks the helper on SELinux systems (see
[SELinux](#selinux)); AppArmor or other local policy can impose additional
restrictions, so use your distro's security logs when the correct path/grant
still fails.

## 4. DNS compatibility

A config containing `DNS =` causes `wg-quick` to use a `resolvconf` compatibility
command. A running systemd-resolved service alone does not prove that this
command exists. Conversely, a command that resolves to the `resolvectl` shim
requires its systemd-resolved service to be active.

The GUI's read-only check classifies native resolvconf, an active/inactive
resolvectl shim, or a missing compatibility command. Its setup command selection
uses `systemd-resolved` on `dnf`/`yum`, `systemd-resolvconf` on apt-based systems
with active resolved, and `openresolv` otherwise. Verify availability on your
particular release; the detector is not a package-repository compatibility test.

For a Debian-family system that uses openresolv:

```sh
sudo apt-get install openresolv
```

For an Arch-family system that uses openresolv:

```sh
sudo pacman -S openresolv
```

When using the resolvectl shim, check the actual service:

```sh
systemctl is-active systemd-resolved.service
```

Enabling/reconfiguring a resolver changes host DNS policy; review that choice
with the system administrator. Setup's **Fix automatically** can offer to enable
an inactive detected shim's service. The current GUI installer has a simpler
best-effort resolver check: if `resolvconf` already exists it returns, and it
does not itself diagnose/enable an inactive shim. Re-check DNS after installing.

## 5. What Setup can and cannot fix

Setup appears at startup for critical missing requirements. **Show commands**
shows suggestions; **Re-check** refreshes checks; **Skip for now** opens the app.
There is no Setup item in the main app menu. Helper checks use the backend-selected trusted executable. A polkit rule is
reported as an unverified authorization warning; it does not prove session or
group membership.

After you click **Fix automatically**, it runs selected commands with `pkexec`
to install tools/a DNS provider, enable a detected inactive resolved service, or
create `/etc/wireguard` mode 0700. It does not install or authorize the helper,
create/import a config, connect a tunnel, configure server forwarding/NAT or
enable a tunnel's boot service.

## 6. Boot, logs and persistence

**Start on boot** calls systemd to enable/disable `wg-quick@<name>`. It does not
start a currently inactive interface or configure desktop login autostart.
On OpenRC/runit or another init system, use that system's own service management;
the GUI has no alternative boot-service implementation.

**Log** reads `journalctl` helper and `wg-quick@*` entries. On a system without
journald, the app cannot provide those journal entries. Log Clear only clears
the in-app buffer; Copy/Save use the full loaded log, not just filtered rows.

| State | Persistence |
|---|---|
| Installed packages/helper/authorization | Files persist until removed or replaced |
| Saved tunnel configs | Files persist under `/etc/wireguard` |
| Active interface | Runtime state; use a reviewed boot service if required |
| Systemd enablement | Boot-service setting persists until disabled |
| UI ZIP backups | Files in the user's shared `wireguard/backups` data directory |
| Helper kill-switch rules | Runtime rules; no persistence service is installed |

A helper deactivation/delete/rename retains protection when stop fails or state
is unknown, and reports errors rather than claiming cleanup succeeded. External
service actions may leave rules blocking traffic. Restarting the app or merely
activating does not rebuild them. Disable the kill switch through the helper/UI
when recovering. When upgrading from before 1.9.2, reapply enabled rules once
(off/on), with a planned brief interval without those rules. An external firewall service
may restore its own saved rules after reboot; the GUI does not control that.

## 7. Client versus server policy

An ordinary outbound VPN client needs correct tools, keys, routes and DNS
integration. Do not automatically apply server/gateway configuration to it.
A server/gateway may additionally need a permitted inbound UDP listen port,
forwarding and NAT/routing appropriate to its network. Those are administrator
policy decisions; this app is not a server provisioning wizard.

The helper-managed kill switch does change host outbound firewall rules when
you enable it. It does not configure a server's inbound port or masquerading.
Manage permanent firewall/forwarding policy with your distro's tools and test
it against the actual network. The GUI helper rejects saved script hooks, so a
PostUp/PostDown NAT example cannot be saved through the app. Any root-managed
hook config remains root code and runs normally when activated by wg-quick.

Avoid configuring the same interface through NetworkManager and this app at the
same time. Keep declarative distro/service management and direct file edits
consistent rather than assuming the GUI overrides other system policy.

## 8. Install records, updates and rollback

`install.sh` records how it installed the app in a root-owned, world-readable
file next to the helper, `PREFIX/lib/wireguard-gui/install-manifest` (by default
`/usr/local/lib/wireguard-gui/install-manifest`). It holds the installation method,
prefix, authorization mode (`sudoers`, `polkit` or `none`), the account the
grant belongs to, the version, and the group/SELinux/directory
changes the installer made so `uninstall` can undo exactly those.

- **Re-running the installer keeps your choices.** Without `--sudoers` or
  `--polkit` a later run (or an update) keeps the recorded authorization mode
  instead of falling back to sudoers. Installs made by earlier releases have no
  record; the installer then detects the grant already on disk.
- **The in-app updater keeps the installation method.** It updates only a copy
  installed by `install.sh`, and passes the recorded prefix and authorization
  mode to the verified release installer. It refuses, before downloading
  anything, when the running copy belongs to a distro package (detected with
  `dpkg-query -S`, `rpm -qf`, `pacman -Qo`, `apk info --who-owns` or
  `xbps-query -o`, or the package's own manifest) and names the package-manager
  command to use instead; AUR packages are pointed at a rebuild after
  `pacman -Syu`. A `cargo install` copy is told to re-run `cargo install`, and a
  copy whose origin is unknown (for example a binary run from a build tree) is
  not touched.
- **No duplicate installs.** If a distro package already provides
  `/usr/bin/wireguard-gui` or `/usr/lib/wireguard-gui/wg-helper`, `install.sh` refuses to add
  a second copy under `/usr/local` (both would be on `PATH` with separate
  helpers and grants) and names the package to remove. `--force` installs anyway
  and is remembered for later updates. Installing with `PREFIX=/usr` refuses to
  overwrite package-owned files, and warns when a `/usr/local` copy shadows it.
  The AUR, RPM, Alpine and Void packages warn after installation when a
  `/usr/local` copy exists.
- **Failed installs roll back.** Before it changes anything, the installer
  backs up the app, alias, helper, desktop entry, icon, manifest, sudoers
  drop-in and polkit rule. If any later step fails (or the run is interrupted),
  it restores them, removes a `wireguard` group or membership it created,
  removes directories it created and drops a SELinux rule it added; the
  previous installation keeps working. Packages installed as dependencies are
  kept.
- **Local binaries.** `./install.sh --binaries=DIR` installs `wireguard-gui` and
  `wg-helper` from `DIR` (for example your own build) instead of building or
  using a release bundle.
- **Uninstall.** `./install.sh uninstall` removes the files, grants, the SELinux
  rule and empty directories the installer created, and the `wireguard` group
  or membership it added unless the sibling app still uses the group. Tunnel
  configurations in `/etc/wireguard` are kept.

### Package managers and partial upgrades

The installer installs only packages that are missing, so a repeat run or an
update normally makes no package-manager call. On Arch-family systems it uses
`pacman -S --needed` and never refreshes the databases on its own: refreshing
without upgrading (`pacman -Sy <pkg>`) can leave a rolling system partially
upgraded. If pacman reports missing or stale databases, run `sudo pacman -Syu`
and re-run the installer. On Void it uses `xbps-install -S`; if that fails, run
`sudo xbps-install -Su` first. The in-app updater follows the same rules when it
installs `minisign`.

### SELinux

On SELinux systems (Fedora, RHEL-family, some openSUSE setups) a file written
to `/usr/local/lib` is labelled `lib_t`, and a file moved from `/tmp` keeps a
temporary-file label; sudo or pkexec may then be denied execution of the
helper. After installing, the installer:

1. adds a persistent rule giving the helper an executable label when `semanage`
   is available (`semanage fcontext -a -t bin_t PREFIX/lib/wireguard-gui/wg-helper`),
2. runs `restorecon -F` on every file it installed, and
3. falls back to `chcon -t bin_t` (lost on a full relabel) when `semanage` is
   missing — install `policycoreutils-python-utils` and re-run to make it
   persistent.

It then runs the helper as root once. If execution is denied it stops with a
clear error, restores the previous installation, and points at the audit log:

```sh
sudo ausearch -m avc -ts recent
ls -Z /usr/local/lib/wireguard-gui/wg-helper
sudo semanage fcontext -a -t bin_t /usr/local/lib/wireguard-gui/wg-helper
sudo restorecon -v /usr/local/lib/wireguard-gui/wg-helper
```

The same check reports a helper that cannot run at all, for example a glibc
build on a musl system (Alpine) or a `PREFIX` on a `noexec` mount. Distro
packages install the helper under `/usr/lib/wireguard-gui/`, which the distro policy
labels; uninstall removes the rule the installer added. Do not disable SELinux
to work around a denial.

## 1.9.3 integration requirements

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

Repeating `--polkit` checks the existing `wireguard` group and account
membership before invoking group-management tools. An already configured
Alpine/BusyBox system can reinstall without optional Shadow `groupadd`/`usermod`.
Missing groups or membership must still be created and verified before the
previous authorization is removed.

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

Only **one helper-managed kill switch** can be active at a time across managed
tunnels. Turn the existing one off before enabling another. The helper requires
an already-active interface; it can assign an unused runtime WireGuard fwmark
without changing the saved config. It does not activate a stopped tunnel as a
side effect of enabling a switch.

The helper prefers nftables. It keeps the kill switch in its **own table**,
`inet wg_helper_killswitch`, with its own OUTPUT base chain, and replaces that
table in one nft transaction. It never edits another table, so firewalld, a
distro ruleset or your own rules stay untouched. The fallback uses a dedicated
`WGH-KILLSWITCH` chain reached by one jump at the top of `OUTPUT`. It requires
`iptables` and `iptables-restore`; when the kernel exposes IPv6 support it also
requires `ip6tables` and `ip6tables-restore`. Both families are preflighted;
commits are atomic **per family**, not one transaction across IPv4 and IPv6.

The rules allow loopback, packets routed into the selected tunnel, its
transport traffic (identified by the tunnel's runtime fwmark), IPv6
router/neighbor solicitation and neighbor advertisement (hop limit 255) and
MLDv2 reports, DHCP client traffic, and established SSH return traffic when
`SSH_CONNECTION` reaches the helper; everything else leaving the host is
rejected. Without the Neighbor Discovery exception an IPv6 endpoint becomes
unreachable once its neighbor cache entry expires. The exceptions **never
override your deny rules**: nftables `accept` only ends the kill switch's own
chain, and the iptables chain `RETURN`s allowed traffic to `OUTPUT` instead of
accepting it, so later rules still apply. This is host-output control, not
server forwarding/NAT or a general no-leak guarantee. It may disrupt remote
access.

Status is one of `enabled`, `disabled`, `missing` or `mismatch`. **Enabled**
requires complete rules whose mark equals the interface's current fwmark.
**Missing** means the switch was turned on but its rules are gone or
incomplete, for example after a firewall reload (`nft flush ruleset`,
`firewall-cmd --reload`). **Mismatch** means rules exist but are stale: the
mark changed, the interface is down, an older release built them, or the
iptables jump is no longer first in `OUTPUT`. On an active tunnel the app
repairs both automatically (`wg-helper killswitch-repair`), once per loss and
with backoff, and reports the result.

Editing a connected tunnel keeps the kill switch working. `wg syncconf` would
reset the fwmark that wg-quick set at runtime; the helper carries the running
fwmark and listen port into the synced config, refuses a config that changes
`FwMark` on a running tunnel (reconnect to apply it), verifies the mark
afterwards and repairs the kill switch if needed.

Down/delete/rename retain protection if the interface is still active or its
state is unknown, and report stop/cleanup failures. If the interface
disappears without a disconnect, the rules stay and keep blocking traffic
(status `mismatch`) until you disconnect. App exit does not clear rules. The
switch is not persistent: the rules and the record that it was requested live
in the kernel and under `/run`, so both are gone after a reboot. A switch built
by an earlier release (1.9.3 or older) is reported as `mismatch` and rebuilt in the
current layout on the next refresh.
