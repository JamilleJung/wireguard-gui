#!/usr/bin/env bash
#
# Universal installer for wireguard-gui.
#
#   ./install.sh              build + install (auto-installs missing deps)
#   ./install.sh uninstall    remove everything
#
# Works on Debian/Ubuntu, Fedora/RHEL, Arch/Manjaro, openSUSE, Alpine, Void
# and Solus. Run as a normal user — it calls sudo only where it must.
set -euo pipefail

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
PREFIX="${PREFIX:-/usr/local}"
LIBDIR="$PREFIX/lib/wireguard-gui"
BIN="$PREFIX/bin/wireguard-gui"
HELPER="$LIBDIR/wg-helper"
DESKTOP="$PREFIX/share/applications/wireguard-gui.desktop"
ICON_DIR="$PREFIX/share/icons/hicolor/scalable/apps"
SUDOERS="/etc/sudoers.d/wireguard-gui"
POLKIT_RULE="/etc/polkit-1/rules.d/49-wireguard-gui.rules"
HERE="$(cd "$(dirname "$0")" && pwd)"

# ---------------------------------------------------------------------------
# Args:  [uninstall] [--polkit | --sudoers]
# ---------------------------------------------------------------------------
ACTION="install"; AUTH_MODE="sudoers"
for arg in "$@"; do
    case "$arg" in
        uninstall)  ACTION="uninstall" ;;
        --polkit)   AUTH_MODE="polkit" ;;
        --sudoers)  AUTH_MODE="sudoers" ;;
        *) ;;
    esac
done

# ---------------------------------------------------------------------------
# Pretty output
# ---------------------------------------------------------------------------
if [ -t 1 ]; then
    B="\033[1m"; G="\033[1;32m"; Y="\033[1;33m"; R="\033[1;31m"; C="\033[1;36m"; N="\033[0m"
else
    B=""; G=""; Y=""; R=""; C=""; N=""
fi
say()  { printf "${C}::${N} ${B}%s${N}\n" "$*"; }
ok()   { printf "${G}✓${N} %s\n" "$*"; }
warn() { printf "${Y}!${N} %s\n" "$*"; }
die()  { printf "${R}✗ %s${N}\n" "$*" >&2; exit 1; }

# ---------------------------------------------------------------------------
# Privilege helper
# ---------------------------------------------------------------------------
if [ "$(id -u)" -eq 0 ]; then
    SUDO=""
    REAL_USER="${SUDO_USER:-root}"
else
    command -v sudo >/dev/null 2>&1 || die "Need root or sudo to install. Re-run as root."
    SUDO="sudo"
    REAL_USER="$USER"
fi

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
if [ "$ACTION" = "uninstall" ]; then
    say "Removing wireguard-gui"
    $SUDO rm -f "$BIN" "$HELPER" "$DESKTOP" "$SUDOERS" "$POLKIT_RULE" "$ICON_DIR/wireguard-gui.svg"
    $SUDO rm -rf "$LIBDIR"
    command -v update-desktop-database >/dev/null 2>&1 && \
        $SUDO update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
    ok "Uninstalled."
    exit 0
fi

# ---------------------------------------------------------------------------
# Detect the package manager + the package names for this distro
# ---------------------------------------------------------------------------
PM=""
for c in apt-get dnf yum pacman zypper apk xbps-install eopkg; do
    if command -v "$c" >/dev/null 2>&1; then PM="$c"; break; fi
done

# Each list covers: C toolchain + pkg-config, the libs Slint needs
# (fontconfig + xkbcommon), an OpenGL runtime, plus curl/git and wireguard-tools.
case "$PM" in
    apt-get)
        PKGS="build-essential pkg-config libfontconfig-dev libxkbcommon-dev libgl1 libegl1 libdbus-1-dev curl git wireguard-tools" ;;
    dnf|yum)
        PKGS="gcc gcc-c++ make pkgconf-pkg-config fontconfig-devel libxkbcommon-devel mesa-libGL mesa-libEGL dbus-devel curl git wireguard-tools" ;;
    pacman)
        PKGS="base-devel fontconfig libxkbcommon libglvnd dbus curl git wireguard-tools" ;;
    zypper)
        PKGS="gcc gcc-c++ make pkg-config fontconfig-devel libxkbcommon-devel Mesa-libGL1 Mesa-libEGL1 dbus-1-devel curl git wireguard-tools" ;;
    apk)
        PKGS="build-base pkgconf fontconfig-dev libxkbcommon-dev mesa-gl mesa-egl dbus-dev curl git wireguard-tools" ;;
    xbps-install)
        PKGS="base-devel fontconfig-devel libxkbcommon-devel MesaLib dbus-devel curl git wireguard-tools" ;;
    eopkg)
        PKGS="system.devel fontconfig-devel libxkbcommon-devel mesalib-devel dbus-devel curl git wireguard-tools" ;;
    *)
        PM=""
        warn "Could not detect a supported package manager."
        warn "Please install manually: a C compiler, pkg-config, fontconfig + libxkbcommon dev headers, an OpenGL runtime, curl, git, and wireguard-tools." ;;
esac

install_pkgs() {
    [ -z "$PM" ] && { warn "Skipping automatic dependency install."; return 0; }
    say "Installing dependencies via ${PM} (this may ask for your password)"
    case "$PM" in
        apt-get)      $SUDO apt-get update -qq || true
                      $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y $PKGS ;;
        dnf|yum)      $SUDO "$PM" install -y $PKGS ;;
        pacman)       $SUDO pacman -Sy --needed --noconfirm $PKGS ;;
        zypper)       $SUDO zypper --non-interactive install $PKGS ;;
        apk)          $SUDO apk add $PKGS ;;
        xbps-install) $SUDO xbps-install -Sy $PKGS ;;
        eopkg)        $SUDO eopkg -y install $PKGS ;;
    esac
    ok "Dependencies installed."
}

# ---------------------------------------------------------------------------
# Ensure a Rust toolchain (cargo). Prefer an existing one; else rustup.
# ---------------------------------------------------------------------------
ensure_rust() {
    [ -x "$HOME/.cargo/bin/cargo" ] && export PATH="$HOME/.cargo/bin:$PATH"
    if command -v cargo >/dev/null 2>&1; then
        ok "Found cargo: $(cargo --version)"
        return 0
    fi
    say "Rust toolchain not found — installing via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
    export PATH="$HOME/.cargo/bin:$PATH"
    command -v cargo >/dev/null 2>&1 || die "Rust install failed; install cargo manually and re-run."
    ok "Installed $(cargo --version)"
}

# ---------------------------------------------------------------------------
# Verify the toolchain is actually usable (catches minimal installs where a
# package "installed" but the command/headers still aren't where we need them).
# ---------------------------------------------------------------------------
verify_build_deps() {
    say "Checking the build toolchain"
    local missing=0

    if ! command -v cc >/dev/null 2>&1 && ! command -v gcc >/dev/null 2>&1; then
        warn "No C compiler (cc/gcc) found."; missing=1
    fi
    command -v pkg-config >/dev/null 2>&1 || command -v pkgconf >/dev/null 2>&1 || {
        warn "pkg-config not found."; missing=1; }

    # dev headers Slint + ksni link against
    if command -v pkg-config >/dev/null 2>&1; then
        for lib in fontconfig xkbcommon dbus-1; do
            pkg-config --exists "$lib" 2>/dev/null || { warn "Dev headers for '$lib' not found (pkg-config)."; missing=1; }
        done
    fi

    if [ "$missing" -ne 0 ]; then
        warn "Some build dependencies are still missing after the package step."
        warn "On a minimal install you may need to install them by hand — see the"
        warn "per-distro table in the README — then re-run ./install.sh."
        die "Cannot build until the toolchain is complete."
    fi
    ok "Build toolchain OK."
}

# Runtime tools the app needs at run time (not for the build).
verify_runtime_deps() {
    command -v wg >/dev/null 2>&1 && command -v wg-quick >/dev/null 2>&1 \
        || warn "wireguard-tools (wg/wg-quick) not found — the app needs them at runtime. Install the 'wireguard-tools' package."
}

# ---------------------------------------------------------------------------
# Go
# ---------------------------------------------------------------------------
printf "${B}wireguard-gui installer${N}\n"
[ -n "$PM" ] && say "Detected package manager: ${PM}" || true

install_pkgs
ensure_rust
verify_build_deps
verify_runtime_deps

say "Building release binary (first build downloads crates, ~1–3 min)"
( cd "$HERE" && cargo build --release )
[ -f "$HERE/target/release/wireguard-gui" ] || die "Build did not produce a binary."
ok "Built."

say "Installing into $PREFIX"
$SUDO install -d "$LIBDIR" "$ICON_DIR" "$PREFIX/bin" "$PREFIX/share/applications"
$SUDO install -m755 "$HERE/target/release/wireguard-gui" "$BIN"
$SUDO install -m755 "$HERE/packaging/wg-helper" "$HELPER"
$SUDO install -m644 "$HERE/packaging/wireguard-gui.desktop" "$DESKTOP"
[ -f "$HERE/packaging/wireguard-gui.svg" ] && \
    $SUDO install -m644 "$HERE/packaging/wireguard-gui.svg" "$ICON_DIR/wireguard-gui.svg" || true
command -v update-desktop-database >/dev/null 2>&1 && \
    $SUDO update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
ok "Files installed."

if [ "$AUTH_MODE" = "polkit" ]; then
    say "Installing polkit rule (passwordless for active local sessions)"
    $SUDO rm -f "$SUDOERS"
    $SUDO install -d /etc/polkit-1/rules.d
    $SUDO install -m644 "$HERE/packaging/49-wireguard-gui.rules" "$POLKIT_RULE"
    ok "polkit rule installed at $POLKIT_RULE"
else
    say "Granting passwordless access to the wg-helper for $REAL_USER (sudoers)"
    $SUDO rm -f "$POLKIT_RULE"
    printf '%s ALL=(root) NOPASSWD: %s\n' "$REAL_USER" "$HELPER" | $SUDO tee "$SUDOERS" >/dev/null
    $SUDO chmod 440 "$SUDOERS"
    if $SUDO visudo -cf "$SUDOERS" >/dev/null 2>&1; then
        ok "sudoers drop-in valid."
    else
        $SUDO rm -f "$SUDOERS"
        warn "sudoers validation failed; removed it. The app will fall back to pkexec (will prompt)."
    fi
fi

printf "\n${G}${B}Done!${N} Launch ${B}WireGuard${N} from your app menu, or run: ${B}wireguard-gui${N}\n"
printf "Uninstall any time with: ${B}./install.sh uninstall${N}\n"
