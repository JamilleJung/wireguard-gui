#!/usr/bin/env bash
#
# Universal installer for wireguard-gui.
#
#   ./install.sh                  build + install (auto-installs missing deps)
#   ./install.sh --polkit         use a polkit rule instead of a sudoers drop-in
#   ./install.sh --sudoers        switch back to the sudoers drop-in
#   ./install.sh --binaries=DIR   install prebuilt wireguard-gui + wg-helper from DIR
#   ./install.sh --force          install even though a distro package copy exists
#   ./install.sh uninstall        remove everything this installer added
#
# Re-running the installer (or the in-app updater) keeps the authorization
# mode recorded at the first install unless a flag above changes it. A failed install restores the previous files,
# authorization and group setup.
#
# Works on Debian/Ubuntu, Fedora/RHEL, Arch/Manjaro, openSUSE, Alpine, Void
# and Solus. Run as a normal user — it calls sudo only where it must.
set -euo pipefail

# System tools we rely on (visudo, resolvconf, runuser, ...) live in sbin. A
# normal user's PATH - which `su` carries into the root re-exec below - often
# omits sbin, which made `visudo -cf` and the `resolvconf` probe silently fail.
# Make sure they're findable no matter how we ended up running.
export PATH="/usr/local/sbin:/usr/sbin:/sbin:$PATH"

# ---------------------------------------------------------------------------
# Paths
# ---------------------------------------------------------------------------
APP="wireguard-gui"
APP_BIN="wireguard-gui"
SIBLING_APP="wireguard-tui"
PREFIX="${PREFIX:-/usr/local}"
LIBDIR="$PREFIX/lib/$APP"
BIN="$PREFIX/bin/$APP_BIN"
ALIAS="$PREFIX/bin/wg-gui"
HELPER="$LIBDIR/wg-helper"
MANIFEST="$LIBDIR/install-manifest"
DESKTOP="$PREFIX/share/applications/$APP.desktop"
ICON_DIR="$PREFIX/share/icons/hicolor/scalable/apps"
ICON="$ICON_DIR/$APP.svg"
SUDOERS="/etc/sudoers.d/$APP"
POLKIT_DIR="/etc/polkit-1/rules.d"
POLKIT_RULE="$POLKIT_DIR/49-$APP.rules"
# Where distro packages put the app; used to refuse duplicate installs.
PKG_BIN="/usr/bin/$APP_BIN"
PKG_HELPER="/usr/lib/$APP/wg-helper"
HERE="$(cd "$(dirname "$0")" && pwd)"

# ---------------------------------------------------------------------------
# Args
# ---------------------------------------------------------------------------
# The desktop entry is how the GUI is launched, so it is always installed.
ACTION="install"; AUTH_MODE="sudoers"; INSTALL_DESKTOP=1; ALLOW_ROOT="${WG_ALLOW_ROOT:-0}"
AUTH_EXPLICIT=0; DESKTOP_EXPLICIT=1; FORCE=0; UPDATE_MODE=0; BINARIES_DIR=""
for arg in "$@"; do
    case "$arg" in
        uninstall)  ACTION="uninstall" ;;
        --polkit)   AUTH_MODE="polkit"; AUTH_EXPLICIT=1 ;;
        --sudoers)  AUTH_MODE="sudoers"; AUTH_EXPLICIT=1 ;;
        --allow-root) ALLOW_ROOT=1 ;;
        --force)    FORCE=1 ;;
        --update)   UPDATE_MODE=1 ;;
        --binaries=?*) BINARIES_DIR="${arg#--binaries=}" ;;
        *) printf 'Unknown installer option: %s\n' "$arg" >&2; exit 2 ;;
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

# Only --binaries selects a binaries directory; never an inherited variable.
unset WG_PREBUILT_DIR
if [ -n "$BINARIES_DIR" ]; then
    [ -d "$BINARIES_DIR" ] || die "--binaries: '$BINARIES_DIR' is not a directory."
    BINARIES_DIR="$(cd "$BINARIES_DIR" && pwd)"
    [ -f "$BINARIES_DIR/$APP_BIN" ] && [ -f "$BINARIES_DIR/wg-helper" ] \
        || die "--binaries: '$BINARIES_DIR' must contain both $APP_BIN and wg-helper."
    export WG_PREBUILT_DIR="$BINARIES_DIR" WG_USE_PREBUILT=1
fi

# ---------------------------------------------------------------------------
# Privilege helper
# ---------------------------------------------------------------------------
# Having the `sudo` binary is NOT enough: on many Debian servers the login user
# isn't in the sudoers file (sudo prompts, then says "is not in the sudoers
# file"). Treat sudo as usable only if a non-interactive check passes
# (passwordless / cached creds) or the user is in a typical admin group.
CAN_SUDO=0
if command -v sudo >/dev/null 2>&1; then
    if sudo -n true >/dev/null 2>&1; then
        CAN_SUDO=1
    else
        case " $(id -nG 2>/dev/null) " in
            *" sudo "*|*" wheel "*|*" admin "*) CAN_SUDO=1 ;;
        esac
    fi
fi

# The login user, taken from the REAL uid - never $USER, which the caller can set
# to anything (including a sudoers-injection payload). Validated before any use.
INVOKER="$(id -un 2>/dev/null || true)"

# If we can't use sudo, re-run the WHOLE installer as root ONCE (one ROOT-password
# prompt instead of one per privileged step). As root we'll install `sudo` if
# missing and write a passwordless drop-in for the invoking user - after which
# the app escalates with `sudo -n wg-helper` and never needs root again. Carry
# the invoking user and the install location across the re-exec so the drop-in
# targets *them* and the same prefix/binaries are used.
if [ "$(id -u)" -ne 0 ] && [ "$CAN_SUDO" -eq 0 ] && command -v su >/dev/null 2>&1; then
    warn "No usable sudo here - re-running as root (enter the ROOT password once)."
    exec su root -c "WG_REAL_USER=$(printf '%q' "$INVOKER") PREFIX=$(printf '%q' "$PREFIX") WG_USE_PREBUILT=$(printf '%q' "${WG_USE_PREBUILT:-0}") $(printf '%q ' "$HERE/install.sh" "$@")"
fi

if [ "$(id -u)" -eq 0 ]; then
    as_root() { "$@"; }                                  # already root
    # Recover the human who invoked us: explicit hand-off, sudo, then the login tty.
    REAL_USER="${WG_REAL_USER:-${SUDO_USER:-$(logname 2>/dev/null || true)}}"
    [ -n "$REAL_USER" ] || REAL_USER="root"
elif [ "$CAN_SUDO" -eq 1 ]; then
    as_root() { sudo "$@"; }
    REAL_USER="${INVOKER:-$USER}"
else
    die "Need root. Re-run as root ('su -' then ./install.sh), or install 'su'/'sudo'."
fi

# Installing straight from a root login (no normal user behind us) is discouraged:
# the Rust toolchain + build would run as root in /root, and the per-user
# passwordless helper grant has no one to target. Recommend a normal user, but
# let an operator override deliberately with --allow-root (or WG_ALLOW_ROOT=1).
if [ "$ACTION" = "install" ] && [ "$(id -u)" -eq 0 ] && [ "$REAL_USER" = "root" ]; then
    if [ "$ALLOW_ROOT" != "1" ]; then
        printf '\n'
        warn "Running as root with no normal user detected."
        printf "  ${C}Recommended${N}: run the installer as your normal user —\n"
        printf "      su - <youruser>\n      ./install.sh\n\n"
        printf "  Rust builds with a per-user toolchain and the passwordless helper\n"
        printf "  grant is per-user, so a root install gets neither.\n\n"
        printf "  To install system-wide as root anyway (you accept the risk: the\n"
        printf "  build runs as root or an existing prebuilt binary is reused, and\n"
        printf "  the app runs with direct privilege so it needs no helper grant):\n"
        printf "      ${B}./install.sh --allow-root${N}\n\n"
        die "Aborted — re-run as a normal user, or pass --allow-root."
    fi
    warn "Installing as root (--allow-root): the build runs as root or reuses a"
    warn "prebuilt binary, and the per-user helper grant is skipped (root needs none)."
fi

# Run a command as the invoking (non-root) user - used for the BUILD so cargo and
# rustup use that user's home and the source tree is never compiled as root (its
# build scripts / proc-macros would otherwise run with root privilege).
as_user() {
    if [ "$(id -u)" -eq 0 ] && [ -n "$REAL_USER" ] && [ "$REAL_USER" != "root" ]; then
        if command -v runuser >/dev/null 2>&1; then
            runuser -u "$REAL_USER" -- "$@"
        else
            su "$REAL_USER" -c "$(printf '%q ' "$@")"
        fi
    else
        "$@"
    fi
}

# The helper's narrow grant is safe only while no unprivileged user can replace
# its executable through a writable parent. Refuse unsafe layouts; never chown
# an existing caller-owned directory to turn it into a privileged install root.
install_path_metadata() {
    as_root env LC_ALL=C stat -c '%u %a %f %h' -- "$1"
}

validate_install_path() {
    local path="$1" kind="$2" component current="" metadata uid mode bits links
    local LC_ALL=C
    local -a components paths=(/)
    [[ "$path" =~ ^/[A-Za-z0-9_.-]+(/[A-Za-z0-9_.-]+)*$ ]] \
        || die "Unsafe install path '$path': use an absolute ASCII path without whitespace, trailing slashes or authorization metacharacters; existing authorization kept."
    IFS=/ read -r -a components <<< "${path#/}"
    for component in "${components[@]}"; do
        [[ "$component" != . && "$component" != .. ]] \
            || die "Unsafe install path '$path': dot components are forbidden; existing authorization kept."
        current="$current/$component"
        paths+=("$current")
    done
    for current in "${paths[@]}"; do
        if metadata="$(install_path_metadata "$current" 2>&1)"; then
            [[ "$metadata" =~ ^([0-9]+)\ ([0-7]{1,4})\ ([0-9a-fA-F]+)\ ([0-9]+)$ ]] \
                || die "Cannot inspect install path '$current'; existing authorization kept."
            read -r uid mode bits links <<< "$metadata"
            if (( (16#$bits & 0170000) == 0120000 )); then
                die "Symlink install path '$current' is forbidden: use its resolved, protected path; existing authorization kept."
            fi
            if [[ "$uid" != 0 ]] || (( (8#$mode & 0022) != 0 )); then
                die "Untrusted install path '$current': root ownership and no group/other write permission are required; existing authorization kept."
            fi
            if [[ "$current" != "$path" || "$kind" = directory ]]; then
                (( (16#$bits & 0170000) == 0040000 )) \
                    || die "Install ancestor '$current' must be a directory; existing authorization kept."
            else
                (( (16#$bits & 0170000) == 0100000 )) && [[ "$links" = 1 ]] \
                    || die "Install target '$current' must be a regular file with one link; existing authorization kept."
                if [[ "$kind" = helper ]]; then
                    (( (8#$mode & 0111) != 0 )) \
                        || die "Installed helper '$current' is not executable; existing authorization kept."
                fi
            fi
        else
            case "$metadata" in
                *"No such file or directory"*|*"can't stat"*)
                    [[ "$kind" != helper ]] \
                        || die "Installed helper '$path' is missing; existing authorization kept."
                    # A missing path below already-checked protected ancestors
                    # can be created by root without a user-controlled traversal.
                    return 0 ;;
                *) die "Cannot inspect install path '$current': $metadata; existing authorization kept." ;;
            esac
        fi
    done
}

validate_authorization_path() {
    validate_install_path "$HELPER" helper
    if [[ "$AUTH_MODE" = polkit && "$PREFIX" != /usr && "$PREFIX" != /usr/local ]]; then
        die "--polkit supports only PREFIX=/usr or /usr/local; custom helper paths need matching authorization rules and frontend discovery. Existing authorization kept."
    fi
}

validate_install_destinations() {
    validate_install_path "$PREFIX" directory
    validate_install_path "$BIN" file
    validate_install_path "$HELPER" file
    if [ "$INSTALL_DESKTOP" -eq 1 ]; then
        validate_install_path "$DESKTOP" file
        validate_install_path "$ICON" file
    fi
    if [[ "$AUTH_MODE" = polkit && "$PREFIX" != /usr && "$PREFIX" != /usr/local ]]; then
        die "--polkit supports only PREFIX=/usr or /usr/local; existing authorization kept."
    fi
}

# A NOPASSWD sudoers drop-in scoped to the helper grants `sudo -n wg-helper` to
# REAL_USER even if they aren't otherwise a sudoer (sudoers rules are per-user,
# independent of the `sudo` group) - but it needs the `sudo` binary present. So
# "no sudo -> install sudo", then set it up. Skipped for an explicit --polkit.
ensure_sudo() {
    [ "$AUTH_MODE" != "none" ] || return 0
    if [ "$AUTH_MODE" = "polkit" ]; then
        command -v pkexec >/dev/null 2>&1 \
            || die "--polkit requires pkexec/polkit. Install it first or use --sudoers; existing authorization kept."
        return 0
    fi
    command -v sudo >/dev/null 2>&1 && return 0
    say "Installing sudo (so the app can use a passwordless helper drop-in)"
    pkg_install sudo || true
    if command -v sudo >/dev/null 2>&1; then
        ok "sudo installed."
    elif command -v pkexec >/dev/null 2>&1; then
        warn "Could not install sudo - using a polkit rule (pkexec) instead."
        AUTH_MODE="polkit"
    else
        die "Could not install sudo, and pkexec/polkit isn't available either. \
Install 'sudo' manually and re-run, or run wireguard-gui as root."
    fi
}

# ---------------------------------------------------------------------------
# Package manager
# ---------------------------------------------------------------------------
# Every invocation installs only what is missing and never refreshes a rolling
# distro's package database on its own: refreshing pacman's databases without a
# full upgrade can leave Arch half-upgraded, so pacman is only ever asked for
# `-S --needed` against the databases the administrator last synced.
PM=""
for c in apt-get dnf yum pacman zypper apk xbps-install eopkg; do
    command -v "$c" >/dev/null 2>&1 && { PM="$c"; break; }
done

# Print the subset of the named packages that are not installed yet.
pkg_missing() {
    local p
    for p in "$@"; do
        case "$PM" in
            apt-get) dpkg-query -W -f='${db:Status-Abbrev}' "$p" 2>/dev/null | grep -q '^ii' \
                         || printf '%s\n' "$p" ;;
            dnf|yum|zypper) rpm -q --whatprovides "$p" >/dev/null 2>&1 || printf '%s\n' "$p" ;;
            pacman) pacman -T "$p" >/dev/null 2>&1 || printf '%s\n' "$p" ;;
            apk) apk info -e "$p" >/dev/null 2>&1 || printf '%s\n' "$p" ;;
            xbps-install) xbps-query "$p" >/dev/null 2>&1 || printf '%s\n' "$p" ;;
            *) printf '%s\n' "$p" ;;
        esac
    done
}

APT_UPDATED=0
# Install the named packages that are missing. Returns non-zero on failure so
# callers decide whether a package is required or best-effort.
pkg_install() {
    local p
    local -a wanted=()
    while IFS= read -r p; do
        [ -z "$p" ] || wanted+=("$p")
    done < <(pkg_missing "$@")
    [ "${#wanted[@]}" -gt 0 ] || return 0
    case "$PM" in
        apt-get)
            if [ "$APT_UPDATED" -eq 0 ]; then
                as_root apt-get update -qq || true
                APT_UPDATED=1
            fi
            as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "${wanted[@]}" ;;
        dnf|yum) as_root "$PM" install -y "${wanted[@]}" ;;
        pacman)
            if ! as_root pacman -S --needed --noconfirm "${wanted[@]}"; then
                warn "pacman could not install: ${wanted[*]}"
                warn "If the package databases are missing or stale, upgrade the whole system"
                warn "first with 'sudo pacman -Syu', then re-run the installer. (Refreshing the"
                warn "databases without upgrading risks a partial upgrade, so it is never done here.)"
                return 1
            fi ;;
        zypper) as_root zypper --non-interactive install "${wanted[@]}" ;;
        apk) as_root apk add "${wanted[@]}" ;;
        xbps-install)
            if ! as_root xbps-install -Sy "${wanted[@]}"; then
                warn "xbps-install could not install: ${wanted[*]}"
                warn "Update the system first with 'sudo xbps-install -Su', then re-run."
                return 1
            fi ;;
        eopkg) as_root eopkg install -y "${wanted[@]}" ;;
        *) return 1 ;;
    esac
}

# Per-distro package names. The release binary links fontconfig and loads
# xkbcommon, Wayland, EGL/GL, X11 and libdbus at run time (dlopen), so those
# runtime libraries are listed explicitly; a source build also needs the C
# toolchain, pkg-config and development headers.
install_pkgs() {
    local runtime="" build="" iproute="iproute2" certs="ca-certificates"
    case "$PM" in
        apt-get)
            runtime="libfontconfig1 libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-egl1 libegl1 libgl1 libx11-6 libx11-xcb1 libxcursor1 libxi6 libxrender1 libxcb1 libdbus-1-3"
            build="build-essential pkg-config libfontconfig-dev libxkbcommon-dev libxkbcommon-x11-dev libegl1-mesa-dev libwayland-dev libdbus-1-dev git" ;;
        dnf|yum)
            iproute="iproute"
            runtime="fontconfig libxkbcommon libxkbcommon-x11 libwayland-client libwayland-egl mesa-libEGL mesa-libGL libX11 libX11-xcb libXcursor libXi libXrender libxcb dbus-libs"
            build="gcc gcc-c++ make pkgconf-pkg-config fontconfig-devel libxkbcommon-devel libxkbcommon-x11-devel mesa-libEGL-devel wayland-devel dbus-devel git" ;;
        pacman)
            certs=""
            runtime="fontconfig libxkbcommon libxkbcommon-x11 wayland libglvnd mesa libx11 libxcursor libxi libxrender libxcb dbus"
            build="base-devel git" ;;
        zypper)
            runtime="fontconfig libxkbcommon0 libxkbcommon-x11-0 libwayland-client0 libwayland-egl1 libEGL1 libGL1 Mesa-dri libX11-6 libX11-xcb1 libXcursor1 libXi6 libXrender1 libxcb1 libdbus-1-3"
            build="gcc gcc-c++ make pkg-config fontconfig-devel libxkbcommon-devel libxkbcommon-x11-devel Mesa-libEGL-devel wayland-devel dbus-1-devel git" ;;
        apk)
            runtime="fontconfig libxkbcommon libxkbcommon-x11 wayland-libs-client wayland-libs-egl mesa-egl mesa-gl libx11 libxcursor libxi libxrender libxcb dbus-libs"
            build="build-base pkgconf fontconfig-dev libxkbcommon-dev mesa-dev wayland-dev dbus-dev git" ;;
        xbps-install)
            runtime="fontconfig libxkbcommon libxkbcommon-x11 wayland libglvnd mesa-dri libX11 libXcursor libXi libXrender libxcb dbus-libs"
            build="base-devel pkg-config fontconfig-devel libxkbcommon-devel MesaLib-devel wayland-devel dbus-devel git" ;;
        eopkg)
            certs=""
            runtime="fontconfig libxkbcommon wayland mesalib libx11 libxcursor libxi libxrender libxcb dbus"
            build="system.devel fontconfig-devel libxkbcommon-devel mesalib-devel wayland-devel dbus-devel git" ;;
        *)
            warn "Could not detect a supported package manager."
            warn "Please install manually: wireguard-tools, iproute2, curl, fontconfig, libxkbcommon(-x11), Wayland, EGL/GL, X11 and libdbus runtime libraries; to build also a C compiler, pkg-config and their development headers."
            return 0 ;;
    esac
    # Portable releases need runtime libraries, never compilers or headers.
    prebuilt_ready && build=""
    say "Installing dependencies via ${PM} (this may ask for your password)"
    # shellcheck disable=SC2086 # Package lists are intentionally word-split.
    pkg_install wireguard-tools $iproute curl $certs $runtime $build \
        || die "Could not install the required packages; nothing was changed."
    ok "Dependencies installed."
}

# Optional runtime tools: the kill switch needs nftables (or iptables), and the
# in-app updater verifies releases with minisign. Never fatal.
ensure_optional_tools() {
    [ -n "$PM" ] || return 0
    if ! command -v nft >/dev/null 2>&1 && ! command -v iptables-restore >/dev/null 2>&1; then
        pkg_install nftables >/dev/null 2>&1 \
            || warn "nftables/iptables not found: the kill switch needs one of them."
    fi
    if ! command -v minisign >/dev/null 2>&1; then
        pkg_install minisign >/dev/null 2>&1 \
            || warn "minisign not available: in-app updates cannot be verified until it is installed."
    fi
}

# Reuse an extracted release bundle without treating source checkouts as
# prebuilt installs. The updater already stages its verified binaries and opts
# in explicitly; a manual bundle has adjacent binaries and no Cargo manifest;
# --binaries=DIR points at locally built binaries.
prebuilt_ready() {
    [ "${WG_USE_PREBUILT:-0}" = 1 ] \
        && [ -f "${WG_PREBUILT_DIR:-$HERE/target/release}/wireguard-gui" ] \
        && [ -f "${WG_PREBUILT_DIR:-$HERE/target/release}/wg-helper" ]
}

prepare_prebuilt() {
    if [ "${WG_USE_PREBUILT:-0}" = 1 ]; then
        prebuilt_ready || die "Prebuilt install requires both application and helper binaries."
        return 0
    fi
    [ ! -f "$HERE/Cargo.toml" ] || return 0
    [ -f "$HERE/wireguard-gui" ] && [ -f "$HERE/wg-helper" ] \
        || die "Incomplete release bundle: application and helper binaries are required."
    install -d "$HERE/target/release"
    install -m755 "$HERE/wireguard-gui" "$HERE/target/release/wireguard-gui"
    install -m755 "$HERE/wg-helper" "$HERE/target/release/wg-helper"
    WG_USE_PREBUILT=1
}

# Release tarball CPU token (wireguard-gui-<version>-<token>-linux.tar.gz) for
# a `uname -m` machine name. Keep in sync with release_arch() in src/update.rs.
release_arch() {
    case "$1" in
        x86_64|amd64) echo x86_64 ;;
        i386|i486|i586|i686) echo i686 ;;
        aarch64|arm64) echo aarch64 ;;
        armv7l|armv8l) echo armv7 ;; # armv8l: 32-bit userland on an ARMv8 CPU
        *) return 1 ;;
    esac
}

# Token for this system. A 32-bit userland on a 64-bit kernel (uname -m still
# says x86_64/aarch64) needs the 32-bit tarball.
host_release_arch() {
    local arch
    arch="$(release_arch "$(uname -m)")" || return 1
    if [ "$(getconf LONG_BIT 2>/dev/null)" = 32 ]; then
        case "$arch" in x86_64) arch=i686 ;; aarch64) arch=armv7 ;; esac
    fi
    echo "$arch"
}

# Token of an ELF binary from its header: EI_CLASS (byte 4) and the
# little-endian e_machine (bytes 18-19). UPX-packed binaries keep both.
elf_release_arch() {
    local hdr
    hdr="$(od -An -v -t x1 -N 20 "$1" 2>/dev/null | tr -d ' \n')" || return 1
    case "${hdr:0:10}:${hdr:36:4}" in
        7f454c4602:3e00) echo x86_64 ;;
        7f454c4601:0300) echo i686 ;;
        7f454c4602:b700) echo aarch64 ;;
        7f454c4601:2800) echo armv7 ;;
        7f454c46*) echo "unsupported" ;;
        *) return 1 ;; # not ELF (e.g. a script); the helper run check covers it
    esac
}

# Refuse a prebuilt bundle for another CPU before anything is installed, and
# name the tarball that matches this system.
check_prebuilt_arch() {
    prebuilt_ready || return 0
    command -v od >/dev/null 2>&1 || return 0
    local dir="${WG_PREBUILT_DIR:-$HERE/target/release}" host bin bundle warned=""
    host="$(host_release_arch)" || host=""
    for bin in wireguard-gui wg-helper; do
        bundle="$(elf_release_arch "$dir/$bin")" || continue
        [ "$bundle" != "$host" ] || continue
        [ "$bundle" != unsupported ] \
            || die "$bin in this bundle is not a Linux x86_64, i686, aarch64 or armv7 binary."
        case "$host:$bundle" in
            x86_64:i686|aarch64:armv7)
                [ -n "$warned" ] || warn "This bundle is a 32-bit $bundle build on a 64-bit $host system: it needs the 32-bit ($bundle) system libraries. The $host tarball is usually the right one."
                warned=1
                continue ;;
        esac
        [ -n "$host" ] || die "No release build exists for this CPU ($(uname -m)); build from source instead."
        die "$bin in this bundle is built for $bundle, but this system needs the $host build: use wireguard-gui-<version>-$host-linux.tar.gz."
    done
}

# Ensure a Rust toolchain and build the release binary AS THE INVOKING USER (via
# as_user - never as root, so build scripts/proc-macros don't run with privilege
# and the toolchain + artifacts land in that user's home, not /root).
build_app() {
    # Self-update path: when WG_USE_PREBUILT=1 and both verified binaries are
    # already staged in target/release, skip the build entirely.
    if prebuilt_ready; then
        ok "Using staged prebuilt binaries (skipping build)."
        return 0
    fi
    # A root install (--allow-root) must not drop a Rust toolchain into /root or
    # run build scripts as root: reuse an existing prebuilt binary when present.
    if [ "$(id -u)" -eq 0 ] && [ "$REAL_USER" = "root" ] \
            && [ -f "$HERE/target/release/wireguard-gui" ] \
            && [ -f "$HERE/target/release/wg-helper" ]; then
        ok "Using the existing prebuilt binary (skipping the build as root)."
        return 0
    fi
    say "Building release binary (first build downloads crates, ~1-2 min)"
    # Look up the build user's home safely (no eval on the name) so cargo/rustup
    # land there regardless of how runuser/su set the environment.
    local rh
    rh="$(getent passwd "$REAL_USER" 2>/dev/null | cut -d: -f6)"
    [ -n "$rh" ] || rh="$(awk -F: -v u="$REAL_USER" '$1==u {print $6; exit}' /etc/passwd 2>/dev/null || true)"
    [ -n "$rh" ] || rh="$HOME"
    as_user env HOME="$rh" sh -s "$HERE" <<'BUILD'
set -e
HERE="$1"
export PATH="$HOME/.cargo/bin:$PATH"
if ! command -v cargo >/dev/null 2>&1; then
    echo ":: Rust toolchain not found - installing via rustup"
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --no-modify-path
    # shellcheck disable=SC1091
    . "$HOME/.cargo/env"
fi
cd "$HERE"
cargo build --release --locked
BUILD
    [ -f "$HERE/target/release/wireguard-gui" ] || die "Build did not produce a binary."
    ok "Built."
}

# ---------------------------------------------------------------------------
# Verify the toolchain is actually usable (catches minimal installs where a
# package "installed" but the command/headers still aren't where we need them).
# ---------------------------------------------------------------------------
verify_build_deps() {
    # Explicit prebuilt reuse (WG_USE_PREBUILT=1): nothing is built, so don't
    # fail on a missing compiler/headers.
    if prebuilt_ready; then
        return 0
    fi
    # When a root install will reuse the prebuilt binary, there is nothing to
    # build, so don't fail on a missing compiler/headers.
    if [ "$(id -u)" -eq 0 ] && [ "$REAL_USER" = "root" ] \
            && [ -f "$HERE/target/release/wireguard-gui" ] \
            && [ -f "$HERE/target/release/wg-helper" ]; then
        return 0
    fi
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
        # dev headers Slint + ksni link against.  Check every system library
        # the binary links: fontconfig, xkbcommon + x11, GL/EGL (femtovg
        # renderer), wayland-client (arboard clipboard + winit backend), and
        # dbus-1 (ksni tray).
        for lib in fontconfig xkbcommon xkbcommon-x11 egl wayland-client dbus-1; do
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

verify_runtime_deps() {
    command -v wg >/dev/null 2>&1 && command -v wg-quick >/dev/null 2>&1 \
        || warn "wireguard-tools (wg/wg-quick) not found — the app needs them at runtime."
    command -v ip >/dev/null 2>&1 \
        || warn "iproute2 ('ip') not found — wg-quick needs it to bring tunnels up."
}

resolvconf_uses_resolvectl() {
    local resolver_path resolver_target
    command -v resolvconf >/dev/null 2>&1 || return 1
    resolver_path="$(command -v resolvconf)"
    resolver_target="$(readlink -f "$(command -v resolvconf)" 2>/dev/null || printf '%s' "$resolver_path")"
    [ "$(basename "$resolver_target")" = "resolvectl" ]
}

systemd_resolved_active() {
    if command -v systemctl >/dev/null 2>&1; then
        systemctl is-active --quiet systemd-resolved.service
    else
        [ -d /run/systemd/resolve ]
    fi
}

warn_enable_resolved() {
    warn "resolvconf is the resolvectl shim, but systemd-resolved is inactive."
    warn "Run: sudo systemctl enable --now systemd-resolved"
}

# wg-quick needs a working resolvconf compatibility command to apply a config's
# `DNS =` line. Install one best-effort when missing, but never enable a network
# service silently.
ensure_resolvconf() {
    if command -v resolvconf >/dev/null 2>&1; then
        if resolvconf_uses_resolvectl && ! systemd_resolved_active; then
            warn_enable_resolved
        else
            ok "resolvconf provider available."
        fi
        return 0
    fi

    say "Installing a resolvconf provider (so tunnels with 'DNS =' can connect)"
    case "$PM" in
        apt-get)
            # Debian 12+/Ubuntu 24.04+: systemd-resolved itself provides the
            # resolvconf command (there is no separate systemd-resolvconf).
            if systemd_resolved_active; then
                pkg_install systemd-resolved || true
            else
                pkg_install openresolv || true
            fi
            ;;
        dnf|yum)      pkg_install systemd-resolved || true ;;
        pacman|zypper|apk|xbps-install|eopkg) pkg_install openresolv || true ;;
    esac

    if ! command -v resolvconf >/dev/null 2>&1; then
        warn "Could not install a resolvconf provider; tunnels with a 'DNS =' line"
        warn "may fail until a compatible provider is installed."
    elif resolvconf_uses_resolvectl && ! systemd_resolved_active; then
        warn_enable_resolved
    else
        ok "resolvconf provider installed."
    fi
}

# ---------------------------------------------------------------------------
# Ownership: distro packages, duplicates and the install manifest
# ---------------------------------------------------------------------------
# Print "<manager> <package>" when a distro package owns the path.
pkg_owner() {
    local path="$1" out=""
    if command -v dpkg-query >/dev/null 2>&1; then
        out="$(dpkg-query -S "$path" 2>/dev/null | grep -v '^diversion' | head -n 1 | cut -d: -f1 | cut -d, -f1)" || out=""
        [ -z "$out" ] || { printf 'dpkg %s\n' "$out"; return 0; }
    fi
    if command -v rpm >/dev/null 2>&1; then
        out="$(rpm -qf --queryformat '%{NAME}\n' "$path" 2>/dev/null | head -n 1)" || out=""
        [ -z "$out" ] || { printf 'rpm %s\n' "$out"; return 0; }
    fi
    if command -v pacman >/dev/null 2>&1; then
        out="$(pacman -Qqo "$path" 2>/dev/null | head -n 1)" || out=""
        [ -z "$out" ] || { printf 'pacman %s\n' "$out"; return 0; }
    fi
    if command -v apk >/dev/null 2>&1; then
        out="$(apk info --who-owns "$path" 2>/dev/null | sed -n 's/.* is owned by //p' | head -n 1)" || out=""
        [ -z "$out" ] || { printf 'apk %s\n' "$out"; return 0; }
    fi
    if command -v xbps-query >/dev/null 2>&1; then
        out="$(xbps-query -o "$path" 2>/dev/null | head -n 1 | cut -d: -f1)" || out=""
        [ -z "$out" ] || { printf 'xbps %s\n' "$out"; return 0; }
    fi
    return 1
}

pkg_remove_hint() {
    case "$1" in
        dpkg) printf 'sudo apt remove %s' "$2" ;;
        rpm) printf 'sudo dnf remove %s' "$2" ;;
        pacman) printf 'sudo pacman -R %s' "$2" ;;
        apk) printf 'sudo apk del %s' "${2%-*-r*}" ;;
        xbps) printf 'sudo xbps-remove %s' "${2%-*}" ;;
        *) printf 'your package manager' ;;
    esac
}

# Refuse a second copy next to a distro package: both would be on PATH, use
# separate helpers/grants, and the package manager would never update ours.
check_duplicate_install() {
    local path owner found=""
    if [ "$PREFIX" = /usr ]; then
        for path in "$BIN" "$HELPER"; do
            if owner="$(pkg_owner "$path")"; then
                [ "$FORCE" = 1 ] || die "$path belongs to the ${owner% *} package '${owner#* }'. Update it with the package manager, or remove it first ($(pkg_remove_hint "${owner% *}" "${owner#* }")). Pass --force to overwrite package files anyway."
                warn "--force: overwriting $path from the ${owner% *} package '${owner#* }'."
            fi
        done
        for path in "/usr/local/bin/$APP_BIN" "/usr/local/lib/$APP/wg-helper"; do
            if as_root test -e "$path"; then
                warn "Another copy exists under /usr/local ($path) and shadows $PREFIX on PATH."
                warn "Remove it with: PREFIX=/usr/local ./install.sh uninstall"
                break
            fi
        done
        return 0
    fi
    for path in "$PKG_BIN" "$PKG_HELPER"; do
        as_root test -e "$path" || continue
        if owner="$(pkg_owner "$path")"; then
            found="$path (from the ${owner% *} package '${owner#* }'; remove with: $(pkg_remove_hint "${owner% *}" "${owner#* }"))"
        else
            found="$path"
        fi
        break
    done
    [ -n "$found" ] || return 0
    if [ "$FORCE" = 1 ]; then
        warn "--force: installing a second copy into $PREFIX although $found exists."
        warn "The copy that comes first on PATH wins; keep only one of them."
        return 0
    fi
    die "wireguard-gui is already installed by a package: $found. Installing into $PREFIX as well would leave two copies with separate helpers and authorization. Update the package with your package manager, remove it first, or pass --force to install a second copy anyway."
}

manifest_get() {
    as_root sed -n "s/^$1=//p" "$MANIFEST" 2>/dev/null | head -n 1
}

# Values restored from an earlier install (manifest, or detected for installs
# made before the manifest existed).
PREV_INSTALLED=0; PREV_AUTH=""; PREV_AUTH_USER=""; PREV_DESKTOP=""; PREV_FORCE=0
PREV_GROUP_CREATED=0; PREV_MEMBER_ADDED=0; PREV_SELINUX_FCONTEXT=0; PREV_DIRS=""

load_previous_install() {
    local value dir
    if as_root test -e "$MANIFEST"; then
        validate_install_path "$MANIFEST" file
        PREV_INSTALLED=1
        value="$(manifest_get auth)"
        case "$value" in sudoers|polkit|none) PREV_AUTH="$value" ;; esac
        value="$(manifest_get auth_user)"
        [[ "$value" =~ ^[a-zA-Z_][a-zA-Z0-9_-]*$ ]] && PREV_AUTH_USER="$value"
        value="$(manifest_get desktop)"
        case "$value" in 0|1) PREV_DESKTOP="$value" ;; esac
        [ "$(manifest_get force)" = 1 ] && PREV_FORCE=1
        [ "$(manifest_get group_created)" = 1 ] && PREV_GROUP_CREATED=1
        [ "$(manifest_get group_member_added)" = 1 ] && PREV_MEMBER_ADDED=1
        [ "$(manifest_get selinux_fcontext)" = 1 ] && PREV_SELINUX_FCONTEXT=1
        for dir in $(manifest_get created_dirs); do
            case "$dir" in
                "$PREFIX"/*|"$POLKIT_DIR") [[ "$dir" =~ ^/[A-Za-z0-9_./-]+$ ]] && PREV_DIRS="$PREV_DIRS $dir" ;;
            esac
        done
    elif as_root test -e "$HELPER"; then
        # Installed by an installer that predates the manifest: infer the
        # choices from what is on disk so an update keeps them.
        PREV_INSTALLED=1
        if as_root test -e "$POLKIT_RULE"; then
            PREV_AUTH=polkit
        elif as_root test -e "$SUDOERS"; then
            PREV_AUTH=sudoers
            value="$(as_root sed -n 's/^\([a-zA-Z_][a-zA-Z0-9_-]*\) ALL=(root) NOPASSWD: .*/\1/p' "$SUDOERS" | head -n 1)"
            PREV_AUTH_USER="$value"
        else
            PREV_AUTH=none
        fi
        if as_root test -e "$DESKTOP"; then PREV_DESKTOP=1; else PREV_DESKTOP=0; fi
    fi
}

apply_previous_choices() {
    if [ "$UPDATE_MODE" = 1 ] && [ "$PREV_INSTALLED" != 1 ]; then
        die "--update: no installer-managed copy found in $PREFIX; run ./install.sh for a first install."
    fi
    [ "$PREV_INSTALLED" = 1 ] || return 0
    if [ "$AUTH_EXPLICIT" = 0 ] && [ -n "$PREV_AUTH" ]; then
        if [ "$PREV_AUTH" != none ]; then
            AUTH_MODE="$PREV_AUTH"
            say "Keeping the existing $AUTH_MODE authorization (pass --sudoers or --polkit to change it)."
        elif [ "$UPDATE_MODE" = 1 ]; then
            # A root-only install had no helper grant; an update adds none.
            AUTH_MODE=none
        fi
    fi
    if [ "$DESKTOP_EXPLICIT" = 0 ] && [ -n "$PREV_DESKTOP" ]; then
        INSTALL_DESKTOP="$PREV_DESKTOP"
    fi
    [ "$PREV_FORCE" = 1 ] && FORCE=1
    # The updater runs on behalf of whoever clicked "update"; keep the grant on
    # the account it was made for.
    if [ "$UPDATE_MODE" = 1 ] && [ -n "$PREV_AUTH_USER" ] && id "$PREV_AUTH_USER" >/dev/null 2>&1; then
        REAL_USER="$PREV_AUTH_USER"
    fi
    return 0
}

install_version() {
    local v=""
    if [ -x "${WG_PREBUILT_DIR:-$HERE/target/release}/wireguard-gui" ]; then
        v="$(as_user "${WG_PREBUILT_DIR:-$HERE/target/release}/wireguard-gui" --version 2>/dev/null </dev/null | awk 'NR==1 {print $NF}')" || v=""
    fi
    if [[ ! "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] && [ -f "$HERE/Cargo.toml" ]; then
        v="$(sed -n 's/^version = "\([0-9.]*\)"/\1/p' "$HERE/Cargo.toml" | head -n 1)"
    fi
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || v=unknown
    printf '%s' "$v"
}

# ---------------------------------------------------------------------------
# Transaction: back up every path we may change, restore it on any failure
# ---------------------------------------------------------------------------
TXN_DIR=""; TXN_ACTIVE=0; TXN_N=0
TXN_PATH=(); TXN_SAVED=()
CREATED_DIRS=""; GROUP_CREATED=0; MEMBER_ADDED=0; SELINUX_FCONTEXT_ADDED=0

# Test hook: WG_INSTALL_INJECT_FAILURE=<step> makes that step fail so the
# rollback path can be exercised. It can only make an install fail.
inject_failure() {
    if [ "${WG_INSTALL_INJECT_FAILURE:-}" = "$1" ]; then
        die "Injected failure at step '$1' (WG_INSTALL_INJECT_FAILURE)."
    fi
    return 0
}

txn_begin() {
    local path i
    TXN_DIR="$(as_root mktemp -d "/tmp/$APP-install.XXXXXX")" \
        || die "Could not create a private backup directory; nothing was changed."
    for path in "$@"; do
        i="$TXN_N"
        TXN_PATH[i]="$path"
        TXN_SAVED[i]=0
        if as_root test -e "$path" || as_root test -L "$path"; then
            as_root cp -a "$path" "$TXN_DIR/$i" \
                || die "Could not back up $path; nothing was changed."
            TXN_SAVED[i]=1
        fi
        TXN_N=$((i + 1))
    done
    TXN_ACTIVE=1
}

txn_commit() {
    TXN_ACTIVE=0
    [ -z "$TXN_DIR" ] || as_root rm -rf "$TXN_DIR"
    TXN_DIR=""
}

group_remove_member() {
    as_root gpasswd -d "$1" wireguard >/dev/null 2>&1 \
        || as_root deluser "$1" wireguard >/dev/null 2>&1 \
        || as_root delgroup "$1" wireguard >/dev/null 2>&1
}

group_delete() {
    as_root groupdel wireguard >/dev/null 2>&1 || as_root delgroup wireguard >/dev/null 2>&1
}

txn_rollback() {
    local i path dir failed=0 dirs=""
    [ "$TXN_ACTIVE" = 1 ] || return 0
    TXN_ACTIVE=0
    set +e
    warn "Installation failed - restoring the previous installation."
    for ((i = TXN_N - 1; i >= 0; i--)); do
        path="${TXN_PATH[i]}"
        as_root rm -f "$path"
        as_root rm -f "$path.wg-new.$$"
        if [ "${TXN_SAVED[i]}" = 1 ]; then
            as_root cp -a "$TXN_DIR/$i" "$path" || { warn "Could not restore $path"; failed=1; }
        fi
    done
    if [ "$MEMBER_ADDED" = 1 ]; then
        group_remove_member "$REAL_USER" || { warn "Could not remove $REAL_USER from 'wireguard'."; failed=1; }
    fi
    if [ "$GROUP_CREATED" = 1 ]; then
        group_delete || { warn "Could not delete the 'wireguard' group created by this run."; failed=1; }
    fi
    if [ "$SELINUX_FCONTEXT_ADDED" = 1 ]; then
        as_root semanage fcontext -d "${HELPER//./\\.}" >/dev/null 2>&1 || true
    fi
    for dir in $CREATED_DIRS; do dirs="$dir $dirs"; done
    for dir in $dirs; do
        # update-desktop-database leaves a cache in an applications directory
        # this run created; it would keep the directory (and parents) behind.
        case "$dir" in
            */share/applications)
                [ "$(as_root ls -A "$dir" 2>/dev/null)" != mimeinfo.cache ] \
                    || as_root rm -f "$dir/mimeinfo.cache" ;;
        esac
        as_root rmdir "$dir" 2>/dev/null
    done
    if selinux_enabled && command -v restorecon >/dev/null 2>&1; then
        for path in "${TXN_PATH[@]}"; do
            as_root test -e "$path" && as_root restorecon -F "$path" >/dev/null 2>&1
        done
    fi
    if [ "$failed" = 0 ]; then
        as_root rm -rf "$TXN_DIR"
        warn "Previous state restored; nothing was changed."
    else
        warn "Some files could not be restored; backups are kept in $TXN_DIR."
    fi
}

on_exit() {
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        txn_rollback
    fi
    return "$rc"
}

# Create a directory (and missing parents) without touching an existing one,
# recording what we created for rollback and uninstall.
ensure_dir() {
    local dir="$1" probe="$1" todo=""
    while [ -n "$probe" ] && [ "$probe" != / ] && ! as_root test -d "$probe"; do
        todo="$probe $todo"
        probe="$(dirname "$probe")"
    done
    [ -n "$todo" ] || return 0
    as_root install -d -m755 "$dir"
    CREATED_DIRS="$CREATED_DIRS $todo"
}

# Write via a temporary sibling + rename, so a failure never leaves a
# truncated binary behind and a running copy keeps its old inode.
install_file() {
    local mode="$1" src="$2" dest="$3"
    as_root install -m "$mode" "$src" "$dest.wg-new.$$" \
        || { as_root rm -f "$dest.wg-new.$$"; return 1; }
    as_root mv -f "$dest.wg-new.$$" "$dest" \
        || { as_root rm -f "$dest.wg-new.$$"; return 1; }
}

# ---------------------------------------------------------------------------
# SELinux
# ---------------------------------------------------------------------------
selinux_enabled() {
    if command -v selinuxenabled >/dev/null 2>&1; then
        selinuxenabled
    else
        [ -e /sys/fs/selinux/enforce ]
    fi
}

# Files written under /usr/local or moved from /tmp can carry a label that
# pkexec/sudo may not execute. Restore default contexts and give the helper an
# executable (bin_t) context, persistently when semanage is available.
apply_selinux_labels() {
    local path ctx
    local -a paths=()
    selinux_enabled || return 0
    say "Restoring SELinux file contexts"
    if command -v semanage >/dev/null 2>&1; then
        if as_root semanage fcontext -a -t bin_t "${HELPER//./\\.}" >/dev/null 2>&1; then
            SELINUX_FCONTEXT_ADDED=1
        elif ! as_root semanage fcontext -m -t bin_t "${HELPER//./\\.}" >/dev/null 2>&1; then
            warn "semanage could not record a bin_t rule for $HELPER."
        fi
    fi
    for path in "$BIN" "$ALIAS" "$LIBDIR" "$HELPER" "$MANIFEST" "$DESKTOP" "$ICON" "$SUDOERS" "$POLKIT_RULE"; do
        as_root test -e "$path" && paths+=("$path")
    done
    if [ "${#paths[@]}" -gt 0 ] && command -v restorecon >/dev/null 2>&1; then
        as_root restorecon -F "${paths[@]}" \
            || warn "restorecon failed; check the labels with: ls -Z ${paths[*]}"
    else
        warn "restorecon not found (install policycoreutils); file labels were not restored."
    fi
    ctx="$(as_root stat -c %C "$HELPER" 2>/dev/null || true)"
    case "$ctx" in
        *:bin_t:*) ;;
        *)
            if command -v chcon >/dev/null 2>&1 && as_root chcon -t bin_t "$HELPER"; then
                warn "Labelled $HELPER bin_t with chcon. A full relabel resets this; install"
                warn "semanage (policycoreutils-python-utils) and re-run to make it persistent."
            else
                # Not "restored": the exec check below decides whether it runs.
                warn "The helper's SELinux context is '$ctx' (expected bin_t)."
                return 0
            fi ;;
    esac
    ok "SELinux contexts restored."
}

# Run the installed helper the way sudo/pkexec will: as root, by path. Without a
# verb it prints a usage error and exits 2. Exit 126/127 or a signal means it
# never ran; the shell's own message tells a denial (SELinux, a noexec mount)
# from a missing loader (wrong CPU or C library), since shells differ in codes.
verify_helper_executes() {
    local rc=0 out mnt
    out="$(as_root sh -c '"$1" </dev/null 2>&1 >/dev/null' wg-helper-probe "$HELPER")" || rc=$?
    [ "$rc" -ge 126 ] || { ok "Helper runs as root."; return 0; }
    case "$out" in
        *"ermission denied"*)
            # A noexec mount also denies with EACCES; name it before blaming
            # SELinux. The innermost mount holding the helper wins.
            mnt="$(awk -v path="$HELPER" '
                path == $2 || index(path, ($2 == "/" ? "/" : $2 "/")) == 1 {
                    if (length($2) >= best) { best = length($2); top = $2; opts = "," $4 "," }
                }
                END { if (opts ~ /,noexec,/) print top }' /proc/mounts 2>/dev/null)" || mnt=""
            if [ -n "$mnt" ]; then
                die "Executing $HELPER was denied: $mnt is mounted noexec. Install into a PREFIX on a filesystem that allows execution (or remount $mnt with exec), then re-run. The previous installation is restored."
            fi
            if selinux_enabled; then
                die "Executing $HELPER was denied (SELinux). Check 'sudo ausearch -m avc -ts recent' and 'ls -Z $HELPER'; fix the label with 'sudo semanage fcontext -a -t bin_t ${HELPER//./\\.} && sudo restorecon -v $HELPER', then re-run. The previous installation is restored."
            fi
            die "Executing $HELPER was denied (permission denied; is $PREFIX mounted noexec?). The previous installation is restored." ;;
        *)
            die "$HELPER cannot run on this system (exit $rc: ${out:-no output}): wrong CPU architecture or C library for these binaries. Build from source or use the matching release. The previous installation is restored." ;;
    esac
}

# ---------------------------------------------------------------------------
# Authorisation: sudoers drop-in (default) or polkit rule
# ---------------------------------------------------------------------------
wireguard_group_exists() {
    if command -v getent >/dev/null 2>&1; then
        getent group wireguard >/dev/null 2>&1
    else
        grep -q '^wireguard:' /etc/group
    fi
}

wireguard_user_enrolled() {
    local memberships
    memberships="$(id -nG "$REAL_USER" 2>/dev/null)" || return 1
    case " $memberships " in
        *" wireguard "*) return 0 ;;
        *) return 1 ;;
    esac
}

configure_authorization() {
    if [ "$AUTH_MODE" = "none" ]; then
        ok "No passwordless helper grant (unchanged from the existing installation)."
        return 0
    fi
    validate_authorization_path
    if [ "$AUTH_MODE" = "polkit" ]; then
        say "Installing polkit rule (passwordless for the 'wireguard' group)"
        # Gate the root helper on a dedicated group so it is NOT exposed to every
        # active-local user (who could otherwise read another user's private keys
        # via the helper's read/showconf/dump verbs).
        if ! wireguard_group_exists; then
            # BusyBox addgroup fails when the group already exists. Recheck
            # after creation attempts as another installer may have created it.
            as_root groupadd -f wireguard 2>/dev/null \
                || as_root addgroup wireguard 2>/dev/null || true
            wireguard_group_exists \
                || die "Could not create the wireguard authorization group; existing authorization kept."
            GROUP_CREATED=1
        fi
        if [ -n "$REAL_USER" ] && [ "$REAL_USER" != "root" ] \
                && printf '%s' "$REAL_USER" | grep -qE '^[a-zA-Z_][a-zA-Z0-9_-]*$' \
                && id "$REAL_USER" >/dev/null 2>&1; then
            if wireguard_user_enrolled; then
                ok "$REAL_USER is already in the 'wireguard' group."
            else
                as_root usermod -aG wireguard "$REAL_USER" 2>/dev/null \
                    || as_root gpasswd -a "$REAL_USER" wireguard 2>/dev/null \
                    || as_root adduser "$REAL_USER" wireguard 2>/dev/null || true
                wireguard_user_enrolled \
                    || die "Could not enroll $REAL_USER in wireguard; existing authorization kept."
                MEMBER_ADDED=1
                ok "Added $REAL_USER to the 'wireguard' group (log out and back in for it to take effect)."
            fi
        else
            warn "Couldn't add a user to the 'wireguard' group automatically."
            warn "Add yours with: sudo usermod -aG wireguard <your-username>  (then re-login)."
        fi
        inject_failure auth-group
        [ -d "$POLKIT_DIR" ] || ensure_dir "$POLKIT_DIR"
        validate_install_path "$HELPER" helper
        as_root install -m644 "$HERE/packaging/49-wireguard-gui.rules" "$POLKIT_RULE" \
            || die "Could not install polkit rule; existing authorization kept."
        as_root rm -f "$SUDOERS" || die "Could not remove the previous sudoers grant."
        AUTH_RESULT=polkit
        ok "polkit rule installed at $POLKIT_RULE (limited to the 'wireguard' group)."
    elif [ -z "$REAL_USER" ] || [ "$REAL_USER" = "root" ]; then
        warn "Couldn't determine the invoking user - skipping the passwordless drop-in."
        warn "Run wireguard-gui as root, re-run as that user, or pass WG_REAL_USER=<name>."
    elif ! printf '%s' "$REAL_USER" | grep -qE '^[a-zA-Z_][a-zA-Z0-9_-]*$' \
            || ! id "$REAL_USER" >/dev/null 2>&1; then
        # Reject anything that isn't a real, plain username - visudo alone does NOT
        # catch an injected spec like "u ALL=(ALL) NOPASSWD: ALL #".
        die "Refusing to write a sudoers rule for an invalid/unknown user: '$REAL_USER'."
    else
        say "Installing sudoers drop-in (passwordless wg-helper for $REAL_USER)"
        tmp="$(mktemp)"
        printf '%s ALL=(root) NOPASSWD: %s\n' "$REAL_USER" "$HELPER" > "$tmp"
        if as_root visudo -cf "$tmp" >/dev/null 2>&1; then
            validate_install_path "$HELPER" helper
            as_root install -m440 "$tmp" "$SUDOERS" || { rm -f "$tmp"; die "Could not install sudoers grant; existing authorization kept."; }
            as_root rm -f "$POLKIT_RULE" || { rm -f "$tmp"; die "Could not remove the previous polkit grant."; }
            AUTH_RESULT=sudoers
            ok "Passwordless helper set up for $REAL_USER (works even without sudo-group membership)."
        else
            rm -f "$tmp"
            die "sudoers validation failed; existing authorization kept. Re-run with --polkit or fix sudo."
        fi
        rm -f "$tmp"
    fi
}

write_manifest() {
    local tmp auth="${AUTH_RESULT:-}" auth_user="" origin
    if [ -z "$auth" ]; then
        # Grant skipped (root install without a user): keep describing what
        # is still on disk so a later update preserves it.
        if as_root test -e "$POLKIT_RULE"; then auth=polkit
        elif as_root test -e "$SUDOERS"; then auth=sudoers
        else auth=none; fi
        auth_user="$PREV_AUTH_USER"
    elif [ "$REAL_USER" != root ]; then
        auth_user="$REAL_USER"
    fi
    if [ "$UPDATE_MODE" = 1 ]; then origin=release
    elif [ -n "$BINARIES_DIR" ]; then origin=prebuilt
    elif [ -f "$HERE/Cargo.toml" ] && [ "${WG_USE_PREBUILT:-0}" != 1 ]; then origin=source
    else origin=release; fi
    tmp="$(mktemp)"
    {
        printf '# Installation record written by install.sh and read by the in-app\n'
        printf '# updater so updates keep the same method, prefix and authorization.\n'
        printf 'format=1\n'
        printf 'app=%s\n' "$APP"
        printf 'method=installer\n'
        printf 'origin=%s\n' "$origin"
        printf 'version=%s\n' "$INSTALL_VERSION"
        printf 'prefix=%s\n' "$PREFIX"
        printf 'auth=%s\n' "$auth"
        printf 'auth_user=%s\n' "$auth_user"
        if as_root test -e "$DESKTOP"; then printf 'desktop=1\n'; else printf 'desktop=0\n'; fi
        printf 'force=%s\n' "$FORCE"
        printf 'group_created=%s\n' "$(( PREV_GROUP_CREATED | GROUP_CREATED ))"
        printf 'group_member_added=%s\n' "$(( PREV_MEMBER_ADDED | MEMBER_ADDED ))"
        printf 'selinux_fcontext=%s\n' "$(( PREV_SELINUX_FCONTEXT | SELINUX_FCONTEXT_ADDED ))"
        # shellcheck disable=SC2086 # Space-separated path lists (no spaces in paths).
        printf 'created_dirs=%s\n' "$(printf '%s\n' $PREV_DIRS $CREATED_DIRS | awk 'NF && !seen[$0]++' | tr '\n' ' ' | sed 's/ $//')"
    } > "$tmp"
    install_file 644 "$tmp" "$MANIFEST" || { rm -f "$tmp"; die "Could not write $MANIFEST."; }
    rm -f "$tmp"
}

# ---------------------------------------------------------------------------
# Uninstall
# ---------------------------------------------------------------------------
sibling_uses_group() {
    as_root test -e "/etc/polkit-1/rules.d/49-$SIBLING_APP.rules" \
        || as_root test -e "/usr/share/polkit-1/rules.d/49-$SIBLING_APP.rules"
}

uninstall() {
    local path owner dir dirs="" member had_desktop
    say "Removing wireguard-gui from $PREFIX"
    if [ "$PREFIX" = /usr ] && owner="$(pkg_owner "$BIN")"; then
        die "$BIN belongs to the ${owner% *} package '${owner#* }'; remove it with: $(pkg_remove_hint "${owner% *}" "${owner#* }")"
    fi
    validate_install_path "$PREFIX" directory
    load_previous_install
    had_desktop=0
    as_root test -e "$DESKTOP" && had_desktop=1
    as_root rm -f "$BIN" "$HELPER" "$MANIFEST" "$DESKTOP" "$ICON" "$SUDOERS"
    if as_root test -L "$ALIAS" || as_root test -f "$ALIAS"; then as_root rm -f "$ALIAS"; fi
    # The .deb installs a rule with the same name; leave package files alone.
    if ! pkg_owner "$POLKIT_RULE" >/dev/null; then as_root rm -f "$POLKIT_RULE"; fi
    if [ "$PREV_SELINUX_FCONTEXT" = 1 ] && command -v semanage >/dev/null 2>&1; then
        as_root semanage fcontext -d "${HELPER//./\\.}" >/dev/null 2>&1 || true
    fi
    if [ "$PREV_GROUP_CREATED" = 1 ] || [ "$PREV_MEMBER_ADDED" = 1 ]; then
        if sibling_uses_group; then
            warn "Keeping the 'wireguard' group: $SIBLING_APP still uses it."
        else
            if [ "$PREV_MEMBER_ADDED" = 1 ] && [ -n "$PREV_AUTH_USER" ]; then
                group_remove_member "$PREV_AUTH_USER" || true
            fi
            if [ "$PREV_GROUP_CREATED" = 1 ]; then
                member="$( (getent group wireguard 2>/dev/null || grep '^wireguard:' /etc/group) | cut -d: -f4)"
                if [ -z "$member" ]; then
                    group_delete || warn "Could not delete the 'wireguard' group."
                else
                    warn "Keeping the 'wireguard' group: it still has members ($member)."
                fi
            fi
        fi
    fi
    as_root rmdir "$LIBDIR" 2>/dev/null || true
    # Refresh the desktop cache only if an entry was removed; drop the cache
    # once no entries remain (it only indexes .desktop files).
    if [ "$had_desktop" = 1 ] && as_root test -d "$PREFIX/share/applications"; then
        if ls "$PREFIX/share/applications"/*.desktop >/dev/null 2>&1; then
            command -v update-desktop-database >/dev/null 2>&1 \
                && as_root update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
        else
            as_root rm -f "$PREFIX/share/applications/mimeinfo.cache"
        fi
    fi
    for dir in $PREV_DIRS; do dirs="$dir $dirs"; done
    for dir in $dirs; do
        # An empty desktop cache in a directory we created is ours too.
        path="$dir/mimeinfo.cache"
        if [ "$dir" = "$PREFIX/share/applications" ] && as_root test -f "$path" \
                && [ "$(as_root ls -A "$dir")" = mimeinfo.cache ]; then
            as_root rm -f "$path"
        fi
        as_root rmdir "$dir" 2>/dev/null || true
    done
    ok "Uninstalled. (Your /etc/wireguard configs were left untouched.)"
    exit 0
}

# ---------------------------------------------------------------------------
# Go
# ---------------------------------------------------------------------------
printf "${B}wireguard-gui installer${N}\n"
[ "$ACTION" = "uninstall" ] && uninstall
[ -n "$PM" ] && say "Detected package manager: ${PM}" || true

validate_install_destinations
load_previous_install
apply_previous_choices
check_duplicate_install
validate_install_destinations
prepare_prebuilt
check_prebuilt_arch
install_pkgs
ensure_sudo
verify_build_deps
verify_runtime_deps
ensure_resolvconf
ensure_optional_tools
build_app
INSTALL_VERSION="$(install_version)"
STAGE="${WG_PREBUILT_DIR:-$HERE/target/release}"

validate_install_destinations
trap on_exit EXIT
trap 'exit 130' INT TERM HUP
txn_begin "$BIN" "$ALIAS" "$HELPER" "$MANIFEST" "$DESKTOP" "$ICON" "$SUDOERS" "$POLKIT_RULE"

say "Installing wireguard-gui $INSTALL_VERSION into $PREFIX"
ensure_dir "$LIBDIR"
ensure_dir "$PREFIX/bin"
install_file 755 "$STAGE/wireguard-gui" "$BIN" || die "Could not install $BIN."
inject_failure binaries
as_root ln -sfn "$APP_BIN" "$ALIAS"
install_file 755 "$STAGE/wg-helper" "$HELPER" || die "Could not install $HELPER."
inject_failure helper
ensure_dir "$PREFIX/share/applications"
ensure_dir "$ICON_DIR"
install_file 644 "$HERE/packaging/wireguard-gui.desktop" "$DESKTOP" || die "Could not install $DESKTOP."
install_file 644 "$HERE/packaging/wireguard-gui.svg" "$ICON" || die "Could not install $ICON."
command -v update-desktop-database >/dev/null 2>&1 && \
    as_root update-desktop-database "$PREFIX/share/applications" 2>/dev/null || true
ok "Desktop entry installed."
inject_failure desktop
ok "Files installed."

AUTH_RESULT=""
configure_authorization
inject_failure auth
apply_selinux_labels
inject_failure selinux
verify_helper_executes
write_manifest
inject_failure manifest
txn_commit

printf "\n${G}${B}Done!${N} Launch ${B}WireGuard${N} from your app menu, or run: ${B}wireguard-gui${N}\n"
if [ -n "$REAL_USER" ] && [ "$REAL_USER" != "root" ]; then
    printf "Run it as ${B}%s${N} - no sudo prompt, no need to be root.\n" "$REAL_USER"
fi
printf "Uninstall any time with: ${B}./install.sh uninstall${N}\n"
