#!/usr/bin/env bash
set -euo pipefail

helper="${1:-packaging/wg-helper}"

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

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT
mkdir -p "$tmpdir/wg"
sed "s#readonly WG_DIR=.*#readonly WG_DIR=\"$tmpdir/wg\"#" "$helper" > "$tmpdir/helper"
chmod +x "$tmpdir/helper"

key="ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopq="
printf '[Interface]\nPrivateKey = %s\nAddress = 10.0.0.2/32\n\n[Peer]\nPublicKey = %s\nAllowedIPs = 0.0.0.0/0\n' \
    "$key" "$key" | "$tmpdir/helper" save test
test -f "$tmpdir/wg/test.conf"

err="$(mktemp)"
if printf '[Interface]\nPrivateKey = %s\nAddress = 10.0.0.2/32\n' "$key" \
    | "$tmpdir/helper" save bad >/dev/null 2>"$err"; then
    echo "expected invalid config to fail" >&2
    rm -f "$err"
    exit 1
fi
grep -q "invalid config: missing \\[Peer\\]" "$err"
rm -f "$err"

echo "helper validation tests passed"
