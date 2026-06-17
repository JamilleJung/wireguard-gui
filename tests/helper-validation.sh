#!/usr/bin/env bash
set -euo pipefail

helper="${1:-target/release/wg-helper}"

bad_names=(
    "../../etc/passwd"
    "/tmp/x"
    ".."
    "."
    ""
    "name/evil"
    "name\\evil"
    "bad..name"
    "-bad"
    "abcdefghijklmnop"
)

for name in "${bad_names[@]}"; do
    err="$(mktemp)"
    if "$helper" read "$name" >/dev/null 2>"$err"; then
        echo "expected invalid name to fail: '$name'" >&2
        rm -f "$err"
        exit 1
    fi
    if ! grep -q "invalid tunnel name" "$err"; then
        echo "expected invalid-name error for: '$name'" >&2
        cat "$err" >&2
        rm -f "$err"
        exit 1
    fi
    rm -f "$err"
done

err="$(mktemp)"
if PATH="/tmp" "$helper" read "../x" >/dev/null 2>"$err"; then
    echo "expected fixed-PATH invalid-name probe to fail" >&2
    rm -f "$err"
    exit 1
fi
grep -q "invalid tunnel name" "$err"
rm -f "$err"

echo "helper validation tests passed"
