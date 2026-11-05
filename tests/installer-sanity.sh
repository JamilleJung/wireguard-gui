#!/usr/bin/env bash
# shellcheck disable=SC2034 # Fixture variables are read by installer functions loaded via eval.
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
require 'readlink -f.*resolvconf' 'resolvectl shim detection'
require 'systemctl is-active --quiet systemd-resolved\.service' 'active resolver service probe'
require 'systemctl enable --now systemd-resolved' 'manual inactive-shim recovery'
require 'dnf\|yum\).*systemd-resolved' 'RHEL-family resolver package'
reject 'dnf\|yum\).*openresolv' 'unavailable RHEL-family openresolv package'
# Refreshing pacman's databases without a full upgrade (-Sy) risks a partial
# upgrade on Arch; only -S --needed against the synced databases is allowed.
reject 'pacman[^"]*-Sy([^u]|$)' 'partial-upgrade-prone pacman -Sy'
reject 'pacman[^"]*-S[a-z]*y[a-z]* --' 'partial-upgrade-prone pacman refresh'
require 'pacman -S --needed --noconfirm' 'non-refreshing pacman install'
require 'install-manifest' 'install manifest for the updater'
require 'restorecon -F' 'SELinux context restore'
require 'semanage fcontext -a -t bin_t' 'persistent helper SELinux label'
require 'txn_rollback' 'installer rollback'

# Exercise the actual installer functions in a private fixture. Privileged
# commands are recorded, so these checks cannot install packages or files on
# the host, even when the test runner itself is root.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    calls="$fixture/package-calls"
    app="wireguard-gui"
    for fn in prebuilt_ready prepare_prebuilt pkg_install install_pkgs verify_build_deps build_app; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { :; }
    die() { echo "$*" >&2; exit 1; }
    as_root() { printf '%s\n' "$*" >> "$calls"; }
    as_user() { echo "unexpected source build" >&2; exit 1; }
    pkg_missing() { printf '%s\n' "$@"; }
    APT_UPDATED=0
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
    grep -q 'pacman -S --needed --noconfirm wireguard-tools' "$calls"
    if grep -Eq 'pacman -S[a-z]*y' "$calls"; then
        echo "installer sanity failed: pacman refreshed its databases without upgrading" >&2
        exit 1
    fi
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
    for fn in validate_authorization_path wireguard_group_exists wireguard_user_enrolled configure_authorization; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { :; }
    die() { echo "$*" >&2; exit 1; }
    id() {
        case "$*" in
            fixture) return 0 ;;
            '-nG fixture') printf '%s\n' "$MEMBERSHIPS" ;;
            *) return 1 ;;
        esac
    }
    getent() { [ "$*" = 'group wireguard' ] && [ "$GROUP_PRESENT" = 1 ]; }
    HERE="$fixture/bundle"
    mkdir -p "$HERE/packaging"
    # Resolve the asset from the installer, independent of repository dirname.
    asset="$(sed -n 's/.*"\$HERE\/packaging\/\(49-wireguard-[a-z]*\.rules\)".*/\1/p' "$script" | head -n 1)"
    test -n "$asset"
    printf 'new polkit rule\n' > "$HERE/packaging/$asset"
    SUDOERS="$fixture/current.sudoers"
    POLKIT_RULE="$fixture/current.rules"
    POLKIT_DIR="$fixture"
    inject_failure() { :; }
    HELPER="$fixture/helper"
    export AUTH_MODE HELPER PREFIX # Used by dynamically loaded installer functions.
    REAL_USER=fixture
    GROUP_FAIL=0
    GROUP_PRESENT=0
    CREATE_FAIL=0
    CREATE_RACE=0
    SHADOW_MISSING=0
    MEMBERSHIPS=fixture
    VISUDO_FAIL=0
    auth_calls="$fixture/auth-calls"
    PREFIX=/usr/local
    PATH_FAIL=0
    validate_install_path() { [ "$PATH_FAIL" = 0 ] || die 'untrusted helper path'; }
    as_root() {
        case "$1" in
            groupadd|addgroup)
                printf '%s\n' "$1" >> "$auth_calls"
                if [ "$CREATE_RACE" = 1 ]; then GROUP_PRESENT=1; return 1; fi
                if [ "$1" = groupadd ] && [ "$SHADOW_MISSING" = 1 ]; then return 127; fi
                [ "$CREATE_FAIL" = 0 ] || return 1
                GROUP_PRESENT=1
                ;;
            usermod|gpasswd|adduser)
                printf '%s\n' "$1" >> "$auth_calls"
                if [ "$1" != adduser ] && [ "$SHADOW_MISSING" = 1 ]; then return 127; fi
                [ "$GROUP_FAIL" = 0 ] || return 1
                MEMBERSHIPS='fixture wireguard'
                ;;
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
    GROUP_PRESENT=0
    CREATE_FAIL=1
    if (configure_authorization) 2>/dev/null; then
        echo 'failed group creation unexpectedly succeeded' >&2; exit 1
    fi
    test -f "$SUDOERS"
    test ! -e "$POLKIT_RULE"
    GROUP_PRESENT=1
    CREATE_FAIL=0
    GROUP_FAIL=1
    MEMBERSHIPS=fixture
    if (configure_authorization) 2>/dev/null; then
        echo 'failed group enrollment unexpectedly succeeded' >&2; exit 1
    fi
    test -f "$SUDOERS"
    test ! -e "$POLKIT_RULE"

    # An Alpine-style reinstall needs no Shadow commands when group/member
    # state is already correct; exact membership avoids prefix-name matches.
    SHADOW_MISSING=1
    CREATE_FAIL=1
    MEMBERSHIPS='fixture wireguard'
    : > "$auth_calls"
    configure_authorization
    test ! -s "$auth_calls"
    test ! -e "$SUDOERS"
    test -f "$POLKIT_RULE"
    printf 'old sudoers grant\n' > "$SUDOERS"
    rm "$POLKIT_RULE"
    MEMBERSHIPS='fixture wireguard-other'
    if (configure_authorization) 2>/dev/null; then
        echo 'similar group name was treated as wireguard membership' >&2; exit 1
    fi
    test -f "$SUDOERS"
    test ! -e "$POLKIT_RULE"

    # A first BusyBox-only install uses addgroup/adduser fallbacks. A failed
    # create is harmless only if a concurrent install made the group exist.
    CREATE_FAIL=0
    GROUP_FAIL=0
    GROUP_PRESENT=0
    MEMBERSHIPS=fixture
    : > "$auth_calls"
    configure_authorization
    grep -q '^addgroup$' "$auth_calls"
    grep -q '^adduser$' "$auth_calls"
    test "$GROUP_PRESENT" = 1
    test "$MEMBERSHIPS" = 'fixture wireguard'
    GROUP_PRESENT=0
    CREATE_RACE=1
    configure_authorization
    test "$GROUP_PRESENT" = 1

    # Without getent, use an exact local group record; never write /etc/group.
    command() {
        if [ "$*" = '-v getent' ]; then return 1; fi
        builtin command "$@"
    }
    grep() {
        if [ "$*" = '-q ^wireguard: /etc/group' ]; then
            test "$GROUP_PRESENT" = 1
        else
            command grep "$@"
        fi
    }
    wireguard_group_exists
    GROUP_PRESENT=0
    if wireguard_group_exists; then
        echo 'absent local group unexpectedly found' >&2; exit 1
    fi
)


# Resolver probes must distinguish an inactive compatibility shim from a real
# provider and choose the package matching the backing service/distro.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in resolvconf_uses_resolvectl systemd_resolved_active warn_enable_resolved pkg_install ensure_resolvconf; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { printf '%s\n' "$*" >> "$fixture/warnings"; }
    as_root() { printf '%s\n' "$*" >> "$fixture/packages"; }
    pkg_missing() { printf '%s\n' "$@"; }
    APT_UPDATED=0
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
    grep -q 'apt-get install -y systemd-resolved' "$fixture/packages"
)

# pacman installs never refresh the databases on their own; a failure tells the
# administrator to run a full upgrade instead.
(
    eval "$(awk '$0 == "pkg_install() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    warn() { printf '%s\n' "$*" >> "$fixture/warnings"; }
    pkg_missing() { printf '%s\n' "$@"; }
    as_root() { printf '%s\n' "$*" >> "$fixture/calls"; return 1; }
    APT_UPDATED=0
    PM=pacman
    export PM
    if pkg_install wireguard-tools; then
        echo 'failed pacman install reported success' >&2; exit 1
    fi
    test "$(cat "$fixture/calls")" = 'pacman -S --needed --noconfirm wireguard-tools'
    grep -q 'pacman -Syu' "$fixture/warnings"
    # Nothing missing: no package-manager call (and no sudo prompt) at all.
    : > "$fixture/calls"
    pkg_missing() { :; }
    pkg_install wireguard-tools
    test ! -s "$fixture/calls"
)

# A failure at any point after the transaction starts restores the previous
# files, authorization and group setup, and removes what this run created.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in inject_failure txn_begin txn_commit group_remove_member group_delete \
        txn_rollback on_exit ensure_dir install_file; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { :; }
    die() { echo "$*" >&2; exit 1; }
    selinux_enabled() { return 1; }
    as_root() {
        case "$1" in
            groupdel|delgroup|gpasswd|deluser|semanage) printf '%s\n' "$*" >> "$fixture/group-calls"; return 0 ;;
        esac
        "$@"
    }
    APP=wireguard-test
    REAL_USER=fixture
    HELPER="$fixture/prefix/lib/app/wg-helper"
    TXN_DIR=""; TXN_ACTIVE=0; TXN_N=0; TXN_PATH=(); TXN_SAVED=()
    CREATED_DIRS=""; GROUP_CREATED=0; MEMBER_ADDED=0; SELINUX_FCONTEXT_ADDED=0
    mkdir -p "$fixture/prefix/bin" "$fixture/etc"
    printf 'old app\n' > "$fixture/prefix/bin/app"
    ln -s app "$fixture/prefix/bin/alias"
    printf 'old sudoers\n' > "$fixture/etc/sudoers"
    targets=("$fixture/prefix/bin/app" "$fixture/prefix/bin/alias" "$HELPER" \
        "$fixture/etc/sudoers" "$fixture/etc/polkit.rules")
    (
        trap on_exit EXIT
        export WG_INSTALL_INJECT_FAILURE=auth
        txn_begin "${targets[@]}"
        printf 'new\n' > "$fixture/new"
        install_file 755 "$fixture/new" "$fixture/prefix/bin/app"
        ln -sfn other "$fixture/prefix/bin/alias"
        ensure_dir "$fixture/prefix/lib/app"
        install_file 755 "$fixture/new" "$HELPER"
        rm "$fixture/etc/sudoers"
        printf 'new rule\n' > "$fixture/etc/polkit.rules"
        GROUP_CREATED=1
        MEMBER_ADDED=1
        inject_failure auth
        echo 'injected failure did not stop the install' >&2
        exit 0
    ) 2>/dev/null && { echo 'rollback scenario unexpectedly succeeded' >&2; exit 1; }
    grep -qx 'old app' "$fixture/prefix/bin/app"
    test "$(readlink "$fixture/prefix/bin/alias")" = app
    grep -qx 'old sudoers' "$fixture/etc/sudoers"
    test ! -e "$fixture/etc/polkit.rules"
    test ! -e "$HELPER"
    test ! -e "$fixture/prefix/lib"
    grep -q '^gpasswd -d fixture wireguard$' "$fixture/group-calls"
    grep -q '^groupdel wireguard$' "$fixture/group-calls"
    if ls /tmp/wireguard-test-install.* >/dev/null 2>&1; then
        echo 'rollback left its backup directory behind' >&2; exit 1
    fi

    # A committed transaction keeps the new state and drops the backup.
    : > "$fixture/group-calls"
    (
        trap on_exit EXIT
        txn_begin "${targets[@]}"
        install_file 644 "$fixture/new" "$fixture/etc/sudoers"
        GROUP_CREATED=1
        txn_commit
    )
    grep -qx new "$fixture/etc/sudoers"
    test ! -s "$fixture/group-calls"
    if ls /tmp/wireguard-test-install.* >/dev/null 2>&1; then
        echo 'commit left its backup directory behind' >&2; exit 1
    fi
)

# SELinux: restore contexts, give the helper a persistent bin_t label, and
# report a denied execution with actionable advice instead of a raw error.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in apply_selinux_labels verify_helper_executes; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    ok() { :; }
    warn() { printf '%s\n' "$*" >> "$fixture/warnings"; }
    die() { echo "$*" >&2; exit 7; }
    SELINUX=1
    selinux_enabled() { test "$SELINUX" = 1; }
    command() {
        case "$*" in
            '-v semanage'|'-v restorecon'|'-v chcon') return 0 ;;
            *) builtin command "$@" ;;
        esac
    }
    as_root() {
        case "$1" in
            semanage|restorecon|chcon) printf '%s\n' "$*" >> "$fixture/selinux-calls"; return 0 ;;
            stat) printf 'system_u:object_r:bin_t:s0\n'; return 0 ;;
        esac
        "$@"
    }
    PREFIX="$fixture/usr.local"
    LIBDIR="$PREFIX/lib/app"
    HELPER="$LIBDIR/wg-helper"
    BIN="$PREFIX/bin/app"; ALIAS="$PREFIX/bin/alias"; MANIFEST="$LIBDIR/install-manifest"
    DESKTOP="$fixture/none"; ICON="$fixture/none"; SUDOERS="$fixture/none"; POLKIT_RULE="$fixture/none"
    SELINUX_FCONTEXT_ADDED=0
    mkdir -p "$LIBDIR" "$PREFIX/bin"
    printf '#!/bin/sh\nexit 2\n' > "$HELPER"
    chmod 755 "$HELPER"
    touch "$BIN"
    apply_selinux_labels
    grep -q "^semanage fcontext -a -t bin_t ${fixture//./\\\\.}/usr\\\\.local/lib/app/wg-helper\$" "$fixture/selinux-calls"
    grep -q "^restorecon -F .*$HELPER" "$fixture/selinux-calls"
    test "$SELINUX_FCONTEXT_ADDED" = 1
    verify_helper_executes

    chmod 644 "$HELPER"
    if out="$(verify_helper_executes 2>&1)"; then
        echo 'non-executable helper accepted' >&2; exit 1
    fi
    case "$out" in *SELinux*semanage*restorecon*) ;; *) echo "unclear denial: $out" >&2; exit 1 ;; esac
    SELINUX=0
    out="$(verify_helper_executes 2>&1)" && { echo 'denied helper accepted' >&2; exit 1; }
    case "$out" in *noexec*) ;; *) echo "unclear denial: $out" >&2; exit 1 ;; esac
    # A missing ELF loader (e.g. glibc build on musl) exits 127 from the shell.
    printf '#!/bin/sh\nexit 127\n' > "$HELPER"
    chmod 755 "$HELPER"
    out="$(verify_helper_executes 2>&1)" && { echo 'unrunnable helper accepted' >&2; exit 1; }
    case "$out" in *architecture*) ;; *) echo "unclear loader error: $out" >&2; exit 1 ;; esac

    # Without SELinux nothing is relabelled.
    : > "$fixture/selinux-calls"
    apply_selinux_labels
    test ! -s "$fixture/selinux-calls"
)

# A package-managed copy blocks a second /usr/local install unless --force.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in pkg_remove_hint check_duplicate_install; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    warn() { printf '%s\n' "$*" >> "$fixture/warnings"; }
    die() { echo "$*" >&2; exit 7; }
    as_root() { "$@"; }
    OWNER=""
    pkg_owner() { [ -n "$OWNER" ] && printf '%s\n' "$OWNER"; }
    PREFIX=/usr/local; APP_BIN=app
    PKG_BIN="$fixture/usr/bin/app"; PKG_HELPER="$fixture/usr/lib/app/wg-helper"
    FORCE=0
    check_duplicate_install
    mkdir -p "$fixture/usr/bin"
    touch "$PKG_BIN"
    OWNER='dpkg wireguard-test'
    if out="$(check_duplicate_install 2>&1)"; then
        echo 'duplicate package install accepted without --force' >&2; exit 1
    fi
    case "$out" in *"dpkg package 'wireguard-test'"*"apt remove wireguard-test"*--force*) ;;
        *) echo "unclear duplicate refusal: $out" >&2; exit 1 ;; esac
    OWNER=""
    (check_duplicate_install) 2>/dev/null && { echo 'unowned duplicate accepted' >&2; exit 1; }
    FORCE=1
    check_duplicate_install
    grep -q -- '--force' "$fixture/warnings"
)

# The manifest written at install time drives later runs: auth mode, desktop
# choice and the account the grant belongs to survive an update.
(
    fixture="$(mktemp -d)"
    trap 'rm -rf "$fixture"' EXIT
    for fn in manifest_get load_previous_install apply_previous_choices install_file write_manifest; do
        eval "$(awk -v name="$fn" '$0 == name "() {" { reading=1 } reading { print } reading && $0 == "}" { exit }' "$script")"
    done
    say() { :; }
    die() { echo "$*" >&2; exit 7; }
    as_root() { "$@"; }
    validate_install_path() { :; }
    id() { [ "$1" = alice ]; }
    APP=wireguard-test; PREFIX="$fixture/prefix"; LIBDIR="$PREFIX/lib/app"
    MANIFEST="$LIBDIR/install-manifest"; HELPER="$LIBDIR/wg-helper"
    DESKTOP="$PREFIX/share/applications/app.desktop"
    SUDOERS="$fixture/sudoers"; POLKIT_RULE="$fixture/polkit.rules"; POLKIT_DIR="$fixture/polkit"
    HERE="$fixture/bundle"; mkdir -p "$HERE" "$LIBDIR" "$(dirname "$DESKTOP")"
    touch "$POLKIT_RULE" "$DESKTOP"
    PREV_INSTALLED=0; PREV_AUTH=""; PREV_AUTH_USER=""; PREV_DESKTOP=""; PREV_FORCE=0
    PREV_GROUP_CREATED=0; PREV_MEMBER_ADDED=0; PREV_SELINUX_FCONTEXT=0; PREV_DIRS=""
    AUTH_RESULT=polkit; REAL_USER=alice; UPDATE_MODE=0; BINARIES_DIR=""; FORCE=0
    GROUP_CREATED=1; MEMBER_ADDED=1; SELINUX_FCONTEXT_ADDED=0
    CREATED_DIRS=" $LIBDIR $POLKIT_DIR"; INSTALL_VERSION=1.2.3
    write_manifest
    grep -qx 'method=installer' "$MANIFEST"
    grep -qx 'auth=polkit' "$MANIFEST"
    grep -qx 'auth_user=alice' "$MANIFEST"
    grep -qx 'version=1.2.3' "$MANIFEST"
    grep -qx "prefix=$PREFIX" "$MANIFEST"
    grep -qx 'desktop=1' "$MANIFEST"
    test -n "$(find "$MANIFEST" -perm 644 -print)"

    # The next run (an update, here invoked by root) keeps polkit + desktop.
    AUTH_MODE=sudoers; AUTH_EXPLICIT=0; INSTALL_DESKTOP=0; DESKTOP_EXPLICIT=0
    UPDATE_MODE=1; REAL_USER=root; FORCE=0
    load_previous_install
    apply_previous_choices
    test "$AUTH_MODE" = polkit
    test "$INSTALL_DESKTOP" = 1
    test "$REAL_USER" = alice
    test "$PREV_GROUP_CREATED" = 1
    case "$PREV_DIRS" in *"$LIBDIR"*"$POLKIT_DIR"*) ;; *) echo "created dirs lost: $PREV_DIRS" >&2; exit 1 ;; esac
    # An explicit flag still switches modes.
    AUTH_MODE=sudoers; AUTH_EXPLICIT=1
    apply_previous_choices
    test "$AUTH_MODE" = sudoers

    # Installs made before the manifest existed: infer from the files on disk.
    rm "$MANIFEST"
    touch "$HELPER"
    PREV_INSTALLED=0; PREV_AUTH=""; AUTH_EXPLICIT=0; AUTH_MODE=sudoers
    load_previous_install
    test "$PREV_AUTH" = polkit
    rm "$POLKIT_RULE"
    printf 'bob ALL=(root) NOPASSWD: %s\n' "$HELPER" > "$SUDOERS"
    PREV_INSTALLED=0; PREV_AUTH=""; PREV_AUTH_USER=""
    load_previous_install
    test "$PREV_AUTH" = sudoers
    test "$PREV_AUTH_USER" = bob
    rm "$HELPER" "$SUDOERS"
    PREV_INSTALLED=0
    load_previous_install
    UPDATE_MODE=1
    (apply_previous_choices) 2>/dev/null && { echo '--update accepted without an existing install' >&2; exit 1; }
    true
)

echo "installer sanity tests passed"
