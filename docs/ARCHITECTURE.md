# Architecture

This document describes how `wireguard-gui` is put together, where the
privilege boundary is, and how data moves between the desktop UI, the
privileged helper and the system. Read it before changing anything under
`src/bin/wg-helper/`, `src/backend/` or the installer.

## Processes

| Binary | Runs as | Source |
| --- | --- | --- |
| `wireguard-gui` | the logged-in user | `src/main.rs`, the modules below and `ui/*.slint` |
| `wg-helper` | root (via sudo, pkexec, or directly when already root) | `src/bin/wg-helper/` |

`src/config.rs`, `src/validation.rs` and `src/secrets.rs` are compiled into
both binaries, so the frontend and the helper validate and redact configs with
the same code. The helper is the authority: the frontend validates to give
early feedback, the helper validates again before anything touches
`/etc/wireguard`.

## Frontend modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | entry point: CLI parsing, window and tray setup |
| `state.rs` | shared application state |
| `events.rs` | UI callbacks wired to actions |
| `actions.rs` | user actions that start background operations |
| `ops.rs` | operation state machine (Idle → Running → Succeeded / Failed / Unknown → reconciled); refuses overlapping mutations |
| `draft.rs` | editor draft lifecycle (kept on any failure; Retry / Re-edit / Discard) |
| `live.rs` | periodic refresh, health tracking, automatic kill switch repair with backoff |
| `tray.rs` | system tray (StatusNotifierItem) |
| `ui_bridge/` | Rust ↔ Slint models: editor form, windows, help markdown |
| `ui/*.slint` | the interface: `theme`, `widgets`, `main_window`, `editor`, `dialogs` |
| `theme.rs`, `layout.rs` | default, high-contrast and ASCII themes; window-size breakpoints |
| `backend/` | everything that talks to the helper or reads user files: `helper.rs` (invocation), `error.rs` (typed `HelperError`), `status.rs` (kill switch and tunnel status), `verify.rs` (observed-state checks after actions), `backup.rs` (ZIP export/restore with rollback), `qr.rs`, `health.rs` (per-peer health), `diagnostics.rs` |
| `safe_file.rs` | the one routine for opening user-supplied files (no blocking on FIFOs/devices, regular files only, size limits) |
| `logging.rs` | opt-in structured debug log (`WG_GUI_LOG`), always redacted |
| `doctor.rs` | `wireguard-gui doctor` environment checks |
| `update.rs` | self-update that preserves the install method and authorization mode |

## Data flow of an edit

```
user edits in the editor window ─► draft (kept in memory until saved)
  ─► validate (frontend)          fail → draft kept: Retry / Re-edit / Discard
  ─► helper save NAME < config    fail/timeout → draft kept, state re-read
       └ helper: validate, backup old file, stage+fsync+rename, read back
  ─► helper sync NAME (if running)
       └ keeps runtime FwMark/ListenPort, refuses FwMark changes,
         re-checks the kill switch
                                  fail → "saved file and running tunnel
                                  differ": retry sync / reconnect / keep
  ─► refresh: list, active, dump, kill switch status (the observed state wins)
```

## Health

`backend/health.rs` classifies each peer from successive `wg show dump`
snapshots: handshake age, rx/tx deltas, persistent keepalive and time since
activation. WireGuard only handshakes when there is traffic, so an old
handshake on a quiet tunnel is **Idle**, not a failure. **Handshake stale**
and **Connection failed** require traffic being sent without a fresh
handshake. The system clock is mentioned only as a possible cause, and only
when handshakes are actually failing.

## Privilege boundary

```
 user session                         │ root
                                      │
 frontend (wireguard-gui)              │  wg-helper  (one verb per run)
   ├─ reads/writes user files         │    ├─ /etc/wireguard/*.conf   (0600, root)
   │  (imports, exports, backups      │    ├─ /etc/wireguard/.backup/ (per-save copies)
   │   under $XDG_DATA_HOME)          │    ├─ /etc/wireguard/.helper.lock
   └─ runs the helper through ────────┼──► ├─ /run/wg-helper/killswitch (requested switch)
      sudo -n │ pkexec │ directly     │    └─ wg, wg-quick, nft, iptables*, systemctl,
      (stdin: config text only)       │       journalctl, logger  (fixed paths, empty env)
```

The frontend never runs a WireGuard or firewall tool as root itself. It runs
`wg-helper VERB [NAME [NAME]]`; the helper:

1. accepts only the fixed verbs in `verb_shape()` (`src/bin/wg-helper/main.rs`)
   and tunnel names matching `name_ok()` — no paths, options or shell text;
2. takes the shared mutation lock (`/etc/wireguard/.helper.lock`, bounded
   wait) for every mutating verb, so the TUI and GUI cannot interleave;
3. runs tools only from `/usr/sbin:/usr/bin:/sbin:/bin` with a cleared
   environment, bounded output, and a deadline enforced from the parent;
4. opens configs with `O_NOFOLLOW | O_NONBLOCK`, requires an owned regular
   file with one link and no group/other write bit, and caps input at 1 MiB;
5. validates configs and refuses `PreUp`/`PostUp`/`PreDown`/`PostDown`
   (they would run as root);
6. writes with stage → fsync → rename, keeping a backup of the old file;
7. checks the resulting state after every mutation (interface up/down,
   file present/absent, start-on-boot state, kill switch verified) and, after
   a timeout, reports the state it observed instead of guessing;
8. writes one structured, redacted `key=value` audit record per mutation to
   the journal (`journalctl -t wireguard-gui`).

Authorization is either a sudoers drop-in scoped to the helper path or a
polkit rule scoped to the helper path and the `wireguard` group (see
`packaging/49-wireguard-gui.rules`). Read-only polling never opens an
authentication dialog: with pkexec it first asks `pkcheck` without user
interaction.

## Helper modules

| Module | Responsibility |
| --- | --- |
| `main.rs` | verb table, name validation, lock, outcome logging |
| `process.rs` | fixed-path tool lookup, clean environment, bounded I/O, deadlines |
| `fsutil.rs` | private dirs, lock file, link-safe reads, backups, atomic writes |
| `runtime.rs` | `wg show`/`wg set`: interface state, fwmark, listen port, sync merge |
| `commands.rs` | tunnel verbs (`up`, `down`, `save`, `rename`, `delete`, `sync`, …) |
| `firewall/mod.rs` | kill switch policy, status decision, requested-state marker |
| `firewall/nft.rs` | nftables backend: dedicated `inet wg_helper_killswitch` table |
| `firewall/iptables.rs` | iptables backend: dedicated `WGH-KILLSWITCH` chain |
| `logging.rs` | structured, redacted journal records |

## Kill switch data flow

```
killswitch-enable NAME
  observe()  ── nft list table inet wg_helper_killswitch
             ── nft list chain inet filter output        (older releases)
             ── iptables -S / ip6tables -S
  refuse if another tunnel owns a switch
  ensure_tunnel_has_fwmark(NAME)    (wg show/set; never edits the saved file)
  install    nft: one transaction replacing the dedicated table
             iptables: --test both families, then commit each family
  remove leftovers of the other backend / older layout   (only after install)
  write /run/wg-helper/killswitch
  verify: decide() must say "enabled"
```

`decide()` (pure, unit-tested) turns what was observed into one status:

| Status | Meaning | Frontend reaction |
| --- | --- | --- |
| `enabled` | complete rules whose mark equals the runtime fwmark | none |
| `disabled` | no rules and no request | none |
| `missing` | requested (marker) but rules gone or incomplete | run `killswitch-repair` |
| `mismatch` | rules present but stale: mark differs, interface down, older layout or leftovers | run `killswitch-repair`; if the interface is down, report it |

Why the rules look the way they do:

* **Dedicated table / chain.** nftables `accept` ends only the current base
  chain, so exceptions in a separate table never override another table's
  `drop`. In iptables an `ACCEPT` in a jumped chain is final for the table,
  so the kill switch chain `RETURN`s allowed traffic to `OUTPUT`, where the
  administrator's later rules still apply, and the jump must be the first
  `OUTPUT` rule.
* **Exceptions.** Loopback; packets routed into the tunnel interface
  (unmarked plaintext); WireGuard's transport (the runtime fwmark); IPv6
  router/neighbor solicitation and neighbor advertisement with hop limit 255
  and MLDv2 reports with hop limit 1 (without them an IPv6 endpoint becomes
  unreachable once its neighbor cache entry expires); DHCPv4/DHCPv6 client
  traffic (lease renewal); return traffic of the SSH session the switch was
  enabled from. IPv4 ARP is not IP and is not filtered by these hooks.
* **Live config updates.** `wg syncconf` resets interface fields that the
  file omits — including the fwmark wg-quick sets at runtime. `sync` merges the
  live fwmark and listen port into the stripped config, refuses a config that
  changes the fwmark of a running tunnel (reconnect instead), re-reads the mark
  afterwards and repairs the kill switch if needed.
* **Firewall reloads.** A reload (`nft flush ruleset`, firewalld, a distro
  ruleset) can remove the rules. The marker under `/run` keeps the request, so
  status turns `missing` and the frontend repairs it. The marker is on tmpfs,
  so, like the rules, a kill switch does not survive a reboot.

These behaviours are exercised against real kernels by
`tests/firewall-namespace.sh` (rule batches, atomicity, foreign rules kept)
and `tests/killswitch-e2e.sh` (a real tunnel in network namespaces, IPv4 and
IPv6 endpoints, nftables and iptables).
