#!/usr/bin/env bash
# Contributor onboarding test: prove that a fresh clone builds, tests and
# lints using exactly the commands CONTRIBUTING.md documents.
#
# The commands are read from the first ```sh block under the "Required checks"
# heading of CONTRIBUTING.md (which starts with scripts/dev-setup.sh), so the
# documentation cannot drift from what works.
#
# Usage:
#   tests/dev-onboarding.sh                 clone the committed HEAD
#   tests/dev-onboarding.sh --from-worktree copy tracked + new unignored files
#                                           (to test uncommitted changes)
#   tests/dev-onboarding.sh --keep          keep the temporary clone
#
# Set CARGO_TARGET_DIR to reuse a build cache (it is linked as ./target, so the
# documented target/release paths keep working); the clone itself stays fresh.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
from_worktree=0
keep=0
for arg in "$@"; do
    case "$arg" in
        --from-worktree) from_worktree=1 ;;
        --keep) keep=1 ;;
        -h|--help) sed -n '2,15p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "dev-onboarding: unknown argument: $arg" >&2; exit 2 ;;
    esac
done
command -v git >/dev/null || { echo "dev-onboarding: git is required" >&2; exit 2; }

work="$(mktemp -d)"
cleanup() { if [ "$keep" = 1 ]; then echo "kept: $work/clone"; else rm -rf "$work"; fi; }
trap cleanup EXIT
clone="$work/clone"

if [ "$from_worktree" = 1 ]; then
    mkdir -p "$clone"
    (cd "$repo" && git ls-files -z --cached --others --exclude-standard) |
        while IFS= read -r -d '' f; do
            [ -e "$repo/$f" ] || continue # deleted in the worktree
            mkdir -p "$clone/$(dirname "$f")"
            cp -p "$repo/$f" "$clone/$f"
        done
    git -C "$clone" init -q
    git -C "$clone" add -A
    git -C "$clone" -c user.name=onboarding -c user.email=onboarding@localhost \
        commit -q --no-verify -m snapshot
else
    git clone -q --no-hardlinks "$repo" "$clone"
fi
echo "dev-onboarding: fresh tree at $clone ($(git -C "$clone" rev-parse --short HEAD))"

cd "$clone"
# The documented commands use ./target paths. To reuse a build cache, point
# ./target at CARGO_TARGET_DIR instead of letting Cargo build elsewhere.
if [ -n "${CARGO_TARGET_DIR:-}" ]; then
    mkdir -p "$CARGO_TARGET_DIR"
    ln -s "$CARGO_TARGET_DIR" target
    unset CARGO_TARGET_DIR
fi
[ -r CONTRIBUTING.md ] || { echo "dev-onboarding: CONTRIBUTING.md missing" >&2; exit 1; }
commands="$work/commands.txt"
awk '
    /^## / { in_section = ($0 ~ /Required checks/); next }
    in_section && !in_block && /^```sh[[:space:]]*$/ { in_block = 1; next }
    in_block && /^```/ { exit }
    in_block && NF && $1 !~ /^#/ { print }
' CONTRIBUTING.md >"$commands"
count="$(wc -l <"$commands" | tr -d ' ')"
if [ "$count" -lt 4 ]; then
    echo "dev-onboarding: could not find the 'Required checks' command block in CONTRIBUTING.md" >&2
    exit 1
fi

grep -qx 'scripts/dev-setup.sh' "$commands" \
    || { echo "dev-onboarding: the Required checks block must start with scripts/dev-setup.sh" >&2; exit 1; }

failures=0
n=0
while IFS= read -r cmd; do
    n=$((n + 1))
    printf '\n== [%d/%d] %s\n' "$n" "$count" "$cmd"
    start=$(date +%s)
    if bash -euo pipefail -c "$cmd" </dev/null; then
        printf 'ok    (%ss) %s\n' "$(($(date +%s) - start))" "$cmd"
    else
        printf 'FAIL  %s\n' "$cmd"
        failures=$((failures + 1))
    fi
done <"$commands"

# The documented checks must not leave the tree dirty (new files,
# rewritten fixtures, formatter changes).
dirty="$(git status --porcelain --untracked-files=all | grep -vE '^\?\? target/?$' || true)"
if [ -n "$dirty" ]; then
    echo "FAIL  the documented commands modified the checkout:"
    echo "$dirty"
    failures=$((failures + 1))
else
    echo "ok    the checkout is still clean"
fi

echo
if [ "$failures" -ne 0 ]; then
    echo "dev-onboarding: $failures documented step(s) failed in a fresh clone" >&2
    exit 1
fi
echo "dev-onboarding: all $count documented commands pass in a fresh clone"
