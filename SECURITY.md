# Security Policy

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue for
anything exploitable.

- Preferred: GitHub **[Private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new)**
  (Security → Report a vulnerability).
- Or email: **izeystudio@gmail.com**

Please include the version (or commit), your distro + package manager, repro
steps, and impact. Do **not** include real private keys or production configs.
Coordinated disclosure is appreciated — you'll get an acknowledgement as soon as
possible and be kept updated on a fix.

## Supported versions

This is an early project; only the latest release (and `main`) receive fixes.
If you are running an older version, please upgrade before reporting.

## Threat model

`wireguard-gui` manages WireGuard tunnels under `/etc/wireguard`, which requires
root. The design goal is to keep the part that runs as root as small and
auditable as possible, and to keep everything else unprivileged.

**In scope** (things we actively defend against):

- A bug or hijacked environment in the **unprivileged UI** escalating to root.
- The privileged helper being tricked into touching files outside
  `/etc/wireguard` (path traversal) or running attacker-chosen commands.
- A malicious or malformed `.conf`/QR import corrupting existing tunnels or
  being saved without the user understanding what it does.
- Truncated/lost configs from interrupted writes.

**Out of scope** (cannot be defended against here):

- An attacker who is **already root**, or who can already run code **as your
  user** (they can read your keys directly; pinning a binary path doesn't help).
- The security of WireGuard itself, the kernel module, or `wg`/`wg-quick`.
- Physical access / a compromised display server.

## The privilege boundary

The GUI runs as your normal user. **The only thing that runs as root is one
small Rust helper binary**, `wg-helper` (`src/bin/wg-helper.rs` in source),
invoked as `sudo -n wg-helper <verb> [name]` (sudoers mode) or
`pkexec wg-helper …` (polkit / fallback). The GUI binary itself never runs as
root.

Authorisation is scoped to **exactly that one helper path**:

- the **sudoers** drop-in grants passwordless execution of only
  `/usr/local/lib/wireguard-gui/wg-helper` for the installing user;
- the **polkit** rule allows `pkexec` of only that program for an active local
  session.

Because the grant is bound to the absolute helper path, pointing the app at a
different script (e.g. via `$WG_HELPER`) cannot silently gain root — it would
fall outside the sudoers/polkit grant and prompt or fail. In release builds the
helper-path override is additionally refused unless `WG_ALLOW_UNSAFE_HELPER=1`
is set and the target is an absolute, root-owned, non-world-writable file.

### Helper hardening

The helper itself:

- exports a **fixed `PATH`** (`/usr/sbin:/usr/bin:/sbin:/bin`) so a hijacked
  caller `PATH` can't redirect the `wg`/`wg-quick`/`logger` it runs as root;
- **validates every tunnel name** against `^[A-Za-z0-9][A-Za-z0-9_.-]{0,14}$`
  and rejects `.`, `..`, `/`, `\`, so `"$WG_DIR/<name>.conf"` can never escape
  `/etc/wireguard`;
- **no `sh -c`** — all subprocess calls use argv arrays directly;
- **timeouts** — every external call has a `Duration` bound;
- writes configs **atomically** (temp file + best-effort `sync -f` + `rename`,
  mode `600`) and keeps a **timestamped 0600 backup** before any overwrite,
  rename, or delete;
- validates the saved config shape in the helper before save/rename, in
  addition to the unprivileged frontend validation (second check inside the
  privileged boundary);
- **logs every mutating action** (with the invoking user) to the journal
  (`logger -t wireguard-gui`) without key material.

### Kill switch scope

The helper can add/remove tunnel-scoped firewall rules for an active `wg-quick`
tunnel, preferring **nftables** (`inet filter`) with an iptables/ip6tables
fallback. The kill switch never flushes user rules and cleans up on disable.
It does not install a daemon or own the system firewall permanently.
When `$SSH_CONNECTION` is set, it auto-allows established SSH return traffic
to prevent accidental session lock-out.

## Config hooks run as root — only import configs you trust

WireGuard configs may contain `PostUp` / `PreUp` / `PostDown` / `PreDown`
directives, which **`wg-quick` runs as root** when the tunnel is activated. A
malicious `.conf` could therefore run arbitrary root commands on activation.

This is inherent to `wg-quick` (the same risk as running `wg-quick up` on any
config by hand) — it is **not** a flaw specific to this app. To mitigate it,
the editor shows an amber warning whenever a config contains those directives.
Treat importing a `.conf` like running a script: only do it from sources you
trust.

## Private keys and QR codes

- A tunnel `.conf` contains the interface **private key**. Files are written
  `0600`; backups are `0600` in `/etc/wireguard/.backup`. The directory itself
  is mode `0700` and root-owned.
- The **editor** displays the config including the private key in clear text
  by design (same as any WireGuard tool). The app does not log it.
- **Show QR** renders the full config — *including the private key* — as a QR
  code. Anyone who photographs your screen gets the key. Only display it when
  it is safe to do so.
- **Export** writes every tunnel's `.conf` into a `.zip`; that archive contains
  private keys. Store it somewhere safe and delete it when done.

## Supply chain and verifying a download

- Prebuilt release artifacts ship with a `SHA256SUMS` file that is **signed
  with minisign** (`SHA256SUMS.minisig`; public key `minisign.pub`).
- Third-party GitHub Actions are pinned to commit SHAs.
- `linuxdeploy` (AppImage builder) is pinned to a release with a verified
  SHA-256 checksum.
- Verify with:

```sh
sha256sum -c SHA256SUMS --ignore-missing
minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG
```

When in doubt, **build from source** — `cargo build --release` is reproducible
on any supported distro with the Rust toolchain and Slint dev libraries.

## Notes

- The privileged surface is the `wg-helper` binary only. The GUI binary runs
  unprivileged and never escalates.
- Treat the `sudoers`/`polkit` grant as "this local user may control WireGuard
  without a password" — equivalent to the trust you'd place in `wg-quick`.
- Config files contain private keys in clear text (same as upstream WireGuard
  tools). They are stored `0600`, root-owned, in `/etc/wireguard` (mode `0700`).
