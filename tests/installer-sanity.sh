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
