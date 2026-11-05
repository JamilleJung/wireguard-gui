#!/usr/bin/env bash
#
# Build the real distro package from the CURRENT checkout with the
# distribution's own tool, run the package's tests, then install the result
# in a clean container and smoke-test it:
#
#   tests/package-build.sh arch|rpm|apk|void [--out=DIR]
#
#   arch  makepkg -s --noconfirm + namcap      archlinux:latest (linux/amd64)
#   rpm   rpmbuild -ba with the vendored Source1 fedora:latest
#   apk   abuild -r with a throwaway key       alpine:latest
#   void  xbps-src -Q pkg (binary-bootstrap)   ghcr.io/void-linux/void-glibc:latest
#
# The source archive is made from the checkout (git archive of HEAD when the
# tree is clean, otherwise the tracked and untracked, non-ignored files) and
# is named and laid out like the release archive (NAME-VERSION/). Temporary
# copies of the recipes point at it with matching checksums
# (packaging/update-checksums.sh --tarball); the committed recipes keep their
# real URLs. The built packages land in DIR (default target/package-build/FORMAT).
#
# The clean-container check installs the package with its package manager
# (pulling the declared dependencies), runs `APP --version` and
# tests/helper-validation.sh against the installed helper.
#
# Needs docker and network access. CARGO_BUILD_JOBS limits build parallelism
# (useful when a build runs out of memory); PACKAGE_BUILD_KEEP=1 keeps the
# work directory.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
FORMAT=""; OUT=""
for arg in "$@"; do
    case "$arg" in
        arch|rpm|apk|void) FORMAT="$arg" ;;
        --out=?*) OUT="${arg#--out=}" ;;
        -h|--help) sed -n '3,28p' "$0"; exit 0 ;;
        *) printf 'usage: %s arch|rpm|apk|void [--out=DIR]\n' "$0" >&2; exit 2 ;;
    esac
done
[ -n "$FORMAT" ] || { printf 'usage: %s arch|rpm|apk|void [--out=DIR]\n' "$0" >&2; exit 2; }

die() { printf 'package-build: %s\n' "$*" >&2; exit 1; }
log() { printf '\n[package-build %s %4ds] %s\n' "$FORMAT" "$SECONDS" "$*"; }

cargo_field() {
    awk -v key="$1" '
        /^\[/ { in_package = ($0 == "[package]") }
        in_package && $1 == key && $2 == "=" { gsub(/"/, "", $3); print $3; exit }
    ' "$ROOT/Cargo.toml"
}
NAME="$(cargo_field name)"
VERSION="$(cargo_field version)"
APP_BIN="$(sed -n 's/^APP_BIN="\([a-z0-9-]*\)"$/\1/p' "$ROOT/install.sh")"
[[ "$NAME" =~ ^[a-z0-9-]+$ ]] && [[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] && [ -n "$APP_BIN" ] \
    || die "cannot read the package name, version and binary name"
ARCHIVE="$NAME-$VERSION.tar.gz"

command -v docker >/dev/null 2>&1 || die "docker is required"
if [ -z "$OUT" ]; then
    OUT="$ROOT/target/package-build/$FORMAT"
    rm -rf "$OUT"
fi
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"
[ -z "$(ls -A "$OUT")" ] || die "output directory $OUT is not empty"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/package-build.XXXXXX")"
cleanup() {
    if [ "${PACKAGE_BUILD_KEEP:-0}" = 1 ]; then
        printf 'work directory kept: %s\n' "$WORK" >&2
    else
        # Containers may leave root-owned files; clear them from a container.
        rm -rf "$WORK" 2>/dev/null \
            || docker run --rm -v "$WORK:/work" alpine:latest rm -rf /work/tree /work/src >/dev/null 2>&1 || true
        rm -rf "$WORK" 2>/dev/null || true
    fi
}
trap cleanup EXIT
# macOS tar would add AppleDouble files and extended attributes.
export COPYFILE_DISABLE=1
TAR_FLAGS=()
if tar --version 2>/dev/null | grep -q bsdtar; then TAR_FLAGS=(--no-xattrs --no-mac-metadata); fi

# --- source archive -------------------------------------------------------
git_root() { git -c safe.directory='*' -C "$ROOT" "$@"; }
make_source_archive() {
    local f stage="$WORK/src"
    if git_root rev-parse --is-inside-work-tree >/dev/null 2>&1 \
            && [ -z "$(git_root status --porcelain --untracked-files=normal)" ]; then
        git_root archive --format=tar.gz --prefix="$NAME-$VERSION/" -o "$WORK/$ARCHIVE" HEAD
        log "source: git archive of HEAD $(git_root rev-parse --short HEAD)"
        return 0
    fi
    mkdir -p "$stage/$NAME-$VERSION"
    if git_root rev-parse --is-inside-work-tree >/dev/null 2>&1; then
        git_root ls-files -z -c -o --exclude-standard |
            while IFS= read -r -d '' f; do
                if [ -e "$ROOT/$f" ] || [ -L "$ROOT/$f" ]; then printf '%s\0' "$f"; fi
            done | (cd "$ROOT" && tar "${TAR_FLAGS[@]+"${TAR_FLAGS[@]}"}" --null -T - -cf -) \
            | tar -C "$stage/$NAME-$VERSION" -xf -
        log "source: working tree with uncommitted changes (tracked + untracked, not ignored)"
    else
        (cd "$ROOT" && tar "${TAR_FLAGS[@]+"${TAR_FLAGS[@]}"}" --exclude=./target --exclude=./.git \
            --exclude=./fuzz/target -cf - .) | tar -C "$stage/$NAME-$VERSION" -xf -
        log "source: working tree (not a git checkout)"
    fi
    tar "${TAR_FLAGS[@]+"${TAR_FLAGS[@]}"}" -C "$stage" -czf "$WORK/$ARCHIVE" "$NAME-$VERSION"
    rm -rf "$stage"
}

# --- temporary recipes ----------------------------------------------------
rewrite() {
    local file="$1"
    shift
    sed "$@" "$file" > "$file.new" && mv "$file.new" "$file"
}

prepare_recipes() {
    local tree="$WORK/tree"
    mkdir -p "$tree"
    cp "$ROOT/Cargo.toml" "$tree/"
    cp -R "$ROOT/packaging" "$tree/packaging"
    # Checksums of the local archive, written into the copies only.
    bash "$tree/packaging/update-checksums.sh" --tarball="$WORK/$ARCHIVE" >/dev/null
    rewrite "$tree/packaging/aur/PKGBUILD" -e 's|^source=.*|source=("$pkgname-$pkgver.tar.gz")|'
    rewrite "$tree/packaging/apk/APKBUILD" -e 's|^source=.*|source="$pkgname-$pkgver.tar.gz"|'
    rewrite "$tree/packaging/rpm/$NAME.spec" -e 's|^Source0:.*|Source0:        %{name}-%{version}.tar.gz|'
    rewrite "$tree/packaging/void/template" -e "s|^distfiles=.*|distfiles=\"file:///host/local-src/$ARCHIVE\"|"
    cp "$WORK/$ARCHIVE" "$tree/packaging/aur/"
    cp "$WORK/$ARCHIVE" "$tree/packaging/apk/"
    log "temporary recipe changes (committed recipes are untouched):"
    case "$FORMAT" in
        arch) diff -u "$ROOT/packaging/aur/PKGBUILD" "$tree/packaging/aur/PKGBUILD" || true ;;
        apk) diff -u "$ROOT/packaging/apk/APKBUILD" "$tree/packaging/apk/APKBUILD" || true ;;
        rpm) diff -u "$ROOT/packaging/rpm/$NAME.spec" "$tree/packaging/rpm/$NAME.spec" || true ;;
        void) diff -u "$ROOT/packaging/void/template" "$tree/packaging/void/template" || true ;;
    esac
}

# --- containers -----------------------------------------------------------
# run IMAGE PLATFORM SCRIPT: the work tree at /work, output at /out, the
# repository's tests at /tests (read-only). PRIVILEGED=1 adds --privileged.
run() {
    local image="$1" platform="$2" script="$3"
    docker run --rm ${platform:+--platform "$platform"} ${PRIVILEGED:+--privileged} \
        -e NAME="$NAME" -e VERSION="$VERSION" -e APP_BIN="$APP_BIN" -e ARCHIVE="$ARCHIVE" \
        -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-}" \
        -v "$WORK:/work" -v "$OUT:/out" -v "$ROOT/tests:/tests:ro" \
        "$image" sh -euc "$script"
}

# Shared smoke test for the clean container, after the package is installed.
SMOKE='
    [ -n "$CARGO_BUILD_JOBS" ] || unset CARGO_BUILD_JOBS
    command -v "$APP_BIN"
    "$APP_BIN" --version
    test -x "/usr/lib/$NAME/wg-helper"
    grep -qx method=package "/usr/lib/$NAME/install-manifest"
    test -f "/usr/share/polkit-1/rules.d/49-$NAME.rules"
    bash /tests/helper-validation.sh "/usr/lib/$NAME/wg-helper"
    echo "smoke test passed"'

# Tests that cannot pass under user-mode CPU emulation (an amd64 container on
# an arm64 host): the emulator does not report a failed exec from a spawned
# child, so a missing or non-executable program does not surface as "not
# found" or "permission denied". They pass natively, in CI and in the other
# formats' builds. Only the names present in this checkout are skipped.
EMULATION_SKIPS=(helper_failures_are_classified_and_keep_their_wording
    helper_invocation_reports_permission_denied_and_busy_lock)

build_arch() {
    local host skip args=""
    host="$(uname -m)"
    if [ "$host" != x86_64 ] && [ "$host" != amd64 ]; then
        for skip in "${EMULATION_SKIPS[@]}"; do
            if grep -rqF "fn $skip(" "$ROOT/src"; then args="$args --skip $skip"; fi
        done
    fi
    if [ -n "$args" ]; then
        rewrite "$WORK/tree/packaging/aur/PKGBUILD" -e "/^check()/,/^}/s|cargo test --frozen\$|cargo test --frozen --$args|"
        grep -q -- "--skip" "$WORK/tree/packaging/aur/PKGBUILD" || die "could not add the emulation skips to check()"
        log "emulated amd64 on $host: check() skips$args"
    fi
    log "makepkg -s in archlinux:latest (linux/amd64)"
    # pacman's sandbox needs kernel features that emulated containers may lack.
    run archlinux:latest linux/amd64 '
        [ -n "$CARGO_BUILD_JOBS" ] || unset CARGO_BUILD_JOBS
        sed -i "/^\[options\]/a DisableSandbox" /etc/pacman.conf
        pacman -Syu --noconfirm --needed base-devel namcap sudo >/dev/null
        useradd -m builder
        echo "builder ALL=(ALL) NOPASSWD: ALL" > /etc/sudoers.d/builder
        cp -r /work/tree/packaging/aur /home/builder/aur
        chown -R builder /home/builder/aur
        cd /home/builder/aur
        runuser -u builder -- makepkg -s --noconfirm
        namcap PKGBUILD
        namcap ./*.pkg.tar.zst | tee /tmp/namcap
        if grep -E "^[^ ]+ E: " /tmp/namcap; then echo "namcap reported errors" >&2; exit 1; fi
        cp ./*.pkg.tar.zst /out/'
    log "install in a clean archlinux:latest"
    run archlinux:latest linux/amd64 '
        sed -i "/^\[options\]/a DisableSandbox" /etc/pacman.conf
        pacman -Syu --noconfirm >/dev/null
        pacman -U --noconfirm /out/*.pkg.tar.zst
        pacman -Qi "$NAME"'"$SMOKE"
}

build_rpm() {
    log "rpmbuild -ba in fedora:latest"
    run fedora:latest "" '
        [ -n "$CARGO_BUILD_JOBS" ] || unset CARGO_BUILD_JOBS
        dnf -y -q install rpm-build rpmdevtools rpmlint cargo xz >/dev/null
        cd /work/tree
        bash packaging/update-checksums.sh --tarball="/work/$ARCHIVE" --vendor
        rpmdev-setuptree
        cp "/work/$ARCHIVE" target/packaging/*-vendor.tar.xz ~/rpmbuild/SOURCES/
        spec="packaging/rpm/$NAME.spec"
        dnf -y -q builddep "$spec" >/dev/null
        rpmbuild -ba "$spec"
        rpmlint ~/rpmbuild/RPMS/*/*.rpm ~/rpmbuild/SRPMS/*.rpm || true
        cp ~/rpmbuild/RPMS/*/*.rpm ~/rpmbuild/SRPMS/*.rpm /out/'
    log "install in a clean fedora:latest"
    run fedora:latest "" '
        rpm=$(ls /out/"$NAME-$VERSION"-*.rpm | grep -v -e "\.src\.rpm$" -e debuginfo -e debugsource)
        dnf -y -q install $rpm >/dev/null
        rpm -qi "$NAME"'"$SMOKE"
}

build_apk() {
    log "abuild -r in alpine:latest"
    run alpine:latest "" '
        [ -n "$CARGO_BUILD_JOBS" ] || unset CARGO_BUILD_JOBS
        apk add -q alpine-sdk atools >/dev/null
        adduser -D builder
        addgroup builder abuild
        cp -r /work/tree/packaging/apk /home/builder/apk
        chown -R builder /home/builder/apk
        su builder -c "abuild-keygen -a -n -q" >/dev/null 2>&1
        cp /home/builder/.abuild/*.rsa.pub /etc/apk/keys/
        cd /home/builder/apk
        apkbuild-lint APKBUILD
        su builder -c "abuild -r"
        mkdir -p /out/keys
        cp /home/builder/.abuild/*.rsa.pub /out/keys/
        cp -r /home/builder/packages/. /out/'
    log "install in a clean alpine:latest"
    run alpine:latest "" '
        cp /out/keys/*.rsa.pub /etc/apk/keys/
        apk add -q bash
        apk add $(find /out -name "$NAME-$VERSION-r*.apk" ! -name "*-doc-*")
        apk info -e "$NAME"'"$SMOKE"
}

build_void() {
    log "xbps-src pkg in ghcr.io/void-linux/void-glibc:latest"
    # As in void-packages CI: an unprivileged user builds in a masterdir made
    # by binary-bootstrap, entered with xbps-uchroot (needs --privileged).
    # The archive goes under hostdir, which the masterdir sees as /host.
    PRIVILEGED=1 run ghcr.io/void-linux/void-glibc:latest "" '
        xbps-install -Syu xbps >/dev/null
        xbps-install -Syu >/dev/null
        xbps-install -y bash git util-linux shadow >/dev/null
        useradd -m builder
        git clone -q --depth 1 https://github.com/void-linux/void-packages /build/void-packages
        cd /build/void-packages
        mkdir -p "srcpkgs/$NAME" hostdir/local-src
        cp /work/tree/packaging/void/template /work/tree/packaging/void/INSTALL \
            /work/tree/packaging/void/INSTALL.msg "srcpkgs/$NAME/"
        cp "/work/$ARCHIVE" hostdir/local-src/
        chown -R builder /build
        su builder -s /bin/bash -c "./xbps-src binary-bootstrap"
        su builder -s /bin/bash -c "./xbps-src -Q -j${CARGO_BUILD_JOBS:-$(nproc)} pkg $NAME"
        mkdir -p /out/binpkgs
        cp hostdir/binpkgs/*.xbps hostdir/binpkgs/*-repodata /out/binpkgs/'
    log "install in a clean ghcr.io/void-linux/void-glibc:latest"
    run ghcr.io/void-linux/void-glibc:latest "" '
        xbps-install -Syu xbps >/dev/null
        xbps-install -Syu >/dev/null
        xbps-install -y bash >/dev/null
        xbps-install -y --repository=/out/binpkgs "$NAME"
        xbps-query "$NAME"'"$SMOKE"
}

make_source_archive
prepare_recipes
"build_$FORMAT"
log "built packages in $OUT:"
find "$OUT" -type f | sort
