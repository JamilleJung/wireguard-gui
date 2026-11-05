#!/usr/bin/env bash
#
# Install / upgrade / rollback / uninstall integration test, run in a throwaway
# distro container with locally built binaries (no GitHub release needed):
#
#   tests/install-integration.sh <image> <bin-dir> [--platform linux/amd64]
#
# <bin-dir> holds the application binary and wg-helper built for the image's
# CPU and C library (static musl binaries work everywhere). The container gets
# a sudo-capable user, then the test, as that user:
#
#   1. installs an "old" release bundle (version marker 0.0.1) with sudoers,
#   2. switches to polkit, then upgrades the way the in-app updater does and
#      checks the method, prefix, auth mode and desktop choice are kept,
#   3. replays the previous release's updater invocation (no auth flag),
#   4. injects failures at every install step and checks the previous files,
#      grants and group setup are restored exactly,
#   5. checks a distro-package copy blocks a second install unless --force,
#   6. uninstalls and checks nothing is left behind,
#   7. optionally upgrades an install made by the previous release's installer
#      (taken from the git tag in PREVIOUS_TAG, default: the newest v* tag
#      whose installer predates the install manifest).
set -euo pipefail

if [ "${1:-}" != --inner ]; then
    [ "$#" -ge 2 ] || { sed -n '3,22p' "$0"; exit 2; }
    image="$1"; bins="$(cd "$2" && pwd)"; shift 2
    platform=""
    if [ "${1:-}" = --platform ]; then platform="$2"; shift 2; fi
    root="$(cd "$(dirname "$0")/.." && pwd)"
    legacy="$(mktemp -d)"
    trap 'rm -rf "$legacy"' EXIT
    # Default: the newest release whose installer predates the install
    # manifest; that is the upgrade path step 7 exists to cover.
    tag="${PREVIOUS_TAG:-}"
    if [ -z "$tag" ]; then
        for candidate in $(git -C "$root" tag --list 'v*' --sort=-v:refname 2>/dev/null); do
            if ! git -C "$root" show "$candidate:install.sh" 2>/dev/null | grep -q install-manifest; then
                tag="$candidate"
                break
            fi
        done
    fi
    if [ -n "$tag" ] && git -C "$root" show "$tag:install.sh" > "$legacy/install.sh" 2>/dev/null; then
        printf 'Previous installer: %s\n' "$tag"
    else
        rm -f "$legacy/install.sh"
        printf 'No previous release tag available; skipping the legacy-upgrade step.\n'
    fi
    # shellcheck disable=SC2016 # Expanded inside the container.
    exec docker run --rm ${platform:+--platform "$platform"} \
        -v "$root:/repo:ro" -v "$bins:/bins:ro" -v "$legacy:/legacy:ro" \
        "$image" sh -euc '
            if command -v apk >/dev/null; then apk add -q bash >/dev/null; fi
            if command -v xbps-install >/dev/null; then
                xbps-install -Syu xbps >/dev/null 2>&1 || true
                xbps-install -Sy bash >/dev/null
            fi
            exec bash /repo/tests/install-integration.sh --inner'
fi

# ---------------------------------------------------------------------------
# Inside the container (root)
# ---------------------------------------------------------------------------
APP="$(awk '/^\[package\]/ {p=1} p && $1 == "name" {gsub(/"/, "", $3); print $3; exit}' /repo/Cargo.toml)"
APP_BIN="$(awk '/^\[package\]/ {p=1} p && $1 == "default-run" {gsub(/"/, "", $3); print $3; exit}' /repo/Cargo.toml)"
PREFIX=/usr/local
BIN="$PREFIX/bin/$APP_BIN"
HELPER="$PREFIX/lib/$APP/wg-helper"
MANIFEST="$PREFIX/lib/$APP/install-manifest"
DESKTOP="$PREFIX/share/applications/$APP.desktop"
SUDOERS="/etc/sudoers.d/$APP"
POLKIT_RULE="/etc/polkit-1/rules.d/49-$APP.rules"
DESKTOP_OPTIONAL=0
grep -q -- '--no-desktop)' /repo/install.sh && DESKTOP_OPTIONAL=1
FAILURES=0

step() { printf '\n=== %s\n' "$*"; }
pass() { printf 'PASS: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*"; FAILURES=$((FAILURES + 1)); }
check() { local what="$1"; shift; if "$@"; then pass "$what"; else fail "$what"; fi; }
manifest() { sed -n "s/^$1=//p" "$MANIFEST" 2>/dev/null; }
in_group() { case " $(id -nG tester) " in *" wireguard "*) return 0 ;; *) return 1 ;; esac; }
group_exists() { grep -q '^wireguard:' /etc/group; }

as_tester() {
    if command -v runuser >/dev/null 2>&1; then
        runuser -u tester -- "$@"
    else
        su tester -s /bin/sh -c "$(printf '%q ' "$@")"
    fi
}

# Everything an install may touch, plus group state.
snapshot() {
    {
        # fontconfig and ca-certificates create these from their own package
        # scripts when a dependency pulls them in; they are not installer state.
        find /usr/local /etc/sudoers.d /etc/polkit-1/rules.d -xdev \
            ! -path /etc/sudoers.d/00-tester ! -path /usr/local/share/fonts \
            ! -path /usr/local/share/ca-certificates 2>/dev/null \
            | sort | while read -r path; do
            if [ -f "$path" ] && [ ! -L "$path" ]; then
                printf '%s %s %s\n' "$path" "$(stat -c '%u:%g:%a' "$path")" "$(cksum < "$path")"
            elif [ -L "$path" ]; then
                printf '%s -> %s\n' "$path" "$(readlink "$path")"
            else
                printf '%s/ %s\n' "$path" "$(stat -c '%u:%g:%a' "$path")"
            fi
        done
        grep '^wireguard:' /etc/group | cut -d: -f1,4 || true
        printf 'tester groups: %s\n' "$(id -nG tester)"
        ls -d /tmp/"$APP"-install.* 2>/dev/null || true
    }
}

# ---------------------------------------------------------------------------
step "Prepare the container"
if command -v apt-get >/dev/null; then
    export DEBIAN_FRONTEND=noninteractive
    apt-get update -qq
    apt-get install -y -qq sudo pkexec polkitd diffutils ca-certificates >/dev/null
elif command -v dnf >/dev/null; then
    dnf -q install -y sudo polkit diffutils findutils shadow-utils util-linux >/dev/null
elif command -v pacman >/dev/null; then
    # Emulated amd64 containers lack the seccomp support pacman's sandbox needs.
    grep -qx 'DisableSandbox' /etc/pacman.conf || sed -i '0,/^\[options\]/s//[options]\nDisableSandbox/' /etc/pacman.conf
    # Arch requires a full upgrade before installing; the installer never
    # refreshes the databases itself.
    pacman -Syu --noconfirm --needed sudo polkit diffutils >/dev/null
elif command -v apk >/dev/null; then
    # No shadow package: exercises the installer's BusyBox group fallbacks.
    apk add -q sudo polkit diffutils findutils coreutils >/dev/null
elif command -v xbps-install >/dev/null; then
    xbps-install -Sy sudo polkit shadow util-linux diffutils findutils >/dev/null
fi
if command -v useradd >/dev/null; then useradd -m tester; else adduser -D tester; fi
printf 'tester ALL=(ALL) NOPASSWD: ALL\n' > /etc/sudoers.d/00-tester
chmod 440 /etc/sudoers.d/00-tester
grep -Eq '^[@#]includedir /etc/sudoers.d' /etc/sudoers \
    || printf '@includedir /etc/sudoers.d\n' >> /etc/sudoers
command -v pkexec >/dev/null || { echo "pkexec missing in this image"; exit 1; }

# Release-bundle layout, exactly as the release tarballs ship it.
make_bundle() {
    local dir="$1" version="$2"
    mkdir -p "$dir/packaging"
    cp /repo/install.sh /repo/README.md /repo/LICENSE "$dir/"
    cp "/repo/packaging/$APP.desktop" "/repo/packaging/$APP.svg" "/repo/packaging/49-$APP.rules" "$dir/packaging/"
    cp /bins/wg-helper "$dir/wg-helper"
    if [ "$version" = current ]; then
        cp "/bins/$APP_BIN" "$dir/$APP_BIN"
    else
        printf '#!/bin/sh\necho "%s %s"\n' "$APP_BIN" "$version" > "$dir/$APP_BIN"
    fi
    chmod 755 "$dir/$APP_BIN" "$dir/wg-helper"
}
rm -rf /work && mkdir -p /work
make_bundle /work/old 0.0.1
make_bundle /work/new current
# What the updater hands to install.sh: a verified bundle with staged binaries.
mkdir -p /work/new/target/release
cp /work/new/"$APP_BIN" /work/new/wg-helper /work/new/target/release/
chown -R tester /work
NEW_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' /repo/Cargo.toml | head -n 1)"
BASELINE="$(snapshot)"

desktop_flag=()
[ "$DESKTOP_OPTIONAL" = 1 ] && desktop_flag=(--no-desktop)

# ---------------------------------------------------------------------------
step "1. Fresh install of the old bundle (default sudoers grant)"
as_tester bash /work/old/install.sh "${desktop_flag[@]+"${desktop_flag[@]}"}"
check "app installed" test -x "$BIN"
check "helper installed" test -x "$HELPER"
check "manifest: method=installer" test "$(manifest method)" = installer
check "manifest: prefix" test "$(manifest prefix)" = "$PREFIX"
check "manifest: auth=sudoers" test "$(manifest auth)" = sudoers
check "manifest: version marker 0.0.1" test "$(manifest version)" = 0.0.1
check "manifest is root-owned 0644" test "$(stat -c '%u %a' "$MANIFEST")" = "0 644"
check "sudoers grant for tester" grep -qx "tester ALL=(root) NOPASSWD: $HELPER" "$SUDOERS"
check "grant validates" visudo -cf "$SUDOERS"
if [ "$DESKTOP_OPTIONAL" = 1 ]; then
    check "--no-desktop recorded" test "$(manifest desktop)" = 0
    check "no desktop entry" test ! -e "$DESKTOP"
fi

step "2. Switch to polkit, then upgrade the way the in-app updater does"
as_tester bash /work/old/install.sh --polkit
check "manifest: auth=polkit" test "$(manifest auth)" = polkit
check "polkit rule installed" test -f "$POLKIT_RULE"
check "sudoers grant removed" test ! -e "$SUDOERS"
check "group created" group_exists
check "tester enrolled" in_group
check "manifest: group created by installer" test "$(manifest group_created)" = 1
# The updater passes --update and the recorded choices; here deliberately only
# --update, so install.sh must keep polkit and the desktop choice by itself.
as_tester env WG_USE_PREBUILT=1 bash /work/new/install.sh --update
check "upgrade: new app binary" cmp -s /bins/"$APP_BIN" "$BIN"
check "upgrade: new helper" cmp -s /bins/wg-helper "$HELPER"
check "upgrade: manifest version $NEW_VERSION" test "$(manifest version)" = "$NEW_VERSION"
check "upgrade: method kept" test "$(manifest method)" = installer
check "upgrade: prefix kept" test "$(manifest prefix)" = "$PREFIX"
check "upgrade: auth=polkit kept" test "$(manifest auth)" = polkit
check "upgrade: polkit rule kept" test -f "$POLKIT_RULE"
check "upgrade: no sudoers grant appeared" test ! -e "$SUDOERS"
check "upgrade: still enrolled" in_group
if [ "$DESKTOP_OPTIONAL" = 1 ]; then
    check "upgrade: desktop choice kept" test ! -e "$DESKTOP"
fi

step "3. The previous release's updater invocation (no auth flag) keeps polkit"
if [ "$DESKTOP_OPTIONAL" = 1 ]; then
    as_tester env WG_USE_PREBUILT=1 bash /work/new/install.sh --no-desktop
else
    env WG_REAL_USER=tester WG_USE_PREBUILT=1 bash /work/new/install.sh
fi
check "old-updater call: auth=polkit kept" test "$(manifest auth)" = polkit
check "old-updater call: no sudoers grant" test ! -e "$SUDOERS"

step "4. Rollback on failure at every step"
# expect_rollback <point> <installer args...>: the run must fail and leave the
# system exactly as it was.
expect_rollback() {
    local point="$1" before
    shift
    before="$(snapshot)"
    if as_tester env WG_INSTALL_INJECT_FAILURE="$point" bash /work/old/install.sh "$@" >/tmp/out 2>&1; then
        fail "injected failure at $point was ignored"
    elif [ "$(snapshot)" = "$before" ]; then
        pass "failure at $point ($*): previous files, grants and group restored"
    else
        fail "failure at $point changed the system"
        diff <(printf '%s\n' "$before") <(snapshot) || true
        tail -n 5 /tmp/out
    fi
}
# From the polkit install: a switch to sudoers fails at each step.
for point in binaries helper desktop auth selinux manifest; do
    expect_rollback "$point" --sudoers
done
# From a sudoers install without the group: a switch to polkit fails after
# creating the group and enrolling the user, and must undo both.
as_tester bash /work/new/install.sh --sudoers >/dev/null
if command -v groupdel >/dev/null; then groupdel wireguard; else delgroup wireguard; fi
check "precondition: no wireguard group" sh -c '! grep -q "^wireguard:" /etc/group'
expect_rollback auth-group --polkit
expect_rollback manifest --polkit
check "group creation was undone" sh -c '! grep -q "^wireguard:" /etc/group'
as_tester bash /work/new/install.sh --polkit >/dev/null
check "back on polkit for the next steps" test "$(manifest auth)" = polkit
before="$(snapshot)"
# A genuine failure: a helper that cannot run on this system.
make_bundle /work/broken 0.0.2
printf '#!/nonexistent/loader\n' > /work/broken/wg-helper
chown -R tester /work/broken
if as_tester bash /work/broken/install.sh >/tmp/out 2>&1; then
    fail "unrunnable helper was installed"
else
    check "unrunnable helper: clear error" grep -q "cannot run on this system" /tmp/out
    check "unrunnable helper: previous state restored" test "$(snapshot)" = "$before"
fi

step "5. A distro-package copy blocks a second install unless --force"
mkdir -p /usr/bin
fake_pkg=""
if command -v dpkg-deb >/dev/null; then
    mkdir -p /work/fakepkg/DEBIAN /work/fakepkg/usr/bin
    printf 'Package: %s\nVersion: 0.0.1\nArchitecture: all\nMaintainer: test <test@example.invalid>\nDescription: test fixture\n' \
        "$APP" > /work/fakepkg/DEBIAN/control
    printf '#!/bin/sh\n' > "/work/fakepkg/usr/bin/$APP_BIN"
    chmod 755 "/work/fakepkg/usr/bin/$APP_BIN"
    dpkg-deb -b /work/fakepkg /work/fake.deb >/dev/null && dpkg -i /work/fake.deb >/dev/null
    fake_pkg=dpkg
else
    printf '#!/bin/sh\n' > "/usr/bin/$APP_BIN"
fi
if as_tester env WG_USE_PREBUILT=1 bash /work/new/install.sh --update >/tmp/out 2>&1; then
    fail "install next to a package copy accepted without --force"
else
    check "duplicate refused with an explanation" grep -q "already installed by a package" /tmp/out
    [ -z "$fake_pkg" ] || check "refusal names the owning package" grep -q "dpkg package '$APP'" /tmp/out
    check "refusal left the install untouched" test "$(snapshot)" = "$before"
fi
as_tester env WG_USE_PREBUILT=1 bash /work/new/install.sh --force >/tmp/out 2>&1 \
    && pass "--force installs anyway" || { fail "--force install failed"; tail -n 5 /tmp/out; }
check "manifest records --force" test "$(manifest force)" = 1
if [ "$fake_pkg" = dpkg ]; then dpkg -r "$APP" >/dev/null; else rm -f "/usr/bin/$APP_BIN"; fi

step "6. Uninstall leaves nothing behind"
as_tester bash /work/new/install.sh uninstall
after="$(snapshot)"
if [ "$after" = "$BASELINE" ]; then
    pass "system matches the state before the first install"
else
    fail "uninstall left changes behind"
    diff <(printf '%s\n' "$BASELINE") <(printf '%s\n' "$after") || true
fi

step "7. Upgrade an install made by the previous release's installer"
if [ -f /legacy/install.sh ]; then
    make_bundle /work/legacy 0.0.1
    cp /legacy/install.sh /work/legacy/install.sh
    chown -R tester /work/legacy
    if as_tester bash /work/legacy/install.sh --polkit >/tmp/out 2>&1; then
        check "legacy install has no manifest" test ! -e "$MANIFEST"
        as_tester env WG_USE_PREBUILT=1 bash /work/new/install.sh --update
        check "legacy upgrade: manifest written" test "$(manifest method)" = installer
        check "legacy upgrade: polkit detected and kept" test "$(manifest auth)" = polkit
        check "legacy upgrade: no sudoers grant" test ! -e "$SUDOERS"
        as_tester bash /work/new/install.sh uninstall
        check "legacy uninstall: files removed" test ! -e "$BIN" -a ! -e "$HELPER" -a ! -e "$POLKIT_RULE"
    else
        printf 'note: the previous installer failed in this image; skipped.\n'
        tail -n 5 /tmp/out
    fi
else
    printf 'skipped (no previous release installer)\n'
fi

printf '\n%s: %s failure(s)\n' "$(. /etc/os-release && printf '%s' "${PRETTY_NAME:-$ID}")" "$FAILURES"
[ "$FAILURES" -eq 0 ]
