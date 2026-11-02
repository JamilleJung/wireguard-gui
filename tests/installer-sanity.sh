#!/usr/bin/env bash
set -euo pipefail

script="${1:-install.sh}"

bash -n "$script"

# Check only the actual argument parser in isolation: a regression must never
# run the installer's package or privilege actions on the test host.
args_script="$(mktemp)"
err="$(mktemp)"
awk '/^ACTION=/ { reading=1 } reading { print } reading && $0 == "done" { exit }' "$script" > "$args_script"
grep -q 'for arg in' "$args_script"
if PATH=/nonexistent /bin/bash "$args_script" --invalid-installer-option > /dev/null 2> "$err"; then
    rm -f "$err" "$args_script"
    echo "installer accepted an unknown option" >&2
    exit 1
else
    status=$?
fi
test "$status" -eq 2
grep -q 'Unknown installer option' "$err"
rm -f "$err" "$args_script"

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


# Polkit mode cannot remove a working sudoers grant on a host without pkexec.
(
    eval "$(awk '$0 == "ensure_sudo() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    export AUTH_MODE=polkit
    die() { exit 7; }
    command() { return 1; }
    as_root() { echo 'unexpected package operation' >&2; exit 9; }
    if (ensure_sudo); then
        echo 'polkit mode accepted missing pkexec' >&2; exit 1
    else
        test "$?" -eq 7
    fi
)

# Exercise Linux metadata checks with an isolated fake stat provider. No test
# traverses or changes a privileged directory, even on a root CI runner.
(
    for fn in validate_install_path validate_install_destinations; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    die() { echo "$*" >&2; exit 7; }
    BAD_PATH=""
    BAD_METADATA=""
    MISSING_PATH=""
    install_path_metadata() {
        if [[ "$1" = "$MISSING_PATH" ]]; then
            echo 'stat: No such file or directory' >&2; return 1
        elif [[ "$1" = "$BAD_PATH" ]]; then
            printf '%s\n' "$BAD_METADATA"
        elif [[ "$1" = */wg-helper || "$1" = */application ]]; then
            printf '0 755 81ed 1\n'
        else
            printf '0 755 41ed 1\n'
        fi
    }
    validate_install_path /usr/local/lib/project/wg-helper helper
    for path in relative /usr/ /usr//local /usr/./local /usr/../local \
        '/usr/a b' '/usr/a,b' '/usr/a:b' '/usr/a#b' '/usr/a*b' '/usr/a\b' $'/usr/a\nb'; do
        if (validate_install_path "$path" directory) 2>/dev/null; then
            echo "unsafe install syntax accepted: $path" >&2; exit 1
        fi
    done
    for metadata in '1000 755 41ed 1' '0 775 41fd 1' '0 755 a1ed 1' \
        '0 755 81ed 1' 'malformed metadata'; do
        BAD_PATH=/usr/local/lib
        BAD_METADATA="$metadata"
        if (validate_install_path /usr/local/lib/project/wg-helper helper) 2>/dev/null; then
            echo "unsafe helper ancestor accepted: $metadata" >&2; exit 1
        fi
    done
    BAD_PATH=/usr/local/lib/project/wg-helper
    for metadata in '1000 755 81ed 1' '0 775 81fd 1' '0 755 a1ed 1' \
        '0 755 81ed 2' '0 644 81a4 1' '0 600 11a0 1'; do
        BAD_METADATA="$metadata"
        if (validate_install_path "$BAD_PATH" helper) 2>/dev/null; then
            echo "unsafe installed helper accepted: $metadata" >&2; exit 1
        fi
    done
    BAD_PATH=""
    MISSING_PATH=/usr/local/lib/project
    validate_install_path /usr/local/lib/project/wg-helper file
    if (validate_install_path /usr/local/lib/project/wg-helper helper) 2>/dev/null; then
        echo 'missing installed helper accepted' >&2; exit 1
    fi
    # The planned-path preflight must inspect existing project directories and
    # app targets too, before the installer creates/copies any privileged file.
    MISSING_PATH=""
    PREFIX=/usr/local
    BIN=/usr/local/bin/application
    HELPER=/usr/local/lib/project/wg-helper
    DESKTOP=/usr/local/share/applications/desktop
    ICON_DIR=/usr/local/share/icons/apps
    ICON="$ICON_DIR/icon"
    INSTALL_DESKTOP=0
    export PREFIX BIN HELPER DESKTOP ICON_DIR ICON INSTALL_DESKTOP AUTH_MODE # Read by extracted functions.
    AUTH_MODE=sudoers
    BAD_PATH=/usr/local/lib/project
    BAD_METADATA='1000 755 41ed 1'
    if (validate_install_destinations) 2>/dev/null; then
        echo 'planned helper directory trust check missing' >&2; exit 1
    fi
    BAD_PATH="$BIN"
    BAD_METADATA='0 755 a1ed 1'
    if (validate_install_destinations) 2>/dev/null; then
        echo 'planned application target symlink accepted' >&2; exit 1
    fi
)

# Auth-mode changes must not leave the previous passwordless grant active or
# delete the working grant before replacement validation succeeds.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in validate_authorization_path configure_authorization; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { :; }
    die() { echo "$*" >&2; exit 1; }
    id() { [ "$*" = fixture ]; }
    HERE="$fixture/bundle"
    mkdir -p "$HERE/packaging"
    # Resolve the asset from the installer, independent of repository dirname.
    asset="$(sed -n 's/.*"\$HERE\/packaging\/\(49-wireguard-[a-z]*\.rules\)".*/\1/p' "$script" | head -n 1)"
    test -n "$asset"
    printf 'new polkit rule\n' > "$HERE/packaging/$asset"
    SUDOERS="$fixture/current.sudoers"
    POLKIT_RULE="$fixture/current.rules"
    HELPER="$fixture/helper"
    export AUTH_MODE HELPER PREFIX # Used by dynamically loaded installer functions.
    REAL_USER=fixture
    GROUP_FAIL=0
    VISUDO_FAIL=0
    PREFIX=/usr/local
    PATH_FAIL=0
    validate_install_path() { [ "$PATH_FAIL" = 0 ] || die 'untrusted helper path'; }
    as_root() {
        case "$1" in
            groupadd|addgroup) return 0 ;;
            usermod|gpasswd|adduser) test "$GROUP_FAIL" = 0 ;;
            visudo) test "$VISUDO_FAIL" = 0 ;;
            install)
                if [ "$2" = -d ]; then return 0; fi
                case "$4" in "$fixture"/*) command install "$2" "$3" "$4" ;;
                    *) echo 'unsafe test destination' >&2; exit 1 ;; esac
                ;;
            rm)
                case "$3" in "$fixture"/*) command rm "$2" "$3" ;;
                    *) echo 'unsafe test deletion' >&2; exit 1 ;; esac
                ;;
            *) echo "unexpected privileged command: $*" >&2; exit 1 ;;
        esac
    }

    AUTH_MODE=polkit
    printf 'old sudoers grant\n' > "$SUDOERS"
    # A failed trust check or unsupported polkit prefix keeps the old grant,
    # before group changes or writing either authorization replacement.
    for AUTH_MODE in polkit sudoers; do
        PATH_FAIL=1
        if (configure_authorization) 2>/dev/null; then
            echo 'untrusted helper received an authorization grant' >&2; exit 1
        fi
        grep -q 'old sudoers grant' "$SUDOERS"
        test ! -e "$POLKIT_RULE"
    done
    PATH_FAIL=0
    AUTH_MODE=polkit
    PREFIX=/opt/protected
    if (configure_authorization) 2>/dev/null; then
        echo 'custom polkit prefix received a nonmatching grant' >&2; exit 1
    fi
    grep -q 'old sudoers grant' "$SUDOERS"
    test ! -e "$POLKIT_RULE"
    PREFIX=/usr/local
    configure_authorization
    test ! -e "$SUDOERS"
    grep -q 'new polkit rule' "$POLKIT_RULE"

    AUTH_MODE=sudoers
    VISUDO_FAIL=1
    if (configure_authorization) 2>/dev/null; then
        echo 'invalid sudoers replacement unexpectedly succeeded' >&2; exit 1
    fi
    test -f "$POLKIT_RULE"
    test ! -e "$SUDOERS"

    VISUDO_FAIL=0
    configure_authorization
    test ! -e "$POLKIT_RULE"
    grep -q 'fixture ALL=(root) NOPASSWD:' "$SUDOERS"

    AUTH_MODE=polkit
    GROUP_FAIL=1
    if (configure_authorization) 2>/dev/null; then
        echo 'failed group enrollment unexpectedly succeeded' >&2; exit 1
    fi
    test -f "$SUDOERS"
    test ! -e "$POLKIT_RULE"
)


# Resolver probes must distinguish an inactive compatibility shim from a real
# provider and choose the package matching the backing service/distro.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in resolvconf_uses_resolvectl systemd_resolved_active warn_enable_resolved ensure_resolvconf; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { printf '%s\n' "$*" >> "$fixture/warnings"; }
    as_root() { printf '%s\n' "$*" >> "$fixture/packages"; }
    RESOLVCONF_PRESENT=1
    RESOLVED_ACTIVE=0
    command() {
        case "$*" in
            '-v resolvconf') test "$RESOLVCONF_PRESENT" = 1 && printf /usr/bin/resolvconf ;;
            '-v systemctl') return 0 ;;
            *) builtin command "$@" ;;
        esac
    }
    readlink() { printf /usr/bin/resolvectl; }
    systemctl() { test "$RESOLVED_ACTIVE" = 1; }
    export PM
    PM=dnf
    ensure_resolvconf
    grep -q 'systemd-resolved is inactive' "$fixture/warnings"
    test ! -e "$fixture/packages"

    RESOLVCONF_PRESENT=0
    ensure_resolvconf
    grep -q 'dnf install -y systemd-resolved' "$fixture/packages"
    if grep -q openresolv "$fixture/packages"; then
        echo 'RHEL resolver repair chose unavailable openresolv package' >&2; exit 1
    fi
    : > "$fixture/packages"
    PM=apt-get
    RESOLVED_ACTIVE=1
    ensure_resolvconf
    grep -q 'apt-get install -y systemd-resolvconf' "$fixture/packages"
)

echo "installer sanity tests passed"
