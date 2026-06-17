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

echo "installer sanity tests passed"
