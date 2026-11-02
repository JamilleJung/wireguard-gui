#!/usr/bin/env bash
set -euo pipefail

script="${1:-install.sh}"

bash -n "$script"

require() {
    local pattern="$1"
    local why="$2"
    if ! grep -Eq "$pattern" "$script"; then
        echo "installer sanity failed: missing $why" >&2
        exit 1
    fi
}

reject() {
    local pattern="$1"
    local why="$2"
    if grep -Eq "$pattern" "$script"; then
        echo "installer sanity failed: found $why" >&2
        exit 1
    fi
}

require 'target/release/wg-helper' 'Rust helper install path'
require 'visudo -cf' 'sudoers validation before install'
require 'cargo build --release' 'release build step'
require 'runuser -u "\$REAL_USER"|su - "\$REAL_USER"' 'non-root cargo build handoff'
require 'packaging/49-wireguard-gui\.rules' 'polkit helper rule install'
reject 'packaging/wg-helper' 'deleted shell helper path'

# Exercise the actual installer functions in a private fixture. Privileged
# commands are recorded, so these checks cannot install packages or files on
# the host, even when the test runner itself is root.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    calls="$fixture/package-calls"
    app="wireguard-gui"
    for fn in prebuilt_ready prepare_prebuilt install_pkgs verify_build_deps build_app; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { :; }
    die() { echo "$*" >&2; exit 1; }
    as_root() { printf '%s\n' "$*" >> "$calls"; }
    as_user() { echo "unexpected source build" >&2; exit 1; }
    command() {
        case "$*" in
            '-v cc'|'-v gcc'|'-v pkg-config'|'-v pkgconf')
                echo "unexpected build dependency probe" >&2; exit 1 ;;
            *) builtin command "$@" ;;
        esac
    }

    HERE="$fixture/bundle"
    mkdir -p "$HERE"
    printf 'application fixture\n' > "$HERE/$app"
    printf 'helper fixture\n' > "$HERE/wg-helper"
    WG_USE_PREBUILT=0
    export REAL_USER=fixture
    prepare_prebuilt
    prebuilt_ready
    cmp "$HERE/$app" "$HERE/target/release/$app"
    cmp "$HERE/wg-helper" "$HERE/target/release/wg-helper"
    test -n "$(find "$HERE/target/release/$app" -perm 755 -print)"
    test -n "$(find "$HERE/target/release/wg-helper" -perm 755 -print)"
    verify_build_deps
    build_app
    export PM PKGS # Read by the dynamically loaded installer functions.
    for PM in apt-get dnf yum pacman zypper apk xbps-install eopkg; do
        PKGS="build-essential gcc base-devel build-base pkg-config dev-headers git"
        install_pkgs
    done
    grep -q wireguard-tools "$calls"
    if grep -Eq 'build-essential|gcc|base-devel|build-base|pkg-config|dev-headers| git|rustup|cargo|-(dev|devel)( |$)' "$calls"; then
        echo "installer sanity failed: prebuilt install requested build dependencies" >&2
        cat "$calls" >&2
        exit 1
    fi

    # A source checkout must build even if adjacent binaries happen to exist.
    HERE="$fixture/source"
    mkdir -p "$HERE"
    touch "$HERE/Cargo.toml" "$HERE/$app" "$HERE/wg-helper"
    WG_USE_PREBUILT=0
    prepare_prebuilt
    test "$WG_USE_PREBUILT" = 0
    test ! -e "$HERE/target"

    # An incomplete bundle or explicit staged update fails before installation.
    HERE="$fixture/incomplete"
    mkdir -p "$HERE"
    touch "$HERE/$app"
    if (prepare_prebuilt) 2>/dev/null; then
        echo "installer sanity failed: incomplete bundle accepted" >&2
        exit 1
    fi
    WG_USE_PREBUILT=1
    if (prepare_prebuilt) 2>/dev/null; then
        echo "installer sanity failed: missing staged binaries accepted" >&2
        exit 1
    fi
)

# A release updater runs this installer from the extracted tarball. Validate
# the actual archive, including the packaging/ paths used after binary install,
# so a missing desktop entry or polkit rule cannot produce a partial update.
if [ "$#" -ge 2 ]; then
    archive="$2"
    release_root="${archive##*/}"
    release_root="${release_root%.tar.gz}"
    listing="$(mktemp)"
    trap 'rm -f "$listing"' EXIT
    tar -tzf "$archive" > "$listing"
    for file in wireguard-gui wg-helper install.sh README.md LICENSE \
        packaging/wireguard-gui.desktop packaging/wireguard-gui.svg \
        packaging/49-wireguard-gui.rules; do
        if ! grep -Fxq "$release_root/$file" "$listing"; then
            echo "installer sanity failed: release archive missing $file" >&2
            exit 1
        fi
    done
fi

echo "installer sanity tests passed"
