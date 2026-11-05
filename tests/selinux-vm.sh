#!/usr/bin/env bash
#
# Real SELinux test for install.sh. Boots a Fedora Cloud Base VM (SELinux
# enforcing), installs locally built binaries with ./install.sh as a normal
# user, and checks the helper label (bin_t), the semanage fcontext rule, AVC
# denials, restorecon, the sudo/polkit grant, re-runs and uninstall, plus two
# exec-denied failure paths (a noexec mount and an SELinux denial) and their
# rollback.
#
#   tests/selinux-vm.sh --binaries=DIR [--arch=x86_64|aarch64] [--keep]
#
# DIR holds the app binary and wg-helper built for the guest architecture
# (default: the host's). --keep leaves the VM running on exit and prints the
# ssh command for debugging.
#
# Host tools: qemu-system-<arch>, qemu-img, ssh, ssh-keygen, curl, sha256sum,
# tar, and one of cloud-localds/genisoimage/mkisofs/xorriso; aarch64 also
# needs UEFI firmware (qemu-efi-aarch64 or edk2-aarch64). KVM is used when
# /dev/kvm is usable and the guest matches the host CPU, TCG otherwise (TCG
# is slow: allow an hour). The guest needs network access for dnf.
#
# Environment:
#   SELINUX_VM_DIR           image cache and work directory
#                            (default ${XDG_CACHE_HOME:-~/.cache}/wg-selinux-vm)
#   SELINUX_VM_CPUS          vCPUs (default: up to 4)
#   SELINUX_VM_MEM           memory in MiB (default 3072)
#   SELINUX_VM_SSH_PORT      host port forwarded to the guest's sshd (default 2222)
#   SELINUX_VM_FIRMWARE      aarch64 UEFI firmware image
#   SELINUX_VM_BOOT_TIMEOUT  seconds to wait for ssh (default 300 KVM, 1800 TCG)
#
# The same file runs inside the guest as `selinux-vm.sh --guest STEP ...`.
set -euo pipefail

# Fedora Cloud Base Generic images, pinned. When this release reaches end of
# life its images move to archives.fedoraproject.org; update the pins then.
FEDORA_RELEASE=45
FEDORA_COMPOSE=1.4
FEDORA_SHA256_x86_64=50bdc5e70f6a24f540932d15411c1c6ae9fcfa40fb990d876d83c5cc823b2ad0
FEDORA_SHA256_aarch64=1632acc8bc14d45b6c9596a0acc71b070d3748d1abdcf1a519be5c983bb0286c

GUEST_USER=tester
GUEST_ADMIN="admin"
GUEST_HOME="/home/$GUEST_USER"
GUEST_SRC="$GUEST_HOME/src"
GUEST_BINS="$GUEST_HOME/bins"
SESSION_DIR="$GUEST_HOME/.wg-session"
BROAD_GRANT=/etc/sudoers.d/zz-selinux-vm-tester
STATE_DIR=/var/tmp/wg-selinux-vm
NOEXEC_PREFIX=/opt/wg-noexec
DENIED_PREFIX=/opt/wg-denied
DENIED_TYPE=wg_selinux_vm_noexec_t

die() { printf 'selinux-vm: %s\n' "$*" >&2; exit 1; }

# Read APP/APP_BIN from install.sh so the test follows the installer.
read_app_names() {
    local installer="$1"
    APP="$(sed -n 's/^APP="\([a-z0-9-]*\)"$/\1/p' "$installer")"
    APP_BIN="$(sed -n 's/^APP_BIN="\([a-z0-9-]*\)"$/\1/p' "$installer")"
    [ -n "$APP" ] && [ -n "$APP_BIN" ] || die "cannot read APP/APP_BIN from $installer"
}

# ===========================================================================
# Guest side: one STEP per invocation, called over ssh by the host side.
# ===========================================================================
gfail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
gpass() { printf 'ok: %s\n' "$*"; }

guest_paths() {
    local prefix="$1"
    LIBDIR="$prefix/lib/$APP"
    HELPER="$LIBDIR/wg-helper"
    HELPER_RE="${HELPER//./\\.}"
    BIN="$prefix/bin/$APP_BIN"
    ALIAS="$prefix/bin/$APP"
    MANIFEST="$LIBDIR/install-manifest"
    DESKTOP="$prefix/share/applications/$APP.desktop"
    ICON="$prefix/share/icons/hicolor/scalable/apps/$APP.svg"
    SUDOERS="/etc/sudoers.d/$APP"
    POLKIT_RULE="/etc/polkit-1/rules.d/49-$APP.rules"
}

# AVC/SELinux records since boot that mention the helper. --input-logs:
# without a terminal ausearch would read stdin instead of the audit log.
helper_avcs() {
    ausearch --input-logs -m AVC,USER_AVC,SELINUX_ERR -ts boot -i 2>/dev/null </dev/null \
        | grep -F 'wg-helper' || true
}

fcontext_rule() {
    semanage fcontext -l | awk -v p="$1" '$1 == p'
}

# root: install the SELinux tools and polkit, set up the accounts. The host
# has already unpacked the payload (this script is part of it).
guest_prepare() {
    local mode need=()
    mode="$(getenforce)"
    [ "$mode" = Enforcing ] || gfail "getenforce is '$mode', expected Enforcing"
    gpass "getenforce: Enforcing ($(sestatus | sed -n 's/^Loaded policy name:[[:space:]]*//p') policy)"
    chown -R "$GUEST_USER:$GUEST_USER" "$GUEST_SRC" "$GUEST_BINS"
    restorecon -R "$GUEST_HOME"
    command -v semanage >/dev/null 2>&1 || need+=(policycoreutils-python-utils)
    command -v ausearch >/dev/null 2>&1 || need+=(audit)
    command -v pkexec >/dev/null 2>&1 || need+=(polkit)
    if [ "${#need[@]}" -gt 0 ]; then
        dnf -y -q install "${need[@]}" >/dev/null
    fi
    systemctl is-active -q auditd || systemctl start auditd
    systemctl is-active -q auditd || gfail "auditd is not running; AVC checks need it"
    install -d -m 700 "$STATE_DIR"
    guest_grant
    # An autologin on tty1 gives the user an active, local seat0 session:
    # the only kind the polkit rule authorizes. A small agent in that session
    # runs the commands the host asks for (pkexec checks).
    install -d -m 700 -o "$GUEST_USER" -g "$GUEST_USER" "$SESSION_DIR"
    cat >> "$GUEST_HOME/.bash_profile" <<EOF
if [ "\$(tty)" = /dev/tty1 ]; then
    exec bash $GUEST_SRC/tests/selinux-vm.sh --guest session-agent
fi
EOF
    install -d /etc/systemd/system/getty@tty1.service.d
    printf '[Service]\nExecStart=\nExecStart=-/sbin/agetty --autologin %s --noclear %%I $TERM\n' \
        "$GUEST_USER" > /etc/systemd/system/getty@tty1.service.d/autologin.conf
    systemctl daemon-reload
    systemctl restart getty@tty1.service
    for _ in $(seq 120); do
        [ -s "$SESSION_DIR/info" ] && break
        sleep 1
    done
    [ -s "$SESSION_DIR/info" ] || gfail "the tty1 autologin session did not start"
    grep -qx 'Seat=seat0' "$SESSION_DIR/info" && grep -qx 'Active=yes' "$SESSION_DIR/info" \
        && grep -qx 'Remote=no' "$SESSION_DIR/info" \
        || gfail "tty1 session is not an active local seat0 session: $(tr '\n' ' ' < "$SESSION_DIR/info")"
    gpass "local session on tty1: $(tr '\n' ' ' < "$SESSION_DIR/info")"
}

# user (tty1 session): run requested commands, report their output and status.
guest_session_agent() {
    local rc
    {
        id -Z
        loginctl show-session "$XDG_SESSION_ID" -p Seat -p Active -p Remote
    } > "$SESSION_DIR/info.tmp" 2>&1
    mv "$SESSION_DIR/info.tmp" "$SESSION_DIR/info"
    while :; do
        if [ -f "$SESSION_DIR/request" ]; then
            mv "$SESSION_DIR/request" "$SESSION_DIR/run"
            rc=0
            bash "$SESSION_DIR/run" > "$SESSION_DIR/out" 2>&1 < /dev/null || rc=$?
            printf '%s\n' "$rc" > "$SESSION_DIR/rc.tmp"
            mv "$SESSION_DIR/rc.tmp" "$SESSION_DIR/rc"
        fi
        sleep 1
    done
}

# root: give/withdraw the test user's blanket sudo (install.sh needs it; the
# helper checks must prove the installer's own grant without it).
guest_grant() {
    printf '%s ALL=(ALL) NOPASSWD: ALL\n' "$GUEST_USER" > "$BROAD_GRANT"
    chmod 440 "$BROAD_GRANT"
    visudo -cqf "$BROAD_GRANT" || gfail "invalid test sudoers grant"
}

guest_revoke() {
    rm -f "$BROAD_GRANT"
}

# user: run the installer from the checkout.
guest_install() {
    cd "$GUEST_SRC"
    ./install.sh --binaries="$GUEST_BINS" "$@"
}

guest_uninstall() {
    cd "$GUEST_SRC"
    ./install.sh uninstall
}

# user: an install into PREFIX must fail on the exec check and roll back.
guest_install_fail() {
    local prefix="$1" kind="$2" out rc=0 expected
    guest_paths "$prefix"
    cd "$GUEST_SRC"
    out="$(PREFIX="$prefix" ./install.sh --binaries="$GUEST_BINS" 2>&1)" || rc=$?
    printf '%s\n' "$out"
    [ "$rc" -ne 0 ] || gfail "install into $prefix succeeded although exec is denied there"
    case "$kind" in
        noexec) expected="Executing $HELPER was denied: $prefix is mounted noexec." ;;
        selinux) expected="Executing $HELPER was denied (SELinux)." ;;
        *) gfail "unknown failure kind $kind" ;;
    esac
    grep -qF "$expected" <<< "$out" || gfail "missing error: $expected"
    grep -qF "Previous state restored; nothing was changed." <<< "$out" \
        || gfail "the installer did not report a complete rollback"
    gpass "install into $prefix failed with: $expected"
}

# user, without the blanket grant: only the installer's drop-in may work.
guest_sudo_check() {
    local rc=0
    guest_paths "$1"
    if sudo -n true 2>/dev/null; then
        gfail "the test user still has unrelated sudo rights; the check would prove nothing"
    fi
    sudo -n "$HELPER" list || gfail "sudo -n $HELPER list failed for $(id -un)"
    sudo -n "$HELPER" </dev/null >/dev/null 2>&1 || rc=$?
    [ "$rc" = 2 ] || gfail "sudo -n $HELPER without a verb exited $rc, expected its usage error (2)"
    gpass "sudo -n $HELPER list works for $(id -un) through the installer's drop-in only"
}

# root: the assertions for an installed copy.
guest_assert_installed() {
    local mode="$1" ctx rule relabel avcs path rc=0
    local -a paths=()
    guest_paths "$2"
    [ -x "$HELPER" ] && [ -x "$BIN" ] || gfail "$BIN or $HELPER is missing"
    ctx="$(ls -Z "$HELPER" | awk '{print $1}')"
    [[ "$ctx" == *:object_r:bin_t:* ]] || gfail "helper label is '$ctx', expected bin_t"
    gpass "ls -Z $HELPER: $ctx"
    rule="$(fcontext_rule "$HELPER_RE")"
    [[ "$rule" == *system_u:object_r:bin_t:s0* ]] || gfail "semanage fcontext -l has no bin_t rule for $HELPER_RE: '$rule'"
    gpass "semanage fcontext -l: $(tr -s ' ' <<< "$rule")"
    [ "$(sed -n 's/^selinux_fcontext=//p' "$MANIFEST")" = 1 ] || gfail "manifest does not record the fcontext rule"
    for path in "$BIN" "$ALIAS" "$LIBDIR" "$DESKTOP" "$ICON" "$SUDOERS" "$POLKIT_RULE"; do
        if [ -e "$path" ] || [ -L "$path" ]; then paths+=("$path"); fi
    done
    relabel="$(restorecon -n -v -R "${paths[@]}" 2>&1)"
    [ -z "$relabel" ] || gfail "restorecon -n -v would change labels: $relabel"
    gpass "restorecon -n -v: nothing to change (${#paths[@]} paths)"
    avcs="$(helper_avcs)"
    [ -z "$avcs" ] || gfail "AVC denials for the helper: $avcs"
    gpass "ausearch -m avc: no denials for wg-helper"
    "$HELPER" </dev/null >/dev/null 2>&1 || rc=$?
    [ "$rc" = 2 ] || gfail "$HELPER exited $rc as root, expected its usage error (2)"
    case "$mode" in
        sudoers)
            [ "$(cat "$SUDOERS")" = "$GUEST_USER ALL=(root) NOPASSWD: $HELPER" ] \
                || gfail "unexpected sudoers drop-in: $(cat "$SUDOERS" 2>&1)"
            [ ! -e "$POLKIT_RULE" ] || gfail "polkit rule present in sudoers mode"
            grep -qx 'auth=sudoers' "$MANIFEST" || gfail "manifest auth is not sudoers" ;;
        polkit)
            [ -f "$POLKIT_RULE" ] || gfail "polkit rule missing"
            [ ! -e "$SUDOERS" ] || gfail "sudoers drop-in left behind in polkit mode"
            id -nG "$GUEST_USER" | grep -qw wireguard || gfail "$GUEST_USER is not in the wireguard group"
            grep -qx 'auth=polkit' "$MANIFEST" || gfail "manifest auth is not polkit" ;;
    esac
    gpass "$mode authorization in place"
}

# root: in polkit mode the user's local session may pkexec the helper.
guest_pkexec_check() {
    local rc
    guest_paths "$1"
    rm -f "$SESSION_DIR/rc" "$SESSION_DIR/out"
    printf 'set -e\npkexec %q list\n! pkexec %q read ../x\n' "$HELPER" "$HELPER" > "$SESSION_DIR/request.tmp"
    chown "$GUEST_USER:" "$SESSION_DIR/request.tmp"
    mv "$SESSION_DIR/request.tmp" "$SESSION_DIR/request"
    for _ in $(seq 180); do
        [ -f "$SESSION_DIR/rc" ] && break
        sleep 1
    done
    [ -f "$SESSION_DIR/rc" ] || gfail "the session agent did not answer"
    rc="$(cat "$SESSION_DIR/rc")"
    cat "$SESSION_DIR/out"
    [ "$rc" = 0 ] || gfail "pkexec $HELPER list failed in the local session (exit $rc)"
    gpass "pkexec $HELPER list works in $GUEST_USER's local session"
}

guest_assert_uninstalled() {
    local path
    guest_paths "$1"
    for path in "$BIN" "$ALIAS" "$HELPER" "$MANIFEST" "$LIBDIR" "$DESKTOP" "$ICON" "$SUDOERS" "$POLKIT_RULE"; do
        [ ! -e "$path" ] && [ ! -L "$path" ] || gfail "$path is still present"
    done
    [ -z "$(fcontext_rule "$HELPER_RE")" ] || gfail "the fcontext rule for $HELPER_RE is still present"
    if getent group wireguard >/dev/null; then gfail "the wireguard group created by --polkit is still present"; fi
    gpass "files, fcontext rule and group removed"
}

sudoers_digest() {
    if [ -e "/etc/sudoers.d/$APP" ]; then sha256sum "/etc/sudoers.d/$APP" | cut -d' ' -f1; else echo absent; fi
}

guest_save_state() {
    sudoers_digest > "$STATE_DIR/sudoers"
    semanage fcontext -l -C > "$STATE_DIR/fcontext"
}

# root: after a failed install into PREFIX nothing may be left behind.
guest_assert_rolled_back() {
    local path
    guest_paths "$1"
    for path in "$BIN" "$ALIAS" "$HELPER" "$MANIFEST" "$LIBDIR" "$1/bin" "$1/lib" "$1/share"; do
        [ ! -e "$path" ] && [ ! -L "$path" ] || gfail "rollback left $path behind"
    done
    [ -z "$(fcontext_rule "$HELPER_RE")" ] || gfail "rollback left the fcontext rule for $HELPER_RE"
    [ "$(sudoers_digest)" = "$(cat "$STATE_DIR/sudoers")" ] || gfail "rollback did not restore /etc/sudoers.d/$APP"
    semanage fcontext -l -C | diff -u "$STATE_DIR/fcontext" - || gfail "rollback changed the local fcontext rules"
    gpass "rollback restored the previous state"
}

# root: a noexec mount to install into.
guest_noexec_setup() {
    guest_save_state
    install -d -m 755 "$NOEXEC_PREFIX"
    mount -t tmpfs -o noexec,nosuid,nodev,mode=0755 wg-noexec "$NOEXEC_PREFIX"
}

# root: a mount whose files all carry a fixed label (context=, like NFS or
# vfat) that the user's domain may write but not execute. The installer can
# neither relabel it nor run the helper from it: SELinux denies the exec.
guest_denied_setup() {
    local cil="$STATE_DIR/$DENIED_TYPE.cil" ctx
    guest_save_state
    ctx="$(id -Z | cut -d: -f3)"
    cat > "$cil" <<EOF
(type $DENIED_TYPE)
(roletype object_r $DENIED_TYPE)
(typeattributeset cil_gen_require $ctx)
(allow $DENIED_TYPE fs_t (filesystem (associate)))
(allow $DENIED_TYPE self (filesystem (associate)))
(allow $ctx $DENIED_TYPE (filesystem (mount unmount getattr relabelfrom relabelto)))
(allow $ctx $DENIED_TYPE (dir (create getattr setattr open read write search add_name remove_name rmdir ioctl lock)))
(allow $ctx $DENIED_TYPE (file (create getattr setattr open read write append rename unlink link ioctl lock)))
(allow $ctx $DENIED_TYPE (lnk_file (create getattr read unlink rename)))
(allow $ctx tmpfs_t (filesystem (relabelfrom)))
EOF
    semodule -i "$cil"
    install -d -m 755 "$DENIED_PREFIX"
    mount -t tmpfs -o "mode=0755,context=system_u:object_r:$DENIED_TYPE:s0" wg-denied "$DENIED_PREFIX"
    ls -Zd "$DENIED_PREFIX"
}

guest_expect_avc() {
    local avcs
    guest_paths "$1"
    avcs="$(helper_avcs | grep -F "$DENIED_TYPE" || true)"
    [ -n "$avcs" ] || gfail "no AVC denial recorded for the helper under $1"
    grep -q 'execute' <<< "$avcs" || gfail "the AVC denial is not an execute denial: $avcs"
    gpass "ausearch -m avc shows the execute denial: $(head -n 1 <<< "$avcs")"
}

guest_teardown() {
    umount "$NOEXEC_PREFIX" 2>/dev/null || true
    umount "$DENIED_PREFIX" 2>/dev/null || true
    semodule -r "${DENIED_TYPE%_t}" 2>/dev/null || true
}

guest_main() {
    local step="$1"
    shift
    read_app_names "$GUEST_SRC/install.sh"
    case "$step" in
        prepare|session-agent|grant|revoke|install|uninstall|install-fail|sudo-check|assert-installed|\
        pkexec-check|assert-uninstalled|assert-rolled-back|noexec-setup|denied-setup|expect-avc|teardown)
            "guest_${step//-/_}" "$@" ;;
        *) die "unknown guest step: $step" ;;
    esac
}

if [ "${1:-}" = --guest ]; then
    shift
    guest_main "$@"
    exit 0
fi

# ===========================================================================
# Host side
# ===========================================================================
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
ARCH="$(uname -m)"; BINARIES=""; KEEP=0
for arg in "$@"; do
    case "$arg" in
        --arch=x86_64|--arch=aarch64) ARCH="${arg#--arch=}" ;;
        --binaries=?*) BINARIES="${arg#--binaries=}" ;;
        --keep) KEEP=1 ;;
        -h|--help) sed -n '3,30p' "$0"; exit 0 ;;
        *) die "unknown option: $arg (see --help)" ;;
    esac
done
case "$ARCH" in arm64) ARCH=aarch64 ;; amd64) ARCH=x86_64 ;; esac
[ "$ARCH" = x86_64 ] || [ "$ARCH" = aarch64 ] || die "unsupported architecture $ARCH"
read_app_names "$ROOT/install.sh"
[ -n "$BINARIES" ] || die "--binaries=DIR is required (DIR holds $APP_BIN and wg-helper)"
[ -x "$BINARIES/$APP_BIN" ] && [ -x "$BINARIES/wg-helper" ] || die "$BINARIES must contain executable $APP_BIN and wg-helper"

WORK="${SELINUX_VM_DIR:-${XDG_CACHE_HOME:-$HOME/.cache}/wg-selinux-vm}"
RUN="$WORK/run-$APP-$ARCH"
SSH_PORT="${SELINUX_VM_SSH_PORT:-2222}"
MEM="${SELINUX_VM_MEM:-3072}"
CPUS="${SELINUX_VM_CPUS:-$(n="$(nproc)"; [ "$n" -gt 4 ] && n=4; echo "$n")}"
IMAGE="Fedora-Cloud-Base-Generic-$FEDORA_RELEASE-$FEDORA_COMPOSE.$ARCH.qcow2"
IMAGE_URL="https://download.fedoraproject.org/pub/fedora/linux/releases/$FEDORA_RELEASE/Cloud/$ARCH/images/$IMAGE"
case "$ARCH" in
    x86_64) IMAGE_SHA256="$FEDORA_SHA256_x86_64" ;;
    aarch64) IMAGE_SHA256="$FEDORA_SHA256_aarch64" ;;
esac

for tool in "qemu-system-$ARCH" qemu-img ssh ssh-keygen curl sha256sum tar; do
    command -v "$tool" >/dev/null 2>&1 || die "missing host tool: $tool"
done

ACCEL=tcg
host_arch="$(uname -m)"; [ "$host_arch" = arm64 ] && host_arch=aarch64
if [ "$host_arch" = "$ARCH" ] && [ -r /dev/kvm ] && [ -w /dev/kvm ]; then ACCEL=kvm; fi
BOOT_TIMEOUT="${SELINUX_VM_BOOT_TIMEOUT:-$([ "$ACCEL" = kvm ] && echo 300 || echo 1800)}"

T0=$SECONDS; TIMINGS=()
log() { printf '\n[selinux-vm %4ds] %s\n' "$((SECONDS - T0))" "$*"; }
phase_start=0; phase_name=""
phase() {
    [ -z "$phase_name" ] || TIMINGS+=("$(printf '%5ds  %s' "$((SECONDS - phase_start))" "$phase_name")")
    phase_name="$1"; phase_start=$SECONDS
    [ -z "$1" ] || log "== $1"
}

SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR
    -o BatchMode=yes -o ConnectTimeout=20 -o ServerAliveInterval=15 -o ServerAliveCountMax=8
    -i "$RUN/id_ed25519" -p "$SSH_PORT")
vm_ssh() { local user="$1"; shift; ssh "${SSH_OPTS[@]}" "$user@127.0.0.1" "$@"; }
# A guest step as the test user, or as root through the admin account.
guser() { vm_ssh "$GUEST_USER" "bash $GUEST_SRC/tests/selinux-vm.sh --guest $(printf '%q ' "$@")"; }
groot() { vm_ssh "$GUEST_ADMIN" "sudo bash $GUEST_SRC/tests/selinux-vm.sh --guest $(printf '%q ' "$@")"; }

QEMU_PID=""
cleanup() {
    local rc=$?
    if [ "$rc" -ne 0 ] && [ -f "$RUN/serial.log" ]; then
        printf '\n--- last serial console lines ---\n' >&2
        tail -n 40 "$RUN/serial.log" >&2 || true
    fi
    if [ -n "$QEMU_PID" ] && qemu_running; then
        if [ "$KEEP" = 1 ]; then
            printf '\nVM kept running (pid %s). Connect with:\n  ssh %s %s@127.0.0.1\n' \
                "$QEMU_PID" "${SSH_OPTS[*]}" "$GUEST_ADMIN" >&2
        else
            kill "$QEMU_PID" 2>/dev/null || true
        fi
    fi
    exit "$rc"
}
trap cleanup EXIT

fetch_image() {
    mkdir -p "$WORK"
    if [ -f "$WORK/$IMAGE" ] && [ "$(sha256sum "$WORK/$IMAGE" | cut -d' ' -f1)" = "$IMAGE_SHA256" ]; then
        log "using cached $IMAGE"
        return 0
    fi
    log "downloading $IMAGE_URL"
    curl -fL --proto '=https' --tlsv1.2 --retry 3 -o "$WORK/$IMAGE.part" "$IMAGE_URL"
    [ "$(sha256sum "$WORK/$IMAGE.part" | cut -d' ' -f1)" = "$IMAGE_SHA256" ] \
        || die "SHA256 mismatch for $IMAGE"
    mv "$WORK/$IMAGE.part" "$WORK/$IMAGE"
}

make_seed() {
    local key
    key="$(cat "$RUN/id_ed25519.pub")"
    cat > "$RUN/seed/user-data" <<EOF
#cloud-config
ssh_pwauth: false
users:
  - name: $GUEST_ADMIN
    sudo: "ALL=(ALL) NOPASSWD:ALL"
    lock_passwd: true
    ssh_authorized_keys: ["$key"]
  - name: $GUEST_USER
    lock_passwd: true
    ssh_authorized_keys: ["$key"]
bootcmd:
  - [systemctl, mask, --now, dnf-makecache.timer]
EOF
    printf 'instance-id: wg-selinux-vm-%s\nlocal-hostname: wg-selinux-vm\n' "$$" > "$RUN/seed/meta-data"
    if command -v cloud-localds >/dev/null 2>&1; then
        cloud-localds "$RUN/seed.iso" "$RUN/seed/user-data" "$RUN/seed/meta-data"
    else
        local mkiso=""
        for mkiso in genisoimage mkisofs "xorriso -as mkisofs"; do
            command -v "${mkiso%% *}" >/dev/null 2>&1 && break
            mkiso=""
        done
        [ -n "$mkiso" ] || die "need cloud-localds, genisoimage, mkisofs or xorriso for the cloud-init seed"
        # shellcheck disable=SC2086 # "xorriso -as mkisofs" is two words.
        $mkiso -quiet -output "$RUN/seed.iso" -volid cidata -joliet -rock \
            "$RUN/seed/user-data" "$RUN/seed/meta-data"
    fi
}

# The checkout's files (tracked and untracked, never ignored ones such as
# build output or local notes), NUL-separated.
checkout_files() {
    local f
    if git -c safe.directory='*' -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git -c safe.directory='*' -C "$ROOT" ls-files -z -c -o --exclude-standard |
            while IFS= read -r -d '' f; do
                if [ -e "$ROOT/$f" ] || [ -L "$ROOT/$f" ]; then printf '%s\0' "$f"; fi
            done
    else
        (cd "$ROOT" && find . \( -path ./target -o -path ./.git -o -path ./fuzz/target \) -prune \
            -o \( -type f -o -type l \) -print0)
    fi
}

# The checkout and the binaries, as a tar archive the guest reads straight
# from an extra virtio disk.
make_payload() {
    local stage="$RUN/payload"
    rm -rf "$stage"
    mkdir -p "$stage/src" "$stage/bins"
    checkout_files | (cd "$ROOT" && tar --null -T - -cf -) | tar -C "$stage/src" -xf -
    install -m 755 "$BINARIES/$APP_BIN" "$BINARIES/wg-helper" "$stage/bins/"
    tar -C "$stage" --owner=0 --group=0 --numeric-owner -cf "$RUN/payload.tar" src bins
    rm -rf "$stage"
}

firmware() {
    local f
    for f in "${SELINUX_VM_FIRMWARE:-}" /usr/share/qemu-efi-aarch64/QEMU_EFI.fd \
            /usr/share/edk2/aarch64/QEMU_EFI.fd /usr/share/qemu/edk2-aarch64-code.fd \
            /usr/share/AAVMF/AAVMF_CODE.fd; do
        [ -n "$f" ] && [ -f "$f" ] && { printf '%s' "$f"; return 0; }
    done
    die "no aarch64 UEFI firmware found (install qemu-efi-aarch64 or edk2-aarch64, or set SELINUX_VM_FIRMWARE)"
}

boot_vm() {
    local -a qemu=("qemu-system-$ARCH")
    rm -f "$RUN/disk.qcow2" "$RUN/serial.log"
    qemu-img create -q -f qcow2 -b "$WORK/$IMAGE" -F qcow2 "$RUN/disk.qcow2" 20G
    if [ "$ARCH" = x86_64 ]; then
        qemu+=(-machine q35)
        if [ "$ACCEL" = kvm ]; then qemu+=(-cpu host); else qemu+=(-cpu max); fi
    else
        qemu+=(-machine "virt,gic-version=max" -bios "$(firmware)")
        if [ "$ACCEL" = kvm ]; then qemu+=(-cpu host); else qemu+=(-cpu "max,pauth-impdef=on"); fi
    fi
    if [ "$ACCEL" = kvm ]; then qemu+=(-accel kvm); else qemu+=(-accel "tcg,thread=multi"); fi
    qemu+=(-smp "$CPUS" -m "$MEM" -display none -monitor none -serial "file:$RUN/serial.log"
        # bootindex: SeaBIOS (x86_64) boots only the first disk it finds,
        # which may otherwise be the payload.
        -drive "if=none,id=root,file=$RUN/disk.qcow2,format=qcow2"
        -device "virtio-blk-pci,drive=root,bootindex=0"
        -drive "if=virtio,file=$RUN/seed.iso,format=raw,readonly=on"
        -drive "if=none,id=payload,file=$RUN/payload.tar,format=raw,readonly=on"
        -device "virtio-blk-pci,drive=payload,serial=wgpayload"
        -netdev "user,id=net0,hostfwd=tcp:127.0.0.1:$SSH_PORT-:22"
        -device "virtio-net-pci,netdev=net0"
        -object "rng-random,id=rng0,filename=/dev/urandom" -device "virtio-rng-pci,rng=rng0"
        -daemonize -pidfile "$RUN/qemu.pid")
    log "starting $ARCH guest with $ACCEL ($CPUS vCPUs, $MEM MiB)"
    "${qemu[@]}"
    QEMU_PID="$(cat "$RUN/qemu.pid")"
}

# Alive and not a zombie (a container's init may never reap the daemon).
qemu_running() {
    [ -r "/proc/$QEMU_PID/stat" ] && ! grep -q '^[0-9]* (.*) Z ' "/proc/$QEMU_PID/stat"
}

wait_for_guest() {
    local deadline=$((SECONDS + BOOT_TIMEOUT))
    until vm_ssh "$GUEST_ADMIN" true 2>/dev/null; do
        qemu_running || die "qemu exited during boot"
        [ "$SECONDS" -lt "$deadline" ] || die "no ssh after ${BOOT_TIMEOUT}s"
        sleep 10
    done
    vm_ssh "$GUEST_ADMIN" "cloud-init status --wait >/dev/null; cloud-init status; uname -r"
}

main() {
    rm -rf "$RUN"
    mkdir -p "$RUN/seed"
    ssh-keygen -q -t ed25519 -N '' -f "$RUN/id_ed25519"
    phase "download $IMAGE"
    fetch_image
    phase "boot ($ARCH, $ACCEL)"
    make_seed
    make_payload
    boot_vm
    wait_for_guest
    phase "prepare guest"
    vm_ssh "$GUEST_ADMIN" "sudo tar -xf /dev/disk/by-id/virtio-wgpayload -C $GUEST_HOME"
    groot prepare

    phase "install (sudoers)"
    guser install
    groot revoke
    guser sudo-check /usr/local
    groot grant
    groot assert-installed sudoers /usr/local

    phase "re-run (sudoers) keeps the label"
    guser install
    groot assert-installed sudoers /usr/local

    phase "failure: noexec mount"
    groot noexec-setup
    guser install-fail "$NOEXEC_PREFIX" noexec
    groot assert-rolled-back "$NOEXEC_PREFIX"
    groot assert-installed sudoers /usr/local

    phase "install --polkit"
    guser install --polkit
    groot assert-installed polkit /usr/local
    groot pkexec-check /usr/local

    phase "re-run (polkit) keeps the label"
    guser install
    groot assert-installed polkit /usr/local

    phase "uninstall"
    guser uninstall
    groot assert-uninstalled /usr/local

    phase "failure: SELinux exec denial"
    groot denied-setup
    guser install-fail "$DENIED_PREFIX" selinux
    groot assert-rolled-back "$DENIED_PREFIX"
    groot expect-avc "$DENIED_PREFIX"
    groot teardown
    phase ""

    printf '\nSELinux VM test passed for %s on Fedora %s %s (%s) in %ds:\n' \
        "$APP" "$FEDORA_RELEASE" "$ARCH" "$ACCEL" "$((SECONDS - T0))"
    printf '%s\n' "${TIMINGS[@]}"
}

main
