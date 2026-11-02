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
Local policy such as SELinux/AppArmor can impose additional restrictions; use
your distro's security logs when the correct path/grant still fails.

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
when recovering; after upgrading to 1.9.2 reapply enabled rules once (off/on),
with a planned brief interval without those rules. An external firewall service
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

## 1.9.2 integration requirements

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
