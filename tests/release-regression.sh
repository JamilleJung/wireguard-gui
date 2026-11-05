#!/usr/bin/env bash
# Release regression: pin the command-line contract of the built binaries so a
# release cannot silently change it. Runs against any build (debug, release,
# UPX-packed, foreign-arch under QEMU binfmt).
#
# Usage:
#   tests/release-regression.sh [--version X.Y.Z] [--runner CMD] APP HELPER
#
#   APP      the frontend binary (wg-tui or wireguard-gui)
#   HELPER   the matching wg-helper binary
#   --version  expected version (default: Cargo.toml [package] version when
#              run from a checkout; otherwise only the format is checked)
#   --runner   command prefix to execute foreign binaries, e.g. qemu-aarch64
#
# Contract (changing any of it is a deliberate, documented CLI change):
#   APP --version / -V   exit 0, stdout exactly "<app> X.Y.Z", nothing on stderr
#   APP --help / -h      exit 0, first line "<app> X.Y.Z - ...", lists usage
#   APP <unknown>        exit 2, "unknown argument" on stderr, empty stdout
#   wg-tui doctor        exit 0, 1 or 2, prints the checklist
#   HELPER               every rejection exits 2 with "wg-helper: <reason>" on
#                        stderr and empty stdout: no verb, unknown verb, wrong
#                        arity, and invalid names for every name-taking verb
set -euo pipefail

expected_version=""
runner=()
while [ $# -gt 0 ]; do
    case "$1" in
        --version) expected_version="${2:?--version needs a value}"; shift 2 ;;
        --runner) read -r -a runner <<<"${2:?--runner needs a value}"; shift 2 ;;
        -h|--help) sed -n '2,24p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --) shift; break ;;
        -*) echo "release-regression: unknown option $1" >&2; exit 2 ;;
        *) break ;;
    esac
done
[ $# -eq 2 ] || { echo "usage: $0 [--version X.Y.Z] [--runner CMD] APP HELPER" >&2; exit 2; }
app="$1"
helper="$2"
for bin in "$app" "$helper"; do
    [ -f "$bin" ] && [ -x "$bin" ] || { echo "release-regression: not an executable: $bin" >&2; exit 2; }
done

if [ -z "$expected_version" ]; then
    manifest="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/Cargo.toml"
    if [ -r "$manifest" ]; then
        expected_version="$(awk '/^\[/{p=($0=="[package]");next} p&&$1=="version"{gsub(/"/,"",$3);print $3;exit}' "$manifest")"
    fi
fi

app_name="$(basename "$app")"
app_name="${app_name%.full}"
case "$app_name" in
    wg-tui|wireguard-gui) ;;
    *) echo "release-regression: unexpected app binary name '$app_name'" >&2; exit 2 ;;
esac

limit=()
if command -v timeout >/dev/null 2>&1; then
    limit=(timeout 120)
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
failures=0
checks=0

# run <binary> <args...>: sets rc, and $work/out, $work/err
run() {
    local bin="$1"
    shift
    set +e
    ${limit[@]+"${limit[@]}"} ${runner[@]+"${runner[@]}"} "$bin" "$@" </dev/null >"$work/out" 2>"$work/err"
    rc=$?
    set -e
}
pass() { checks=$((checks + 1)); printf 'ok    %s\n' "$*"; }
fail() {
    checks=$((checks + 1))
    failures=$((failures + 1))
    printf 'FAIL  %s\n' "$*"
    printf '      rc=%s stdout=%q stderr=%q\n' "$rc" "$(head -c 300 "$work/out")" "$(head -c 300 "$work/err")"
}
expect() {
    # expect <description> <condition...>
    local what="$1"
    shift
    if "$@"; then pass "$what"; else fail "$what"; fi
}
rc_is() { [ "$rc" -eq "$1" ]; }
stdout_empty() { [ ! -s "$work/out" ]; }
stderr_empty() { [ ! -s "$work/err" ]; }
stdout_has() { grep -qF -- "$1" "$work/out"; }
stderr_has() { grep -qF -- "$1" "$work/err"; }
stdout_matches() { grep -qE -- "$1" "$work/out"; }

version_re='[0-9]+\.[0-9]+\.[0-9]+'
want_version="$version_re"
if [ -n "$expected_version" ]; then
    want_version="${expected_version//./\\.}"
fi

echo "== $app_name ($app)"
# Fail fast with one clear message when the binary cannot run here at all
# (wrong architecture without binfmt, missing loader or too-old glibc).
for bin in "$app" "$helper"; do
    run "$bin" --version
    if [ "$rc" -eq 126 ] || [ "$rc" -eq 127 ] || grep -qE 'not found|cannot execute|Exec format error' "$work/err"; then
        echo "release-regression: cannot execute $bin on this host:" >&2
        head -c 500 "$work/err" >&2
        exit 2
    fi
done
for flag in --version -V; do
    run "$app" "$flag"
    expect "$app_name $flag exits 0" rc_is 0
    if [ -n "$expected_version" ]; then
        expect "$app_name $flag prints exactly '$app_name $expected_version'" \
            test "$(cat "$work/out")" = "$app_name $expected_version"
    fi
    expect "$app_name $flag output is one well-formed line" \
        sh -c '[ "$(wc -l <"$1")" -eq 1 ] && grep -qE "^$2 $3\$" "$1"' _ "$work/out" "$app_name" "$version_re"
    expect "$app_name $flag writes nothing to stderr" stderr_empty
done

for flag in --help -h; do
    run "$app" "$flag"
    expect "$app_name $flag exits 0" rc_is 0
    expect "$app_name $flag first line is '$app_name <version> - ...'" \
        sh -c 'head -n 1 "$1" | grep -qE "^$2 $3 - "' _ "$work/out" "$app_name" "$want_version"
    expect "$app_name $flag documents --version and --help" \
        sh -c 'grep -qF -- "--version" "$1" && grep -qF -- "--help" "$1"' _ "$work/out"
    if [ "$app_name" = wg-tui ]; then
        expect "wg-tui $flag documents doctor and setup" \
            sh -c 'grep -qE "wg-tui doctor" "$1" && grep -qE "wg-tui setup" "$1"' _ "$work/out"
    fi
done

for arg in --bogus-flag nonsense --VERSION; do
    run "$app" "$arg"
    expect "$app_name $arg exits 2" rc_is 2
    expect "$app_name $arg reports an unknown argument" stderr_has "unknown argument '$arg'"
    expect "$app_name $arg prints nothing on stdout" stdout_empty
done

if [ "$app_name" = wg-tui ]; then
    run "$app" doctor
    expect "wg-tui doctor exits 0, 1 or 2 (got $rc)" sh -c '[ "$1" -ge 0 ] && [ "$1" -le 2 ]' _ "$rc"
    expect "wg-tui doctor prints the WireGuard tools check" stdout_has "WireGuard tools"
fi

echo "== wg-helper ($helper)"
helper_rejects() {
    # helper_rejects <description> <expected stderr fragment> <args...>
    local what="$1" fragment="$2"
    shift 2
    run "$helper" "$@"
    if rc_is 2 && stdout_empty && stderr_has "wg-helper: " && stderr_has "$fragment"; then
        pass "helper rejects $what"
    else
        fail "helper rejects $what (want exit 2 and '$fragment')"
    fi
}

helper_rejects "no verb" "no verb"
helper_rejects "an unknown verb" "unknown verb: frobnicate" frobnicate
helper_rejects "--version (no such flag)" "unknown verb: --version" --version
helper_rejects "--help (no such flag)" "unknown verb: --help" --help
helper_rejects "list with a name" "list requires 0 tunnel name(s)" list wg0
helper_rejects "active with a name" "active requires 0 tunnel name(s)" active wg0
helper_rejects "log with a name" "log requires 0 tunnel name(s)" log wg0
helper_rejects "read without a name" "read requires 1 tunnel name(s)" read
helper_rejects "up with two names" "up requires 1 tunnel name(s)" up wg0 wg1
helper_rejects "rename with one name" "rename requires 2 tunnel name(s)" rename wg0

name_verbs=(read dump up down save delete enable disable is-enabled sync showconf persist
    killswitch-status killswitch-enable killswitch-disable)
bad_names=("../../etc/passwd" "/tmp/x" ".." "." "" "name/evil" "bad..name" "-bad"
    "name.conf" "NAME.CONF" "abcdefghijklmnop" "wg0 " " wg0" "wg0;id" 'wg0$(id)' "wg0
x" "é")
for verb in "${name_verbs[@]}"; do
    before=$failures
    for name in "${bad_names[@]}"; do
        run "$helper" "$verb" "$name"
        if ! { rc_is 2 && stdout_empty && stderr_has "invalid tunnel name"; }; then
            fail "helper $verb rejects name $(printf '%q' "$name")"
        fi
    done
    if [ "$failures" -eq "$before" ]; then
        pass "helper $verb rejects ${#bad_names[@]} invalid names with exit 2"
    fi
done
before=$failures
for name in "${bad_names[@]}"; do
    run "$helper" rename wg0 "$name"
    if ! { rc_is 2 && stdout_empty && stderr_has "invalid tunnel name"; }; then
        fail "helper rename rejects new name $(printf '%q' "$name")"
    fi
    run "$helper" rename "$name" wg0
    if ! { rc_is 2 && stdout_empty && stderr_has "invalid tunnel name"; }; then
        fail "helper rename rejects old name $(printf '%q' "$name")"
    fi
done
if [ "$failures" -eq "$before" ]; then
    pass "helper rename rejects invalid old and new names"
fi

echo
if [ "$failures" -ne 0 ]; then
    echo "release-regression: $failures of $checks checks FAILED" >&2
    exit 1
fi
echo "release-regression: all $checks checks passed"
