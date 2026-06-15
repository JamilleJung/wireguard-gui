# Security Policy

## Reporting a vulnerability

Please report security issues **privately** — do not open a public issue for
anything exploitable.

- Preferred: GitHub **[Private vulnerability reporting](https://github.com/JamilleJung/wireguard-gui/security/advisories/new)**
  (Security → Report a vulnerability).
- Or email: **izeystudio@gmail.com**

Please include the version (or commit), your distro + package manager, repro
steps, and impact. I'll acknowledge as soon as I can and keep you updated on a
fix. Coordinated disclosure is appreciated.

## Supported versions

This is an early project; only the latest release (and `main`) receive fixes.

## Security model

`wireguard-gui` runs as your normal user. Anything requiring root is funnelled
through one small, auditable script — **`packaging/wg-helper`** — which is the
*entire* privileged surface. The GUI binary itself never runs as root.

Hardening in `wg-helper`:

- **Fixed paths.** `WG_DIR` is hard-coded to `/etc/wireguard`; nothing is taken
  from the caller's environment.
- **No path traversal.** Tunnel names must match
  `^[A-Za-z0-9][A-Za-z0-9_.-]{0,14}$` and may never be `.`/`..` or contain `..`,
  so a name can't escape the config directory.
- **Atomic writes.** Configs are written to a temp file and `rename()`d into
  place (mode `600`), so a crash can't leave a truncated config.
- **Reversible destruction.** Every overwrite and delete first copies the old
  config to `/etc/wireguard/.backup/<name>.conf.<timestamp>`.
- **Audit log.** Mutating actions are logged to the journal
  (`journalctl -t wireguard-gui`).

### Privilege escalation backend

- **sudoers** (default): a drop-in whitelists *only* `wg-helper` `NOPASSWD` for
  the installing user.
- **polkit** (`--polkit`, and the `.deb`): a rule allows running `wg-helper` via
  `pkexec` without a password for an **active local session** only.
- If neither is configured, the app falls back to `pkexec`, which prompts.

### Config hooks run as root — only import configs you trust

WireGuard configs may contain `PostUp` / `PreUp` / `PostDown` / `PreDown`
directives, which **`wg-quick` runs as root** when the tunnel is activated. A
malicious `.conf` could therefore run arbitrary root commands on activation.

This is inherent to `wg-quick` (the same risk as running `wg-quick up` on any
config by hand) — it is **not** a flaw specific to this app. To mitigate it, the
editor shows an amber warning whenever a config contains those directives. Treat
importing a `.conf` like running a script: only do it from sources you trust.

### Notes & scope

- The privileged surface is the `wg-helper` script: fixed `PATH` and `WG_DIR`,
  strict name validation (no shell metacharacters, no path traversal), and no
  use of `eval`/shell-interpolation of inputs (the Rust side spawns it with
  `execve`-style argv, never a shell).
- Config files contain private keys in clear text (same as upstream WireGuard
  tools). They are stored `0600`, root-owned, in `/etc/wireguard` (mode `700`).
  The editor displays them in clear text by design. The audit log records
  actions and tunnel names only — **never** key material.
- Treat the `sudoers`/`polkit` grant as "this local user may control WireGuard
  without a password" — equivalent to the trust you'd place in `wg-quick`.
- Supply chain: prebuilt release artifacts ship with a `SHA256SUMS` file that is
  **signed with minisign** (`SHA256SUMS.minisig`; public key `minisign.pub`).
  Verify with:
  ```sh
  sha256sum -c SHA256SUMS --ignore-missing
  minisign -Vm SHA256SUMS -P RWSrokrj4nWGDhUf409+6yXuqPfF7WQuGtSk/PdsnTWKwfOpb3Hv4DxG
  ```
  Release builds run in GitHub Actions with all third-party Actions **pinned to
  commit SHAs**, and `linuxdeploy` pinned to a release with a verified SHA-256.
