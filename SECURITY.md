# 🛡️ Security Policy

## 📧 Reporting a vulnerability

Report exploitable issues privately through
[GitHub private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new)
or **izeystudio@gmail.com**. Include the version/commit, distro, reproduction
steps and impact. Do not send real private keys or production configs.

Only the latest release and `main` receive fixes. The current published release
is **1.9.1**; upgrade older installations when evaluating a reported issue.

## 🔍 Threat model

The project aims to constrain privileged operations to managing plain WireGuard
configs, interfaces, boot settings and its own firewall rules. It validates
untrusted imports and names before saving, rejects managed script hooks, keeps
secrets out of ordinary action logs, and uses private config/export permissions.

This is not per-user tunnel isolation. A user authorized to invoke the helper
can read all managed private keys, replace/delete tunnels, alter host networking,
change boot enablement and affect host outbound traffic. Grant sudoers access
or `wireguard` group membership only to users trusted with that capability.
A compromised account holding this grant has the same capability.

The app cannot protect against an already privileged administrator, a compromised
kernel/WireGuard toolchain, display/screen capture, or clipboard history. Import
validation and dependency audits reduce specific risks; they are not a guarantee
that every configuration or dependency is safe.

## 🔒 Privilege and authorization

Run the GUI as a normal desktop user. Its helper client executes the helper
directly if the process is already root, otherwise tests a `sudo -n` grant and
falls back to `pkexec`. Root execution is supported, but is not needed for normal
use. The source/bundle installer requires an explicit `--allow-root` override
when launched from a direct root login without an invoking normal user.

The helper is not the only possible privileged process: after a user chooses
Setup's automatic fixes or Update now, package-manager commands or the verified
release installer can also run with administrator authorization.

| Installation | Helper | Authorization installed |
|---|---|---|
| `install.sh` default | `/usr/local/lib/wireguard-gui/wg-helper` | `/etc/sudoers.d/wireguard-gui`, scoped to the installing user and helper path |
| `install.sh --polkit` | `/usr/local/lib/wireguard-gui/wg-helper` | `/etc/polkit-1/rules.d/49-wireguard-gui.rules`; attempts group creation/membership |
| Release `.deb` | `/usr/lib/wireguard-gui/wg-helper` | Same rule under `/etc/polkit-1/rules.d`; no group-creation/user-enrollment script |
| AUR/RPM/APK/Void recipes | `/usr/lib/wireguard-gui/wg-helper` | Rule under `/usr/share/polkit-1/rules.d`; no user-enrollment hooks in these recipes |

The polkit rule matches only the `/usr/local/lib/...` and `/usr/lib/...` helper
paths, an **active local session**, and membership in **`wireguard`**. An SSH
session alone does not meet the active-local condition. If the rule does not
match, normal polkit policy applies; authentication may be requested or denied.
It does not automatically grant every logged-in user access.

For a package installation, an administrator can explicitly enroll a trusted
user (run these from that user's normal session, then log out and back in):

```sh
sudo groupadd -f wireguard
sudo usermod -aG wireguard "$USER"
```

Use the corresponding group-management tools on distros without those commands.
Membership is security-sensitive because it grants access to tunnel secrets.

A helper-path override does not create authorization for the new path. Debug
builds accept `WG_HELPER` for development; release builds require
`WG_ALLOW_UNSAFE_HELPER=1` and an absolute, root-owned regular file with neither
group nor other write permission. Installed helper discovery normally uses the
two standard prefixes, then an adjacent binary or development build fallback.

## 🛡️ Helper hardening

- Tools are resolved from `/usr/sbin`, `/usr/bin`, `/sbin`, `/bin`, and child
  commands receive that fixed PATH. The helper exposes fixed verbs and uses
  argument arrays rather than caller-controlled shell expansion.
- A tunnel name is 1–15 ASCII bytes, begins with a letter or number, and otherwise
  contains only letters, digits, `_`, `.` or `-`. Names containing `..` or ending
  in `.conf` are rejected. Callers supply a name, not a destination path.
- Save/rename validate config shape again inside the privileged boundary and
  reject `PreUp`, `PostUp`, `PreDown` and `PostDown` directives.
- Config writes use a new exclusive `0600` temporary file, file sync, rename and
  best-effort directory sync. Overwrite, rename, delete and Save live create a
  timestamped backup under `/etc/wireguard/.backup` first. This does not make a
  multi-config restore transactional.
- Command output is drained while the child runs. On timeout the helper kills
  its process group, including descendants that might hold pipes open.
- Mutating actions use `logger`; config-like private/preshared-key lines are
  redacted in errors/log text. Treat arbitrary user-provided text as sensitive
  when adding new logging paths.

### Script hooks

`wg-quick` can run script directives as root. The helper refuses to save/rename
configs containing them, and backup restore rejects them before any writes.
An editor warning is not permission to bypass that rejection.

An administrator can place files directly under `/etc/wireguard`. Activation
then invokes `wg-quick` normally, including any administrator-installed hooks;
`up` does not sanitize those existing files. Review such configs as root code.

### Kill-switch scope and lifetime

The helper prefers nftables (`inet filter` output chain) when `nft` exists,
otherwise iptables/ip6tables. It allows loopback, the selected tunnel's output
and fwmark, plus established SSH return traffic only if `SSH_CONNECTION` reaches
the helper. A missing fwmark can be set at runtime without modifying the file.
Do not assume a remote SSH session will remain reachable after firewall changes.

Rejection is inserted ahead of existing accept rules. With non-loopback IPv6
present, the iptables fallback refuses to enable without `ip6tables`. These
rules affect host outbound traffic, including LAN destinations outside a split
tunnel, and can interact with existing firewall rules or other kill switches.
They are not a complete policy manager or a general no-leak guarantee.

No daemon or firewall-persistence service is installed. Helper down/delete/rename
attempt rule cleanup. Stopping a tunnel through another service or tool can leave
rules blocking traffic; quitting/restarting the GUI or activating the interface
alone does not rebuild or remove them. Reactivate if necessary to enable the UI
checkbox, then turn the kill switch off, or ask an administrator to run the
installed helper's `killswitch-disable <name>` verb. A system reboot normally
loses runtime rules unless another firewall service restores them.

Upgrading binaries to 1.9.1 does not replace existing rules. Reapply an active
kill switch once by turning it off/on to use corrected rule ordering and exact
comment matching. Plan for the brief unprotected interval while doing so.

## 🔑 Configs, backups, QR and clipboard

Saved tunnel files and helper backups are root-owned `0600`; the helper creates
or tightens `/etc/wireguard` and its `.backup` directory to `0700`.

The Backup tab stores ZIPs in `$XDG_DATA_HOME/wireguard/backups` (absolute paths
only), or `~/.local/share/wireguard/backups`. Newly created directories use `0700`;
archives and exports use `0600` and refuse a symlink at the final destination.
These archives are unencrypted and contain private/preshared keys.

Restore preflights all selected top-level `.conf` entries before saving: unique
valid names, valid config text, no script hooks, at most 1 MiB per config,
16 MiB total config text and 1,024 configs. Non-config entries/directories are
skipped; nested/traversal config paths are rejected. Saves then run sequentially,
so a later filesystem or helper failure can still leave a partial restore.

The editor, Show QR, Copy config, Running cfg, ZIP exports and backups expose
private keys by design. Clipboard managers and screenshots can retain them.
The system must protect the user's session and exported files as well as the
root config directory. Export includes readable saved configs; read failures
can cause individual configs to be omitted, so check the reported count.

## ✅ Downloads, updates and dependencies

The current release workflow signs `SHA256SUMS` with minisign and refuses to
publish without a valid signature. The updater verifies it using a public key
compiled into the application, selects exactly the requested archive's signed
checksum, then stages downloads in an exclusive private temporary directory.
No installer is run before signature/checksum verification succeeds.

Startup update checks and the Updates window contact GitHub over HTTPS using
`curl`; update requests include the application version in the User-Agent.
The update request does not upload tunnel configs. Diagnostics may send ICMP
pings and DNS queries. Setup/install may contact distro package repositories,
and source builds fetch Rust tools/crates when needed.

Set `WG_NO_UPDATE_CHECK=1` or create
`${XDG_CONFIG_HOME:-$HOME/.config}/wireguard-gui/no-update` to disable the startup
update check. Manual opening of Updates still checks. See
[download verification](README.md#verify-a-download) and the
[update guide](docs/TUTORIAL.md#updates).

GitHub Actions use pinned commits; release packaging tools are version-pinned
and linuxdeploy's download is checksum-verified. CI tokens are read-only except
for the release publishing job's content permission. Locked dependency builds
improve repeatability, but the project does not promise bit-for-bit identical
binaries on every distro.

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
inspecting its embedded binaries. A source `cargo auditable build --release
--locked` also produces uncompressed auditable binaries.
