# 🙏 Contributing to wireguard-gui

Thanks for contributing. This guide describes the current Linux source tree;
[README.md](README.md) covers installation and everyday use.

## 🛠️ Getting set up

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh
```

The source installer installs dependencies, builds the application and helper,
and configures authorization. With prerequisites already available, build
without installing system files:

```sh
cargo build --release --locked
cargo run --release --locked --bin wireguard-gui
```

Use Rust 1.92 or newer (CI's "MSRV" job enforces the minimum; current stable
is recommended), a C compiler/linker, `pkg-config` (or `pkgconf`), and Slint's
system development libraries (`scripts/dev-setup.sh` checks them): fontconfig, xkbcommon and
its X11 integration, OpenGL/EGL, Wayland and D-Bus. `install.sh` contains the
per-package-manager names. The application is Linux-specific; macOS checks of
portable modules do not substitute for the Linux application build.

The Cargo binary is `wireguard-gui`. Only `install.sh` creates the `wg-gui`
symlink. The installer detects extracted prebuilt bundles separately and skips
the build toolchain there; a checkout with Cargo.toml remains a source build.

The application talks to real tunnels and may change routing/firewall state.
Use a disposable Linux VM for privileged integration tests. Unit tests and the
installer fixture tests avoid real tunnel/system installation changes.

## 📁 Project layout

| Path | Purpose |
|---|---|
| `ui/app.slint`, `ui/components/` | Main UI, editor, Help, Updates, diagnostics, Setup and shared controls |
| `src/main.rs` | Startup, tray, UI callbacks, Markdown display and polling |
| `src/ui_bridge/editor_form.rs` | Form representability and multi-peer serialization |
| `src/backend.rs` | Helper client, live data, diagnostics, QR images, ZIP export and backup manager |
| `src/config.rs`, `src/validation.rs` | Config and tunnel-name parsing/validation |
| `src/create.rs` | Interface-only, full-tunnel and split-tunnel presets |
| `src/clipboard.rs`, `src/secrets.rs` | Copy normalization and secret/script handling |
| `src/doctor.rs` | Read-only system checks and setup command selection |
| `src/update.rs` | GitHub checks, signed archive verification and installer update flow |
| `src/bin/wg-helper.rs` | Privileged fixed-verb helper |
| `install.sh` | Source/prebuilt installation and authorization |
| `tests/` | Helper negative checks and sandboxed installer/archive checks |
| `packaging/` | Native package metadata, desktop assets and polkit rule |
| `.github/workflows/` | CI and release automation |
| `docs/` | User guide, embedded Help, distro notes and release information |

## ✅ Required checks

Run on Linux:

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

CI also launches the real GUI under Xvfb/D-Bus to catch startup failures and runs
`cargo audit --deny warnings`. Install `cargo-audit` if needed; its shared policy
is [.cargo/audit.toml](.cargo/audit.toml). Do not suppress a new advisory just to
make CI green. Update dependencies where a fix exists and explain any remaining
upstream exception.

Coverage includes config/name validation, inline/repeated directives, form
round-trips, restore preflight limits, exact signed-checksum selection, private
update directories, helper output/timeout behavior, firewall rule construction,
secret redaction and installer source/prebuilt decisions. Test counts can change;
use the actual run output rather than maintaining a fixed total here.

For a generated tarball, also check its actual installer layout:

```sh
bash tests/installer-sanity.sh install.sh /path/to/wireguard-gui-1.9.3-x86_64-linux.tar.gz
```

## 🛡️ Privileged changes

Add a helper verb only for a concrete operation with a fixed input contract.
Resolve tools from the fixed system directories, validate names before touching
files, use argument arrays, drain command output while waiting, and bound the
operation with a timeout. Do not interpolate caller input into shell commands.

Saved configs use a new `0600` temporary file, `File::sync_all`, rename and a
best-effort directory sync. Preserve backups before destructive changes. Frontend
and helper validation have different roles: the frontend gives detailed errors;
the helper enforces the privileged boundary, including rejection of script hooks.
The helper includes the shared `config.rs` and `secrets.rs` modules so these
checks do not drift; helper hook rejection stays independent.

Keep firewall rules scoped to exact tunnel comments. Test rule ordering and
both IPv4/IPv6 paths; unit tests of generated arguments do not prove behavior on
a host with an arbitrary firewall. Integration tests belong in a disposable VM.
See [SECURITY.md](SECURITY.md).

## 🎨 UI and documentation changes

Use existing Slint components and keep input text legible under the forced light
palette. Preserve raw text whenever the structured form cannot represent every
field, comment or repeated directive. Advanced actions belong in the visible
per-tunnel disclosure; there is no global Easy/Advanced mode.

`docs/help.md` and `CHANGELOG.md` are embedded with `include_str!` and displayed
through the small Markdown renderer. Use simple headings, bullets, paragraphs,
fenced code and links with useful labels. Tables are not rendered as tables in
the in-app view. Rebuild to include changes; changing repository docs does not
replace the Help text embedded in already published binaries.

Check relative links and shell snippets, update the appropriate current guide,
and record unreleased documentation work under `[Unreleased]` in CHANGELOG.md.
Preserve historical entries as a record of their release. The GUI and TUI are
separate applications; verify each one's controls and CLI independently.

## 🔧 Helper development

Debug builds permit a development helper override:

```sh
cargo build --bin wg-helper --locked
WG_HELPER=/absolute/path/to/wg-helper cargo run --locked --bin wireguard-gui
```

This can invoke real privileged operations. Release overrides additionally need
`WG_ALLOW_UNSAFE_HELPER=1` and an absolute, root-owned regular file with neither
group nor other write permission. An override does not grant authorization for
that new path. Avoid running the GUI itself as root for routine development.

## Testing infrastructure

### Property tests and fixtures

`tests/prop_*.rs` are dependency-free property tests. The binaries have no
library crate, so each test pulls the module it checks straight from `src/`
with `#[path = "../src/config.rs"] mod config;` (the helper uses the same
technique). A small deterministic PRNG in `tests/prop_support/` produces the
inputs; a failure prints the seed and case to replay it:

```sh
cargo test --locked --test 'prop_*'                       # default 2000 cases each
PROP_CASES=50000 cargo test --locked --release --test prop_config
PROP_SEED=0x1234 cargo test --locked --test prop_secrets  # another sequence
```

Invariants covered: parsing and validation never panic on arbitrary or
mutated input; anything accepted satisfies what its consumers rely on (valid
keys, peers with AllowedIPs, well-formed endpoints); random wg-quick
configs are accepted and parse/render round-trips are stable; LF and CRLF
give the same verdict; no PrivateKey/PresharedKey value survives redaction
in any casing, spacing or log-prefix form, while other lines stay unchanged;
every `src/create.rs` template validates once completed; sanitized import
names are always valid and idempotent; and the frontend name validator agrees
with the built `wg-helper` on every random name.

`tests/fixtures/configs/` holds **fake** sample configs (`valid/`, `hooks/`,
`invalid/`) for tests, fuzzing and manual experiments. Keys are derived from
SHA-256 of fixed labels. Never add a real config there; change
`scripts/gen-fixtures.sh` and rerun it instead. CI runs
`scripts/gen-fixtures.sh --check` to prove the files match the script output.

### Fuzzing

`fuzz/` is a standalone cargo-fuzz crate (own workspace and lockfile) with
targets `config_validate`, `redact`, `tunnel_names` and `import_preflight`.
They include the same `src/` modules by path. Running them needs a nightly
toolchain and cargo-fuzz:

```sh
rustup toolchain install nightly
cargo install cargo-fuzz --locked
mkdir -p fuzz/corpus/config_validate
cp tests/fixtures/configs/*/*.conf fuzz/corpus/config_validate/
cargo +nightly fuzz run config_validate -- -max_total_time=300
```

CI checks that the targets compile on stable for every pull request ("Fuzz
crate check") and fuzzes each target for five minutes nightly
(`.github/workflows/fuzz.yml`, crash reproducers are uploaded as artifacts).
The ZIP backup and QR container parsers are private functions in the
backend and are not reachable without a library crate; the fuzz targets cover
the validation every imported config passes through.

### Mutation testing

`cargo mutants` checks that the tests notice changes to validation and
security-critical code: `src/validation.rs`, `src/config.rs`,
`src/secrets.rs` and the helper under `src/bin/wg-helper/`. It runs weekly
and on demand (`.github/workflows/mutants.yml`, four shards, report in the job
summary and the `mutants-shard-*` artifacts). Locally:

```sh
cargo install cargo-mutants --locked
cargo mutants --timeout 300 --file src/validation.rs     # one file, quick
cargo mutants --list --file 'src/bin/wg-helper/**/*.rs'  # preview the helper's mutants
```

A "missed" mutant names a change no test detects. Add a test that fails for
it, or mark the function `#[mutants::skip]` with a comment explaining why the
mutation is equivalent. Never run mutation testing of the helper on a host
whose firewall or tunnels matter.

### Release checks

| Script | Purpose |
|---|---|
| `scripts/check-version.sh [--tag vX.Y.Z]` | Cargo.toml, Cargo.lock, top CHANGELOG release, packaging versions (and tag) agree |
| `tests/release-regression.sh APP HELPER` | Pins the CLI contract: `--version`/`--help` format, exit codes, helper rejections |
| `scripts/stage-tarball.sh VERSION ARCH [DIR]` | Builds a reproducible release tarball (fixed modes, root ownership) |
| `scripts/verify-release.sh --tag vX.Y.Z DIR` | Verifies signature, checksums, archive contents, linkage and embedded version |
| `scripts/apply-repo-settings.sh [--dry-run] OWNER/REPO` | Branch rulesets and repository policy ([docs/REPO_SETTINGS.md](docs/REPO_SETTINGS.md)) |

Verify a published release yourself with
`scripts/verify-release.sh --tag vX.Y.Z --download` (needs `gh`, `minisign`,
`python3`; `upx` and `dpkg-deb` enable the binary and package checks).
Bumping the version means updating Cargo.toml, Cargo.lock (`cargo update -w`),
the CHANGELOG heading and the four packaging recipes together; CI's "Version
consistency" check fails otherwise.

### Onboarding test

`bash tests/dev-onboarding.sh` clones the repository into a temporary
directory and runs `scripts/dev-setup.sh` plus every command in the "Required
checks" block above, then fails if any command fails or leaves the checkout
dirty. Use `--from-worktree` to include uncommitted changes. It runs in CI
(`.github/workflows/onboarding.yml`) whenever this file, the scripts or the
build configuration change, and weekly, so these instructions stay accurate.

## Audit regression expectations

Test all repeated fields and exact config schema; use canonical key fixtures,
not merely 44 characters with an equals sign. Exercise supported Table/FwMark/
SaveConfig/IPv6 variants as well as invalid controls, CIDRs and keepalive bounds.
Keep restore preflight and actual saves aligned through the shared validator.

Regression tests cover failed/partial exports preserving old files, other
hardlink names, symlink directories and linked backups, hidden ZIP duplicates,
QR grid selection/limits, runtime-only peers, bounded output/blocked input,
escaped pipe holders and PID lifetime. The helper serializes mutations with a
2-second lock deadline and refuses unsafe root-managed file types/ownership.
Keep the child leader unreaped until pipe work finishes if cleanup may signal
its process group; otherwise PID reuse can target an unrelated process.

Firewall argument tests do not replace disposable Linux namespace integration.
One managed switch is allowed; nft updates are one batch, iptables families are
preflighted and committed separately. Preserve failure reporting and protection
when stop state is unknown. Never run privileged integration tests on real
production tunnels or a shared host firewall.

## 🐛 Issues and contributions

Please follow the [Code of Conduct](CODE_OF_CONDUCT.md). Accessibility barriers
can be reported using the guidance in [ACCESSIBILITY.md](ACCESSIBILITY.md).

Describe the version/commit, distro, steps, expected and actual behavior. Redact
private keys, preshared keys, QR screenshots and production configs. Report
exploitable issues privately using [SECURITY.md](SECURITY.md). Keep changes
focused, explain the resulting behavior, and include relevant validation.

Contributions use the project's [MIT License](LICENSE). This is an independent,
unofficial WireGuard client.
