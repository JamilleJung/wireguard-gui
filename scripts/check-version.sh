#!/usr/bin/env bash
# Verify that every place that states the release version agrees:
#   Cargo.toml [package] version
#   Cargo.lock entry for this package
#   top released CHANGELOG.md entry (an "Unreleased" heading is skipped)
#   packaging/aur/PKGBUILD pkgver, packaging/rpm/*.spec Version,
#   packaging/apk/APKBUILD pkgver, packaging/void/template version
#   the git tag, when one is given or when running on a tag in GitHub Actions
#
# Usage: scripts/check-version.sh [--tag vX.Y.Z] [--root DIR]
# Exit status: 0 consistent, 1 mismatch, 2 usage or unreadable input.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag=""

usage() {
    sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
}

while [ $# -gt 0 ]; do
    case "$1" in
        --tag) tag="${2:?--tag needs a value}"; shift 2 ;;
        --tag=*) tag="${1#--tag=}"; shift ;;
        --root) root="${2:?--root needs a value}"; shift 2 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "check-version: unknown argument: $1" >&2; usage >&2; exit 2 ;;
    esac
done

if [ -z "$tag" ] && [ "${GITHUB_REF_TYPE:-}" = tag ]; then
    tag="${GITHUB_REF_NAME:-}"
fi

cd "$root"
for f in Cargo.toml Cargo.lock CHANGELOG.md; do
    [ -r "$f" ] || { echo "check-version: missing $f" >&2; exit 2; }
done

# First `name`/`version` keys inside the [package] table only.
package_field() {
    awk -v want="$1" '
        /^\[/ { in_pkg = ($0 == "[package]"); next }
        in_pkg && $1 == want && $2 == "=" {
            v = $3; gsub(/"/, "", v); print v; exit
        }' Cargo.toml
}

name="$(package_field name)"
cargo_version="$(package_field version)"
[ -n "$name" ] && [ -n "$cargo_version" ] || {
    echo "check-version: could not read [package] name/version from Cargo.toml" >&2
    exit 2
}

lock_version="$(awk -v want="$name" '
    $0 == "[[package]]" { pkg = ""; next }
    $1 == "name" { pkg = $3; gsub(/"/, "", pkg); next }
    $1 == "version" && pkg == want { v = $3; gsub(/"/, "", v); print v; exit }
' Cargo.lock)"

# "## [1.2.3] - 2026-01-01" or "## 1.2.3"; skip "## [Unreleased]".
changelog_version="$(sed -nE 's/^## \[?v?([0-9]+\.[0-9]+\.[0-9]+[^] ]*)\]?.*/\1/p' CHANGELOG.md | head -n 1)"

failures=0
report() {
    # report <label> <value>
    local label="$1" value="$2"
    if [ -z "$value" ]; then
        printf 'FAIL  %-34s (not found)\n' "$label"
        failures=$((failures + 1))
    elif [ "$value" != "$cargo_version" ]; then
        printf 'FAIL  %-34s %s (expected %s)\n' "$label" "$value" "$cargo_version"
        failures=$((failures + 1))
    else
        printf 'ok    %-34s %s\n' "$label" "$value"
    fi
}

semver_re='^[0-9]+\.[0-9]+\.[0-9]+$'
if ! printf '%s\n' "$cargo_version" | grep -Eq "$semver_re"; then
    echo "FAIL  Cargo.toml version '$cargo_version' is not MAJOR.MINOR.PATCH"
    failures=$((failures + 1))
fi

printf 'ok    %-34s %s\n' "Cargo.toml ($name)" "$cargo_version"
report "Cargo.lock" "$lock_version"
report "CHANGELOG.md (top release)" "$changelog_version"

shopt -s nullglob
optional_field() {
    # optional_field <label> <file> <sed-expression>
    local label="$1" file="$2" expr="$3" value
    [ -e "$file" ] || return 0
    value="$(sed -nE "$expr" "$file" | head -n 1)"
    report "$label" "$value"
}
optional_field "packaging/aur/PKGBUILD pkgver" packaging/aur/PKGBUILD 's/^pkgver=([^[:space:]#]+).*/\1/p'
for spec in packaging/rpm/*.spec; do
    optional_field "$spec Version" "$spec" 's/^Version:[[:space:]]*([^[:space:]]+).*/\1/p'
done
optional_field "packaging/apk/APKBUILD pkgver" packaging/apk/APKBUILD 's/^pkgver=([^[:space:]#]+).*/\1/p'
optional_field "packaging/void/template version" packaging/void/template 's/^version=([^[:space:]#]+).*/\1/p'

if [ -n "$tag" ]; then
    if ! printf '%s\n' "$tag" | grep -Eq '^v[0-9]+\.[0-9]+\.[0-9]+$'; then
        echo "FAIL  tag '$tag' is not vMAJOR.MINOR.PATCH"
        failures=$((failures + 1))
    else
        report "git tag" "${tag#v}"
    fi
else
    echo "skip  git tag                            (not on a tag; pass --tag to check one)"
fi

if [ "$failures" -ne 0 ]; then
    echo "check-version: $failures mismatch(es); every version source must state $cargo_version" >&2
    exit 1
fi
echo "check-version: all version sources agree on $cargo_version"
