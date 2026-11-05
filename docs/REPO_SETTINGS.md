# Repository settings

This page documents the GitHub settings that protect `main`, the release tags
and the release signing key of **wireguard-gui**, and why each one exists. The
settings are code: `scripts/apply-repo-settings.sh` applies all of them and
reports drift.

```sh
scripts/apply-repo-settings.sh --dry-run JamilleJung/wireguard-gui   # show differences only
scripts/apply-repo-settings.sh JamilleJung/wireguard-gui             # apply them
```

The script needs `gh` authenticated as a repository admin and `jq`. It reads
every setting first, prints `ok`, `change` or `todo` per item, and writes only
what differs, so it is safe to run repeatedly. Rulesets are matched by name;
a ruleset with one of the names below is rewritten to the definition in the
script (including removal of rules added by hand).

## Required status checks

Ruleset **`main: integrity`** requires these checks, by exact job name, from
the GitHub Actions app (`integration_id` 15368, so a commit status posted by
another integration cannot satisfy them):

| Check | Workflow | What it proves |
|---|---|---|
| `rustfmt` | `ci.yml` | `cargo fmt --all -- --check` |
| `clippy` | `ci.yml` | `cargo clippy --locked --all-targets -- -D warnings` |
| `Tests` | `ci.yml` | fixture check, `cargo test --locked` (unit, property, fixture tests), 20 000-case property run |
| `Release build, CLI & GUI smoke` | `ci.yml` | release build, `--version`/`--help`, headless GUI launch under Xvfb, helper validation, `tests/release-regression.sh` |
| `Shell lint & installer sanity` | `ci.yml` | `bash -n` + ShellCheck of `install.sh`, `tests/*.sh`, `scripts/*.sh`; installer sandbox tests |
| `Firewall namespace test` | `ci.yml` | `tests/firewall-namespace.sh` (nftables/iptables rules in an isolated network namespace) |
| `Kill switch e2e` | `ci.yml` | `sudo bash tests/killswitch-e2e.sh target/release/wg-helper` (real WireGuard, kill switch, live config update) |
| `cargo audit (RUSTSEC advisories)` | `ci.yml` | `cargo audit --deny warnings` |
| `Version consistency` | `ci.yml` | `scripts/check-version.sh` |
| `MSRV` | `ci.yml` | `cargo check --all-targets` + `cargo test` with the minimum Rust version |
| `Fuzz crate check` | `ci.yml` | fuzz targets compile and pass clippy on stable |
| `Latest compatible dependencies` | `deps-compat.yml` | Dependabot PRs only (skipped, and therefore passing, on other PRs): `cargo update`, build, clippy, test, MSRV against the newest graph, `cargo tree -d` |

Plain list (for the ruleset UI):

```text
rustfmt
clippy
Tests
Release build, CLI & GUI smoke
Shell lint & installer sanity
Firewall namespace test
Kill switch e2e
cargo audit (RUSTSEC advisories)
Version consistency
MSRV
Fuzz crate check
Latest compatible dependencies
```

`Coverage` (`ci.yml`), `Contributor onboarding`, `Fuzz *`, `Mutants shard *`
and `Enable auto-merge for low-risk updates` are informational and not
required: they are path-filtered, scheduled, or not meaningful as a gate.

If a job is renamed in a workflow, update `common_checks` in
`scripts/apply-repo-settings.sh` and this table in the same pull request, then
rerun the script. A required check whose job no longer exists blocks every
pull request ("Expected — waiting for status").

## `main: integrity` (no bypass)

| Rule | Setting | Why |
|---|---|---|
| Target | default branch (`~DEFAULT_BRANCH`) | Follows a rename of `main`. |
| Restrict deletions | on | `main` cannot be deleted. |
| Block force pushes | on (`non_fast_forward`) | Published history cannot be rewritten. |
| Require a pull request | on, 0 approvals | Every change to `main` goes through a PR and therefore through CI. With a single maintainer an approval count above 0 would make every PR unmergeable; code-owner review is handled by the next ruleset. |
| Require conversation resolution | on | Review threads, including Copilot comments on security PRs, must be resolved before merge. |
| Required status checks | the list above | Nothing merges unless CI is green. |
| Require branches to be up to date | on (`strict`) | Checks must have run against the current `main`, so two individually green PRs cannot combine into a broken `main`. "Update branch" is enabled on the repository to make this one click. |
| Bypass list | empty | Not even admins can skip CI, push directly or force-push. |

**Merge queue.** GitHub offers merge queues only for repositories owned by
an organization, so this user-owned repository uses strict status checks
instead. The required workflows (`ci.yml`, `deps-compat.yml`) already listen
for `merge_group` events; after a transfer to an organization, add a
`merge_queue` rule to this ruleset and the queue works without workflow
changes.

## Security/Networking review policy

### `main: security review` (admin bypass via pull request only)

A `pull_request` rule with **Require review from Code Owners** and **Dismiss
stale approvals on push**. `.github/CODEOWNERS` assigns these paths to
@JamilleJung:

- `src/bin/` — the privileged helper, including the firewall and kill switch
  rules (`src/bin/wg-helper/firewall/`)
- `src/backend.rs`, `src/backend/` — the helper client and kill switch state
- `src/config.rs`, `src/validation.rs`, `src/secrets.rs`, `src/safe_file.rs`
- `src/update.rs` — signed self-update
- `install.sh`, `packaging/*.rules` — installer and polkit grants
- `minisign.pub`, `.github/` (workflows, Dependabot, CODEOWNERS), `scripts/`,
  this file
- the security tests: `tests/firewall-namespace.sh`, `tests/killswitch-e2e.sh`,
  `tests/helper-validation.sh`, `tests/release-regression.sh`

A pull request that touches any of them needs the code owner's approval;
other pull requests do not. The repository admin role may bypass **this
ruleset only**, and only through a pull request (`bypass_mode:
pull_request`): a sole maintainer cannot approve their own pull request, so
they merge their own security changes with the bypass checkbox, which is
recorded in the audit log. The required checks in `main: integrity` still
apply to them. Contributions from anyone else, and Dependabot updates of
GitHub Actions, need the owner's explicit approval.

### `main: Copilot code review`

A `copilot_code_review` rule (`review_on_push: true`,
`review_draft_pull_requests: false`) asks Copilot to review pull requests to
`main` and to review again on every push. Rulesets cannot be limited to
changed paths, so the rule covers every pull request, which includes all
Security/Networking ones. Copilot leaves comment-only reviews: it never
approves or blocks a merge by itself, but "Require conversation resolution"
means its comments must be resolved (fixed or answered) before the PR can
merge.

**Fallback if the plan or account does not support the rule.** The script
creates this ruleset as optional: when GitHub rejects it (the rule needs
Copilot code review to be available to the repository owner), the script
prints a warning and continues; rerun with `--skip-copilot` to suppress the
attempt. Then:

1. Code-owner routing and the required approval above still apply unchanged.
2. Request Copilot manually on each PR that touches a CODEOWNERS path, from
   the PR's **Reviewers** menu (recent `gh` releases also accept
   `gh pr edit <number> --add-reviewer @copilot`).
3. Alternatively turn on automatic Copilot code review in the account's
   Copilot settings, where the plan offers it; it has the same effect as the
   rule.

## Release tags: `release tags`

Target `refs/tags/v*`; rules `deletion`, `non_fast_forward` and `update`
(no bypass). A published release tag cannot be deleted, moved or re-pointed,
so a signed release always corresponds to one commit. A mistaken tag that
was never released has to be removed by temporarily disabling the ruleset,
which is an explicit, audited action.

## Release environment and signing key

The Release workflow's `Checksums & signature` job runs in the environment
**`release`**, which the script restricts to deployments from tags matching
`v*`. Store the minisign secret key there rather than as a repository
secret, so a workflow run on a branch (or a modified workflow in a pull
request) can never read it:

```sh
gh secret set MINISIGN_KEY --env release --repo JamilleJung/wireguard-gui < minisign.key
gh secret delete MINISIGN_KEY --repo JamilleJung/wireguard-gui
```

The script reports `todo` until the environment secret exists. Required
reviewers can be added to the environment for a manual approval step before
signing.

## Repository and Actions settings

| Setting | Value | Why |
|---|---|---|
| Allow auto-merge | on | Lets `dependabot-automerge.yml` queue low-risk Dependabot PRs; auto-merge waits for every required check and review. |
| Automatically delete head branches | on | Merged branches disappear. |
| Always suggest updating PR branches | on | Pairs with strict status checks. |
| Default `GITHUB_TOKEN` permissions | read, cannot approve PRs | Workflows elevate per job (`contents: write` only in `Publish release` and the auto-merge job). |
| Require actions pinned to a full SHA | on, where the API offers it | Every action is already pinned; this makes it mandatory. |
| Dependabot alerts and security updates | on | Vulnerable dependencies get alerts and fix PRs, which then pass the same gates. |

## Dependabot: never merge with failing CI

- Required status checks apply to Dependabot PRs like any other PR, and
  Dependabot PRs additionally run `Latest compatible dependencies`.
- `.github/workflows/dependabot-automerge.yml` only *enables auto-merge*, and
  only when every updated dependency is a semver patch or minor update of CI
  tooling (GitHub Actions), a Cargo dev-dependency, or the fuzz crate. Runtime
  Cargo dependencies and all major updates always need a manual merge.
  GitHub merges an auto-merge PR only after all required checks pass.
- The workflow uses `pull_request` (not `pull_request_target`), never checks
  out or runs PR code, and holds `contents`/`pull-requests: write` only in
  that one job.
- `.github/dependabot.yml` groups updates by risk; see the comments there.
- The workflow fails closed: before enabling auto-merge it asks the API for
  the base branch's rules and stops (leaving the PR for a human) unless a
  `required_status_checks` rule is enforced. Without that rule
  `gh pr merge --auto` merges immediately instead of waiting.
- Pull requests that touch `.github/` (every GitHub Actions update) also need
  a code-owner review under `main: security review`, so auto-merge waits for
  a maintainer's approval as well as the checks.

### Incident 2026-11-05: Dependabot PR #27 merged before CI finished

**What happened.** The auto-merge workflow and the branch rulesets were added
in the same change, but the rulesets were applied about an hour after the
push. In that window Dependabot opened PR #27 (GitHub Actions group:
`taiki-e/install-action` v2.87.29 → v2.88.0). The workflow ran
`gh pr merge --auto --squash`; with no required checks on `main`, GitHub
merged the PR at once, while `MSRV`, `Kill switch e2e` and
`Latest compatible dependencies` were failing on that commit.

**Impact.** None on users. The PR changed only the pinned action SHA in
`ci.yml` and `mutants.yml`; `86f7a0ffb25f40114606501a311ebcfaf882ea32` was
checked against the upstream `v2.88.0` tag. The failing checks were caused by
problems already on `main` (property tests that did not compile on the MSRV,
and an e2e key file the runner's confined `wg` could not read), not by the
update. They were fixed on `main` and every check has passed since.

**Fixes.** (1) Rulesets with required, up-to-date status checks are now
enforced on `main` (`scripts/apply-repo-settings.sh`). (2) The workflow
refuses to enable auto-merge unless required status checks are enforced.
(3) Code-owner review is required for `.github/`. (4) The MSRV and e2e
failures were fixed and `Latest compatible dependencies` was re-run on
`main`.

**Rule for the future.** Apply repository settings before (or in the same
step as) pushing any workflow that can merge or publish, and run
`scripts/apply-repo-settings.sh --dry-run` to confirm nothing is pending.

## Minimum supported Rust version

Cargo.toml has no `rust-version` yet. The lowest toolchain that builds the
current lockfile is **1.92**; CI's `MSRV` job uses Cargo.toml's
`rust-version` when present and falls back to that value. Adding
`rust-version = "1.92"` to `[package]` makes Cargo report the requirement
to users directly.

