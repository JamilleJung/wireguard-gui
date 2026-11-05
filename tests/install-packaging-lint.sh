#!/usr/bin/env bash
#
# Validate the distro recipes in packaging/ with each distribution's own tools,
# in throwaway containers:
#
#   tests/install-packaging-lint.sh [arch|fedora|alpine|void|all] [--build]
#
#   arch    makepkg --printsrcinfo == aur/.SRCINFO, namcap, makepkg --verifysource
#   fedora  rpmspec -P (parse), rpmlint
#   alpine  apkbuild-lint, abuild verify (checksums)
#   void    xlint
#
# --build additionally builds and installs the package (slow: a full Rust build
# per distribution; the RPM build uses a vendor archive from update-checksums.sh).
# Needs docker and network access for the images and the tagged source archive.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
NAME="$(awk '/^\[package\]/ {p=1} p && $1 == "name" {gsub(/"/, "", $3); print $3; exit}' "$ROOT/Cargo.toml")"
TARGETS=(); BUILD=0
for arg in "$@"; do
    case "$arg" in
        arch|fedora|alpine|void) TARGETS+=("$arg") ;;
        all) TARGETS=(arch fedora alpine void) ;;
        --build) BUILD=1 ;;
        *) printf 'usage: %s [arch|fedora|alpine|void|all] [--build]\n' "$0" >&2; exit 2 ;;
    esac
done
[ "${#TARGETS[@]}" -gt 0 ] || TARGETS=(arch fedora alpine void)

run() {
    local image="$1" platform="$2" script="$3"
    local -a extra=()
    [ ! -d "$ROOT/target/packaging" ] || extra=(-v "$ROOT/target/packaging:/vendor:ro")
    docker run --rm ${platform:+--platform "$platform"} \
        -e NAME="$NAME" -e BUILD="$BUILD" "${extra[@]+"${extra[@]}"}" \
        -v "$ROOT/packaging:/packaging:ro" "$image" sh -euc "$script"
}

lint_arch() {
    # Arch images are x86_64 only; pacman's sandbox needs seccomp support that
    # emulated containers may lack, so it is disabled for this throwaway image.
    run archlinux:latest linux/amd64 '
        sed -i "/^\[options\]/a DisableSandbox" /etc/pacman.conf
        pacman -Syu --noconfirm --needed base-devel namcap git >/dev/null
        useradd -m builder
        cp -r /packaging/aur /home/builder/aur && chown -R builder /home/builder/aur
        cd /home/builder/aur
        runuser -u builder -- makepkg --printsrcinfo > /tmp/srcinfo
        diff -u .SRCINFO /tmp/srcinfo
        echo ".SRCINFO matches makepkg --printsrcinfo"
        namcap PKGBUILD
        # Placeholder checksums mean the version is not tagged yet, so the
        # release archive does not exist to verify against.
        if grep -q "^sha256sums=(.SKIP.)" PKGBUILD; then
            echo "makepkg --verifysource: skipped (placeholder checksum; not tagged yet)"
        else
            runuser -u builder -- makepkg --verifysource --noconfirm
        fi
        if [ "$BUILD" = 1 ]; then
            pacman -S --noconfirm --needed rust >/dev/null
            runuser -u builder -- makepkg --noconfirm
            namcap ./*.pkg.tar.zst
            pacman -U --noconfirm ./*.pkg.tar.zst
            cat "/usr/lib/$NAME/install-manifest"
        fi
        echo "arch: OK"'
}

lint_fedora() {
    run fedora:latest "" '
        dnf -q install -y rpm-build rpmlint rpmdevtools >/dev/null
        spec="/packaging/rpm/$NAME.spec"
        rpmspec -P "$spec" >/dev/null
        echo "rpmspec -P: parsed"
        rpmlint "$spec"
        if [ "$BUILD" = 1 ]; then
            rpmdev-setuptree
            spectool -g -R "$spec"
            cp /vendor/*.tar.xz ~/rpmbuild/SOURCES/
            dnf -q builddep -y "$spec" >/dev/null
            rpmbuild -ba "$spec"
            rpmlint ~/rpmbuild/RPMS/*/*.rpm
        fi
        echo "fedora: OK"'
}

lint_alpine() {
    run alpine:latest "" '
        apk add -q atools abuild alpine-sdk >/dev/null
        adduser -D builder && addgroup builder abuild
        cp -r /packaging/apk /home/builder/apk && chown -R builder /home/builder/apk
        cd /home/builder/apk
        apkbuild-lint APKBUILD
        echo "apkbuild-lint: clean"
        if grep -q "^SKIP  " APKBUILD; then
            echo "abuild verify: skipped (placeholder checksum; not tagged yet)"
        else
            su builder -c "abuild -q verify" || su builder -c "abuild fetch && abuild verify"
        fi
        if [ "$BUILD" = 1 ]; then
            su builder -c "abuild-keygen -a -n -q" >/dev/null
            su builder -c "abuild -r"
        fi
        echo "alpine: OK"'
}

lint_void() {
    run ghcr.io/void-linux/void-glibc:latest "" '
        xbps-install -Syu xbps >/dev/null 2>&1 || true
        xbps-install -Sy xtools bash >/dev/null
        mkdir -p /tmp/srcpkgs/$NAME
        cp /packaging/void/template /packaging/void/INSTALL /packaging/void/INSTALL.msg /tmp/srcpkgs/$NAME/
        cd /tmp
        xlint srcpkgs/$NAME/template
        echo "void: OK"'
}

for target in "${TARGETS[@]}"; do
    printf '== %s\n' "$target"
    "lint_$target"
done
