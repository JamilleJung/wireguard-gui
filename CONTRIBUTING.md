# Contributing to wireguard-gui

Thanks for taking the time to contribute! This is a small, friendly project -
issues, ideas, and pull requests are all welcome.

## Getting set up

```sh
git clone https://github.com/JamilleJung/wireguard-gui.git
cd wireguard-gui
./install.sh            # installs build deps for your distro, or:
cargo build --release   # if you already have the toolchain + dev libs
cargo run --release     # run from source
```

Build prerequisites (the installer handles these automatically): a Rust
toolchain, `pkg-config`, a C compiler, and the dev headers for `fontconfig` +
`libxkbcommon`. See the per-distro table in the [README](README.md).

`cargo run --release` talks to real tunnels. If you want to test changes
without touching production configs, work in a throwaway VM or use a local test
`/etc/wireguard` setup.

## Project layout

| Path | Purpose |
|------|---------|
| `ui/app.slint` | The entire UI (Slint markup). |
| `src/backend.rs` | Privilege handling, `wg` orchestration, config parse + validation. |
| `src/main.rs` | Wires UI callbacks to the backend. |
| `src/bin/wg-helper.rs` | The single privileged entry point (audited Rust helper). |
| `packaging/49-wireguard-gui.rules` | polkit rule (optional auth backend). |
| `install.sh` | Universal build + install. |

## Before you open a PR

CI runs these and **will fail the build** if they don't pass - please run them
locally first:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
bash -n install.sh
shellcheck -S warning install.sh tests/helper-validation.sh tests/installer-sanity.sh
bash tests/helper-validation.sh target/release/wg-helper
bash tests/installer-sanity.sh
```

Guidelines:

- Keep the diff focused; match the surrounding style.
- If you touch `src/bin/wg-helper.rs`, preserve the safety properties: fixed
  paths, strict name validation (no path traversal), argv-based command
  execution, atomic writes, backups before overwrite/delete, and the audit log.
  Add a line to `CHANGELOG.md`.
- UI changes: please attach a screenshot.
- Be mindful of the **Slint quirk on GNOME/Wayland**: setting `background` on a
  `Window` (or any ancestor of a text input) makes `LineEdit`/`TextEdit` render
  blank here - don't reintroduce it on a window that contains text inputs.

## Commit messages

Short imperative subject line, optional body explaining the *why*. Reference
issues with `#123` where relevant.

## Reporting bugs / requesting features

Use the issue templates - they prompt for the distro, package manager, and
version, which makes problems much faster to reproduce.

## License

By contributing, you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
