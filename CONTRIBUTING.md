# 🙏 Contributing to wireguard-gui

Thanks for stopping by! This is a small, hackable project, and issues, ideas,
questions and pull requests are all welcome. You don't need to be a WireGuard
or Slint expert to help: a clearer error message, a missing test or a typo fix
is a real contribution. 💛

> [!TIP]
> **Short on time?** Read [🚀 Setup in three commands](#-setup-in-three-commands),
> [🗺️ Project map](#️-project-map) and [✅ Required checks](#-required-checks-before-you-open-a-pr).
> Everything below that is the deep end, for when you need it.

## 🚀 Setup in three commands

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git && cd wireguard-gui
scripts/dev-setup.sh                      # checks your machine and prints what's missing
cargo run --locked --bin wireguard-gui    # build and launch a debug copy
```

`scripts/dev-setup.sh` checks the Rust toolchain (1.92 or newer, the MSRV that
CI's "MSRV" job enforces; current stable is recommended), a C compiler and
linker, `pkg-config` (or `pkgconf`), Slint's system libraries (fontconfig,
xkbcommon and its X11 integration, OpenGL/EGL, Wayland and D-Bus) and the tools
the checks use, such as `shellcheck` and `python3`. For anything missing it
prints the command to install it. `scripts/dev-setup.sh --install` also adds
missing rustup components and prefetches the locked crates; it never runs
`sudo` or installs system packages for you.

Prefer the whole thing installed system-wide, helper and authorization
included? `./install.sh` does that (see the [README](README.md#️-build-from-source)).

> [!WARNING]
> The app talks to **real tunnels** and can change routing and firewall state.
> Use a disposable Linux VM for anything privileged. Unit tests and the
> installer fixture tests don't touch real tunnels or install anything.

A few facts that save head-scratching:

- The application is Linux-only. Checking portable modules on macOS is no
  substitute for the Linux build.
- The Cargo binary is `wireguard-gui`. Only `install.sh` creates the `wg-gui`
  alias.
- The installer treats a checkout with `Cargo.toml` as a source build; it only
  skips the toolchain for extracted prebuilt bundles.

## 🗺️ Project map

| Path | What lives there |
|---|---|
| `ui/*.slint` | The interface: `theme`, `widgets`, `main_window`, `editor`, `dialogs` (Help, Updates, About, diagnostics, QR, Setup) |
| `src/main.rs`, `src/state.rs`, `src/events.rs` | Startup and CLI, shared state, UI callbacks |
| `src/actions.rs`, `src/ops.rs`, `src/live.rs` | Background operations, the operation state machine, live refresh and kill switch repair |
| `src/draft.rs`, `src/ui_bridge/` | Editor drafts; Rust ↔ Slint models (editor form, windows, Help markdown) |
| `src/backend/` | Helper client, typed errors, status, verification, backups, QR, health, diagnostics |
| `src/import_parse.rs`, `src/qr_isolate.rs`, `src/safe_file.rs` | Import parsing, isolated QR decoding, the one safe file-open routine |
| `src/config.rs`, `src/validation.rs`, `src/secrets.rs` | Config and name validation, secret redaction (compiled into the helper too) |
| `src/create.rs`, `src/clipboard.rs` | Interface-only / full / split tunnel presets, copy normalization |
| `src/theme.rs`, `src/layout.rs`, `src/tray.rs`, `src/logging.rs` | Themes, size breakpoints and shortcuts, tray, the opt-in redacted log |
| `src/doctor.rs`, `src/update.rs` | Setup-window checks, the verified updater |
| `src/ui_tests.rs` | UI tests that drive the real Slint windows on a headless backend |
| `src/bin/wg-helper/` | The privileged helper: verbs, filesystem, processes, runtime, firewall (`firewall/nft.rs`, `firewall/iptables.rs`), logging |
| `install.sh`, `packaging/` | Installer, polkit rule, desktop entry, distro recipes |
| `tests/`, `fuzz/` | Shell suites, property tests, fixtures, fuzz targets |
| `scripts/` | Dev setup, fixtures, version checks, release staging and verification |
| `docs/` | Tutorial, embedded Help, architecture, distro and release notes |

The big picture (privilege boundary, data flow, kill switch design) is in
[docs/ARCHITECTURE.md](docs/ARCHITECTURE.md). Read it before touching
`src/bin/wg-helper/`, `src/backend/` or the installer.

## ✅ Required checks (before you open a PR)

CI runs all of these and fails the build when one doesn't pass. Run them on
Linux:

```sh
scripts/dev-setup.sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
scripts/check-version.sh
scripts/gen-fixtures.sh --check
shellcheck -S warning install.sh scripts/*.sh tests/*.sh
bash tests/helper-validation.sh target/release/wg-helper
bash tests/release-regression.sh target/release/wireguard-gui target/release/wg-helper
bash tests/installer-sanity.sh
target/release/wireguard-gui --version
target/release/wireguard-gui --help
```

> [!NOTE]
> This block is executable documentation: `tests/dev-onboarding.sh` runs it,
> line by line, in a fresh clone (see [Onboarding test](#onboarding-test)). If
> you change a command here, make sure it still works from a clean checkout.

CI also launches the real GUI under Xvfb and D-Bus to catch startup crashes, and
runs `cargo audit --deny warnings` with the shared policy in
[.cargo/audit.toml](.cargo/audit.toml) (install `cargo-audit` to run it
locally). Please don't silence a new advisory just to make CI green: update the
dependency where a fix exists, and explain any remaining upstream exception.

### 📋 The PR checklist

- [ ] The required checks above pass (mark the ones that don't apply as N/A in
      the PR).
- [ ] The diff is focused and matches the surrounding Rust and Slint style.
- [ ] User-facing changes have a line under `[Unreleased]` in
      [CHANGELOG.md](CHANGELOG.md). Historical entries stay as they were.
- [ ] Docs match the current UI and CLI, relative links resolve, and shell
      snippets were tried.
- [ ] UI changes come with a before/after screenshot. The README and tutorial
      images are rendered with `scripts/render-screenshots.sh` from fictional
      tunnels.
- [ ] Touched `wg-helper`? The [helper rules](#-privileged-changes) still hold.
- [ ] No real keys, configs, QR codes or backups anywhere: not in fixtures, not
      in screenshots, not in logs.

Commit messages: a short imperative subject, plus a body that explains *why*
when it isn't obvious. Reference issues with `#123`.

## 🔒 Privileged changes

The helper runs as root, so it is the part of the project that deserves the
most care. Add a helper verb only for a concrete WireGuard operation with a
fixed input shape. Then:

- resolve tools from the fixed system directories, never through a shell, with
  argument arrays and no caller input interpolated into commands;
- validate tunnel names before touching the filesystem;
- drain command output while waiting, and bound every operation with a timeout;
- write saved configs as a new `0600` temporary file, `File::sync_all`, rename,
  then a best-effort directory sync, and keep a backup before destructive
  changes;
- check the resulting state, and never log private keys.

Frontend and helper validation have different jobs: the frontend gives
detailed errors early, the helper enforces the privileged boundary, including
rejection of script hooks. The helper compiles the shared `config.rs` and
`secrets.rs` so the checks can't drift; its hook rejection stays independent.

Keep firewall rules inside the kill switch's own nftables table or iptables
chain, scoped to the exact tunnel. Test rule ordering and both IPv4 and IPv6.
Unit tests of the rule text don't prove behaviour on a host with an arbitrary
firewall: that is what the namespace and end-to-end tests below are for. See
[SECURITY.md](SECURITY.md) for the full threat model.

### 🔧 Running against a development helper

Debug builds accept a helper override:

```sh
cargo build --bin wg-helper --locked
WG_HELPER=/absolute/path/to/wg-helper cargo run --locked --bin wireguard-gui
```

This can run real privileged operations. Release builds additionally need
`WG_ALLOW_UNSAFE_HELPER=1` and an absolute, root-owned regular file with neither
group nor other write permission. An override never grants authorization for
the new path. Please don't run the GUI itself as root for everyday development.

## 🎨 UI and documentation

- Use the existing Slint components and keep input text legible under the forced
  light palette.
- Keep raw config text whenever the structured form can't represent every
  field, comment or repeated directive.
- Advanced actions belong in the per-tunnel **Advanced** disclosure; there is no
  global Easy/Advanced mode.
- Keyboard shortcuts live in `ui/main_window.slint` and the `SHORTCUTS` table in
  `src/layout.rs`; a test fails when `docs/help.md` misses one.

> [!IMPORTANT]
> `docs/help.md` and `CHANGELOG.md` are compiled into the app with
> `include_str!` and shown by a deliberately tiny Markdown renderer
> (`src/ui_bridge/markdown.rs`). It understands `#`/`##`/`###` headings, `-`
> bullets (indent two spaces to nest), `>` quotes, fenced code, `---` rules,
> `**bold**` and `` `code` `` (shown as plain text) and `[label](url)` (only the
> label is shown). It does **not** render tables, HTML, alerts or images, and
> every stray `*` disappears. Each source line becomes one row that wraps on its
> own, so in `help.md` write each paragraph or bullet on a single line. Keep it
> free of emoji: the `ascii` theme only maps the app's own symbols (… → ⧉ ▾ and
> friends). Rebuild to see changes; published binaries keep the Help they were
> built with.

The GUI and the [terminal sibling](https://github.com/JamilleJung/wireguard-tui)
are separate applications; check each one's controls and CLI on its own.

## 🧪 Testing infrastructure

The deep end. 🏊 Everything here runs in CI; this is how to run it yourself.

### Property tests and fixtures

`tests/prop_*.rs` are dependency-free property tests. The binaries have no
library crate, so each test pulls the module it checks straight from `src/`
with `#[path = "../src/config.rs"] mod config;` (the helper and the fuzz crate
use the same trick). A small deterministic PRNG in `tests/prop_support/`
produces the inputs; a failure prints the seed and case to replay it:

```sh
cargo test --locked --test 'prop_*'                       # default 2000 cases each
PROP_CASES=50000 cargo test --locked --release --test prop_config
PROP_SEED=0x1234 cargo test --locked --test prop_secrets  # another sequence
```

CI runs the default suite with `cargo test` and again with 20,000 cases in a
release build. Invariants covered: parsing and validation never panic on
arbitrary or mutated input; anything accepted satisfies what its consumers rely
on (valid keys, peers with AllowedIPs, well-formed endpoints); random wg-quick
configs are accepted and parse/render round-trips are stable; LF and CRLF give
the same verdict; no PrivateKey/PresharedKey value survives redaction in any
casing, spacing or log-prefix form, while other lines stay unchanged; every
`src/create.rs` template validates once completed; sanitized import names are
always valid and idempotent; the frontend name validator agrees with the built
`wg-helper` on every random name; and `tests/prop_import.rs` covers backup ZIP
preflight and QR config selection.

`tests/qr_isolation.rs` runs the real application binary's isolated QR decoder
and checks that a crashing decode stays inside the child process
(`tests/data/qr-decoder-assert.png` is such an image).

`tests/fixtures/configs/` holds **fake** sample configs (`valid/`, `hooks/`,
`invalid/`) for tests, fuzzing and experiments. Their keys are derived from
SHA-256 of fixed labels. Never add a real config there: change
`scripts/gen-fixtures.sh` and rerun it instead. `scripts/gen-fixtures.sh --check`
proves the files match the script.

### Fuzzing

`fuzz/` is a standalone cargo-fuzz crate (its own workspace and lockfile, so
fuzzing dependencies never reach the shipped binaries). Its targets include the
same `src/` modules by path:

| Target | Fuzzes |
|---|---|
| `config_validate` | config parsing and validation |
| `redact` | secret redaction |
| `tunnel_names` | tunnel-name validation and import-name sanitizing |
| `import_preflight` | the checks every imported config passes |
| `backup_zip` | backup ZIP preflight and limits (`src/import_parse.rs`) |
| `qr_select` | the bounded image decode and QR grid selection (`src/import_parse.rs`) |

Running them needs a nightly toolchain and cargo-fuzz:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked
mkdir -p fuzz/corpus/config_validate
cp tests/fixtures/configs/*/*.conf fuzz/corpus/config_validate/
cargo +nightly fuzz run config_validate -- -max_total_time=300
cargo +nightly fuzz run qr_select --features detect -- -max_total_time=300
```

Grid detection in `rqrr` is fuzzed only with `--features detect`, because rqrr
0.11 asserts on some crafted grids. That is exactly why the apps decode QR
images in a child process (`src/qr_isolate.rs`): such a crash only fails that
one import. Without the feature, `qr_select` covers everything the apps run
in-process. `fuzz/seeds/` holds small valid archives and images to start from.

CI lints the fuzz crate on stable for every pull request ("Fuzz crate check")
and fuzzes each target for five minutes every night
(`.github/workflows/fuzz.yml`); crash reproducers are uploaded as artifacts.

### Mutation testing

`cargo mutants` checks that the tests notice changes to the validation and
security-critical code: `src/validation.rs`, `src/config.rs`, `src/secrets.rs`
and the helper under `src/bin/wg-helper/`. It runs weekly and on demand
(`.github/workflows/mutants.yml`, four shards, report in the job summary and the
`mutants-shard-*` artifacts). Locally:

```sh
cargo install cargo-mutants --locked
cargo mutants --timeout 300 --file src/validation.rs     # one file, quick
cargo mutants --list --file 'src/bin/wg-helper/**/*.rs'  # preview the helper's mutants
```

A "missed" mutant is a change no test notices. Add a test that fails for it, or
mark the function `#[mutants::skip]` with a comment explaining why the mutation
is equivalent. Never run mutation testing of the helper on a host whose firewall
or tunnels matter.

### Namespace and kill switch end-to-end tests

Two suites test the firewall code against a real kernel. Both need Linux, root
and the firewall tools; run them in a VM if your host's firewall matters.

```sh
bash tests/firewall-namespace.sh
cargo build --release --locked --bin wg-helper
sudo bash tests/killswitch-e2e.sh target/release/wg-helper
```

- `tests/firewall-namespace.sh` builds the helper's tests as your normal user and
  runs only the firewall test with privilege, inside its own network namespace
  (`sudo unshare --net`). The Rust test refuses to touch the host namespace. It
  checks rule batches, atomicity and that foreign rules are kept. Needs
  `nftables`, `iptables` and `python3`.
- `tests/killswitch-e2e.sh` builds real WireGuard tunnels in disposable network
  **and** mount namespaces (private tmpfs over `/etc/wireguard` and
  `/run/wg-helper`) for every combination of IPv4/IPv6 endpoint and
  nftables/iptables. It checks that bypass traffic is rejected, IPv6 neighbour
  discovery survives a flushed neighbour cache, an administrator deny rule still
  applies, a live config update keeps the fwmark, listen port and kill switch, a
  firewall reload is detected as `missing` and repaired, losing the interface
  doesn't leak, a full disk during save keeps the previous config, and
  disconnecting removes every rule. Needs the `wireguard` kernel module,
  `wireguard-tools`, `iproute2`, `nftables` and `iptables`.

### Installer and packaging suites

These need Docker (or QEMU) and network access, so they run in their own CI
workflows rather than the required checks:

| Script | Proves |
|---|---|
| `tests/install-integration.sh IMAGE BIN_DIR` | install, sudoers → polkit switch, upgrade the way the updater does, rollback at every injected failure, the distro-copy guard and a clean uninstall, in a throwaway container |
| `tests/install-packaging-lint.sh [arch\|fedora\|alpine\|void\|all] [--build]` | the recipes in `packaging/` pass each distribution's own linters (`--build` also builds them) |
| `tests/package-build.sh arch\|rpm\|apk\|void` | the real package builds from the current checkout and installs and runs in a clean container |
| `tests/selinux-vm.sh --binaries=DIR` | `install.sh` in a Fedora VM with SELinux enforcing: labels, AVC denials, grants, re-runs, uninstall and exec-denied rollbacks |

`bash tests/installer-sanity.sh` (in the required checks) covers the installer's
source/prebuilt decisions without Docker. For a freshly staged tarball, also check
its real layout:

```sh
bash tests/installer-sanity.sh install.sh /path/to/wireguard-gui-1.10.0-x86_64-linux.tar.gz
```

### Release checks and dry runs

| Script | Purpose |
|---|---|
| `scripts/check-version.sh [--tag vX.Y.Z]` | Cargo.toml, Cargo.lock, the top CHANGELOG release, packaging versions (and tag) agree |
| `tests/release-regression.sh APP HELPER` | Pins the CLI contract: `--version`/`--help` format, exit codes, helper rejections |
| `scripts/stage-tarball.sh VERSION ARCH [DIR]` | Builds a reproducible release tarball (fixed modes, root ownership) |
| `scripts/verify-release.sh --tag vX.Y.Z DIR` | Verifies signature, checksums, archive contents, linkage and embedded version |
| `scripts/apply-repo-settings.sh [--dry-run] OWNER/REPO` | Branch rulesets and repository policy ([docs/REPO_SETTINGS.md](docs/REPO_SETTINGS.md)) |

Before tagging, run the whole release pipeline as a dry run. It runs every gate
and signs the checksums with a throwaway key, but publishes nothing:

```sh
gh workflow run release.yml --ref <branch> -f version=X.Y.Z
```

Verify a published release yourself with
`scripts/verify-release.sh --tag vX.Y.Z --download` (needs `gh`, `minisign` and
`python3`; `upx` and `dpkg-deb` enable the binary and package checks). Bumping
the version means updating Cargo.toml, Cargo.lock (`cargo update -w`), the
CHANGELOG heading and the four packaging recipes together; CI's "Version
consistency" check fails otherwise. The full procedure is in
[docs/RELEASE_CHECKLIST.md](docs/RELEASE_CHECKLIST.md) and
[docs/RELEASES.md](docs/RELEASES.md).

### Onboarding test

`bash tests/dev-onboarding.sh` clones the repository into a temporary directory
and runs every command in the [Required checks](#-required-checks-before-you-open-a-pr)
block (which starts with `scripts/dev-setup.sh`), then fails if any command fails
or leaves the checkout dirty. Use `--from-worktree` to include uncommitted
changes, and `--keep` to keep the clone for a look around. It runs in CI
(`.github/workflows/onboarding.yml`) whenever this file, the scripts or the build
configuration change, and weekly, so these instructions stay true.

<details>
<summary><b>🔬 Audit regression expectations</b> (read before changing validation, restore, IPC or the firewall)</summary>

- Test all repeated fields and the exact config schema; use canonical key
  fixtures, not merely 44 characters with an equals sign. Exercise the supported
  Table/FwMark/SaveConfig/IPv6 variants as well as invalid controls, CIDRs and
  keepalive bounds. Keep restore preflight and the actual saves aligned through
  the shared validator.
- Regression tests cover failed or partial exports preserving old files, other
  hardlink names, symlink directories and linked backups, hidden ZIP duplicates,
  QR grid selection and limits, runtime-only peers, bounded output and blocked
  input, escaped pipe holders and PID lifetime.
- The helper serializes mutations with a 2-second lock deadline and refuses
  unsafe root-managed file types and ownership. Keep the child leader unreaped
  until pipe work finishes if cleanup may signal its process group; otherwise
  PID reuse can target an unrelated process.
- Firewall argument tests don't replace namespace integration. One managed
  switch is allowed; nft updates are one batch, iptables families are
  preflighted and committed separately. Preserve failure reporting and
  protection when the stop state is unknown. Never run privileged integration
  tests on real production tunnels or a shared host firewall.

</details>

## 🐛 Issues and conversations

Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Accessibility barriers
are very welcome reports; [ACCESSIBILITY.md](ACCESSIBILITY.md) says how.

A good bug report has the version or commit, your distro, the steps, and what
you expected versus what happened. Redact private keys, preshared keys, QR
screenshots and production configs. Report anything exploitable **privately**,
as described in [SECURITY.md](SECURITY.md).

Contributions are under the project's [MIT License](LICENSE). This is an
independent, unofficial WireGuard client. Thanks for making it better! 🐉
