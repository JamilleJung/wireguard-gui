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
HAVE_SUDO=0
command -v sudo >/dev/null 2>&1 && HAVE_SUDO=1
if [ "$(id -u)" -eq 0 ]; then
    as_root() { "$@"; }                                # already root
    REAL_USER="${SUDO_USER:-root}"
elif [ "$HAVE_SUDO" -eq 1 ]; then
    as_root() { sudo "$@"; }
    REAL_USER="$USER"
elif command -v su >/dev/null 2>&1; then
    # Debian-minimal usually has no sudo - fall back to `su` (asks for the ROOT
    # password). Tip: running this as root ('su -' first) avoids repeat prompts.
    warn "sudo not found - using 'su' (you'll be asked for the ROOT password)."
    as_root() { su root -c "$(printf '%q ' "$@")"; }
    REAL_USER="$USER"
else
    die "Need root. Re-run as root ('su -' then ./install.sh) or install sudo first."
fi

# A sudoers drop-in is useless without sudo; install a polkit rule instead.
if [ "$HAVE_SUDO" -eq 0 ] && [ "$AUTH_MODE" = "sudoers" ]; then
    warn "sudo isn't installed - using a polkit rule for passwordless privilege."
    AUTH_MODE="polkit"
fi

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
if [ "$ACTION" = "uninstall" ]; then
    say "Removing wireguard-gui"
    as_root rm -f "$BIN" "$HELPER" "$DESKTOP" "$SUDOERS" "$POLKIT_RULE" "$ICON_DIR/wireguard-gui.svg"
    as_root rm -rf "$LIBDIR"
    command -v update-desktop-database >/dev/null 2>&1 && \
        as_root update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
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
        apt-get)      as_root apt-get update -qq || true
                      as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y $PKGS ;;
        dnf|yum)      as_root "$PM" install -y $PKGS ;;
        pacman)       as_root pacman -Sy --needed --noconfirm $PKGS ;;
        zypper)       as_root zypper --non-interactive install $PKGS ;;
        apk)          as_root apk add $PKGS ;;
        xbps-install) as_root xbps-install -Sy $PKGS ;;
        eopkg)        as_root eopkg -y install $PKGS ;;
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
    # Resolve one pkg-config tool and use it for BOTH the presence test and the
    # header checks, so they can't diverge (some distros ship only `pkgconf`).
    PKGCONF="$(command -v pkg-config || command -v pkgconf || true)"
    if [ -z "$PKGCONF" ]; then
        warn "pkg-config not found."; missing=1
    else
        # dev headers Slint + ksni link against
        for lib in fontconfig xkbcommon dbus-1; do
            "$PKGCONF" --exists "$lib" 2>/dev/null || { warn "Dev headers for '$lib' not found ($PKGCONF)."; missing=1; }
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

# wg-quick needs a resolvconf provider to apply a config's `DNS =` line. Install
# one (best-effort) when it's missing and systemd-resolved isn't handling DNS -
# the fix for minimal Debian, where tunnels with DNS= otherwise fail to connect.
ensure_resolvconf() {
    command -v resolvconf >/dev/null 2>&1 && return 0
    [ -d /run/systemd/resolve ] && return 0   # systemd-resolved already handles DNS=
    say "Installing a resolvconf provider (so tunnels with 'DNS =' can connect)"
    case "$PM" in
        apt-get)      as_root apt-get install -y openresolv || true ;;
        dnf|yum)      as_root "$PM" install -y openresolv || true ;;
        pacman)       as_root pacman -Sy --noconfirm openresolv || true ;;
        zypper)       as_root zypper --non-interactive install openresolv || true ;;
        apk)          as_root apk add openresolv || true ;;
        xbps-install) as_root xbps-install -Sy openresolv || true ;;
        eopkg)        as_root eopkg install -y openresolv || true ;;
    esac
    command -v resolvconf >/dev/null 2>&1 && ok "resolvconf provider installed." \
        || warn "Could not install a resolvconf provider; tunnels with 'DNS =' may fail until you install 'openresolv'."
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
ensure_resolvconf

say "Building release binary (first build downloads crates, ~1–3 min)"
( cd "$HERE" && cargo build --release )
[ -f "$HERE/target/release/wireguard-gui" ] || die "Build did not produce a binary."
ok "Built."

say "Installing into $PREFIX"
as_root install -d "$LIBDIR" "$ICON_DIR" "$PREFIX/bin" "$PREFIX/share/applications"
as_root install -m755 "$HERE/target/release/wireguard-gui" "$BIN"
as_root install -m755 "$HERE/packaging/wg-helper" "$HELPER"
as_root install -m644 "$HERE/packaging/wireguard-gui.desktop" "$DESKTOP"
[ -f "$HERE/packaging/wireguard-gui.svg" ] && \
    as_root install -m644 "$HERE/packaging/wireguard-gui.svg" "$ICON_DIR/wireguard-gui.svg" || true
command -v update-desktop-database >/dev/null 2>&1 && \
    as_root update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
ok "Files installed."

if [ "$AUTH_MODE" = "polkit" ]; then
    say "Installing polkit rule (passwordless for active local sessions)"
    as_root rm -f "$SUDOERS"
    as_root install -d /etc/polkit-1/rules.d
    as_root install -m644 "$HERE/packaging/49-wireguard-gui.rules" "$POLKIT_RULE"
    ok "polkit rule installed at $POLKIT_RULE"
else
    say "Granting passwordless access to the wg-helper for $REAL_USER (sudoers)"
    as_root rm -f "$POLKIT_RULE"
    printf '%s ALL=(root) NOPASSWD: %s\n' "$REAL_USER" "$HELPER" | as_root tee "$SUDOERS" >/dev/null
    as_root chmod 440 "$SUDOERS"
    if as_root visudo -cf "$SUDOERS" >/dev/null 2>&1; then
        ok "sudoers drop-in valid."
    else
        as_root rm -f "$SUDOERS"
        warn "sudoers validation failed; removed it. The app will fall back to pkexec (will prompt)."
    fi
fi

printf "\n${G}${B}Done!${N} Launch ${B}WireGuard${N} from your app menu, or run: ${B}wireguard-gui${N}\n"
printf "Uninstall any time with: ${B}./install.sh uninstall${N}\n"
