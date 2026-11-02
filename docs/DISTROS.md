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
There is no Setup item in the main app menu. Helper/polkit checks use known-file
presence and a sudo probe, not proof of active-local membership or every path
chosen later by the backend.

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

A helper deactivation/delete/rename attempts kill-switch cleanup. External
service actions may leave rules blocking traffic. Restarting the app or merely
activating does not rebuild them. Disable the kill switch through the helper/UI
when recovering; after upgrading to 1.9.1 reapply enabled rules once (off/on),
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
