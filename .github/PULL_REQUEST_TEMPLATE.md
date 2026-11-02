<!-- Thanks for the PR! Keep it focused and describe the *why*. -->

## What & why

<!-- What does this change, and what problem does it solve? Link issues: Closes #123 -->

## Checklist

- [ ] Validation run is recorded below; mark checks that do not apply as N/A
- [ ] `cargo fmt --all -- --check` is clean
- [ ] `cargo clippy --locked --all-targets -- -D warnings` passes
- [ ] `cargo test --locked` passes
- [ ] `cargo build --release --locked` succeeds
- [ ] Dependency changes pass `cargo audit --deny warnings` using `.cargo/audit.toml`
- [ ] Documentation matches the current UI/CLI, and relative links resolve
- [ ] UI change? screenshot attached
- [ ] `wg-helper` touched? safety properties preserved (fixed paths, name
      validation, script-hook rejection, atomic writes, backups, timeouts,
      secret redaction) and `CHANGELOG.md` updated

## Validation

<!-- List the relevant commands/checks actually run and their results. -->
