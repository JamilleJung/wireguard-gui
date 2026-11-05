#!/usr/bin/env bash
# Check (and optionally prepare) a development machine for this repository.
#
# Usage:
#   scripts/dev-setup.sh            check prerequisites, print fixes for gaps
#   scripts/dev-setup.sh --install  also add missing rustup components and
#                                   prefetch locked crates (no sudo, no system
#                                   packages: those are printed for you to run)
#
# Exit status: 0 ready, 1 something required is missing.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
install=0
case "${1:-}" in
    "") ;;
    --install) install=1 ;;
    -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "dev-setup: unknown argument: $1" >&2; exit 2 ;;
esac
cd "$root"

project="$(awk '/^\[/{p=($0=="[package]");next} p&&$1=="name"{gsub(/"/,"",$3);print $3;exit}' Cargo.toml)"
# Minimum supported Rust: Cargo.toml `rust-version` when set, otherwise the
# value CI's MSRV job enforces for the current lockfile.
msrv="$(awk '/^\[/{p=($0=="[package]");next} p&&$1=="rust-version"{gsub(/"/,"",$3);print $3;exit}' Cargo.toml)"
if [ -z "$msrv" ]; then
    case "$project" in
        wireguard-gui) msrv=1.92 ;;
        *) msrv=1.88 ;;
    esac
fi

missing=0
hints=()
ok() { printf 'ok    %s\n' "$*"; }
miss() { printf 'MISS  %s\n' "$1"; missing=$((missing + 1)); [ -z "${2:-}" ] || hints+=("$2"); }
note() { printf 'note  %s\n' "$*"; }

pkg_hint() {
    # pkg_hint <debian> <fedora> <arch>
    if command -v apt-get >/dev/null; then echo "sudo apt-get install -y $1"
    elif command -v dnf >/dev/null; then echo "sudo dnf install -y $2"
    elif command -v pacman >/dev/null; then echo "sudo pacman -S --needed $3"
    else echo "install: $1 (Debian names)"; fi
}

echo "Checking the $project development environment ($root)"
[ "$(uname -s)" = Linux ] && ok "Linux host" \
    || note "$(uname -s) host: unit/property tests run, but Linux is needed for runtime and namespace tests"

if command -v cargo >/dev/null && command -v rustc >/dev/null; then
    have="$(rustc --version | awk '{print $2}')"
    if [ "$(printf '%s\n%s\n' "$msrv" "${have%%-*}" | sort -V | head -n 1)" = "$msrv" ]; then
        ok "rustc $have (>= $msrv)"
    else
        miss "rustc $have is older than the minimum $msrv" "rustup update stable"
    fi
else
    miss "Rust toolchain (cargo, rustc)" "curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh   # or your distro's rustup"
fi

for component in rustfmt clippy; do
    if cargo "${component/rustfmt/fmt}" --version >/dev/null 2>&1; then
        ok "cargo ${component/rustfmt/fmt}"
    elif [ "$install" = 1 ] && command -v rustup >/dev/null && rustup component add "$component"; then
        ok "cargo ${component/rustfmt/fmt} (installed)"
    else
        miss "cargo ${component/rustfmt/fmt}" "rustup component add $component"
    fi
done

command -v cc >/dev/null && ok "C toolchain (cc)" \
    || miss "C compiler/linker" "$(pkg_hint build-essential 'gcc make' base-devel)"
command -v shellcheck >/dev/null && ok "shellcheck" \
    || miss "shellcheck (shell lint gate)" "$(pkg_hint shellcheck ShellCheck shellcheck)"
command -v python3 >/dev/null && ok "python3 (fixture script, release scripts)" \
    || miss "python3" "$(pkg_hint python3 python3 python)"
command -v git >/dev/null && ok "git" || miss "git" "$(pkg_hint git git git)"

if [ "$project" = wireguard-gui ]; then
    if command -v pkg-config >/dev/null; then
        for lib in fontconfig xkbcommon; do
            pkg-config --exists "$lib" && ok "pkg-config: $lib" \
                || miss "development files for $lib" \
                    "$(pkg_hint 'libfontconfig-dev libxkbcommon-dev libgl1-mesa-dev libegl1-mesa-dev libdbus-1-dev' \
                        'fontconfig-devel libxkbcommon-devel mesa-libGL-devel mesa-libEGL-devel dbus-devel' \
                        'fontconfig libxkbcommon mesa dbus')"
        done
    else
        miss "pkg-config" "$(pkg_hint pkg-config pkgconf-pkg-config pkgconf)"
    fi
fi

for tool in wg wg-quick nft; do
    command -v "$tool" >/dev/null && ok "runtime tool $tool" \
        || note "runtime tool $tool not found (only needed to run the app or the namespace tests)"
done
command -v cargo-fuzz >/dev/null && ok "cargo-fuzz (optional)" \
    || note "cargo-fuzz not installed (optional: cargo install cargo-fuzz --locked; needs nightly)"
command -v cargo-mutants >/dev/null && ok "cargo-mutants (optional)" \
    || note "cargo-mutants not installed (optional: cargo install cargo-mutants --locked)"

if [ "$install" = 1 ] && command -v cargo >/dev/null; then
    cargo fetch --locked && ok "locked crates prefetched"
fi

echo
if [ "$missing" -ne 0 ]; then
    echo "dev-setup: $missing required item(s) missing. Suggested fixes:"
    printf "  %s\n" ${hints[@]+"${hints[@]}"} | awk '!seen[$0]++'
    exit 1
fi
echo "dev-setup: ready. Run the checks listed under 'Required checks' in CONTRIBUTING.md,"
echo "or all of them at once with: bash tests/dev-onboarding.sh --from-worktree"
