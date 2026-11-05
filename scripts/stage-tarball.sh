#!/usr/bin/env bash
# Stage a release tarball with a fixed layout, fixed modes, root ownership,
# sorted entries and the commit time as mtime, so the archive is reproducible
# and scripts/verify-release.sh can check it exactly.
#
# Usage: scripts/stage-tarball.sh VERSION ARCH [BINARY_DIR]
#   BINARY_DIR defaults to target/release. Writes
#   <project>-VERSION-ARCH-linux.tar.gz in the current directory.
# Requires GNU tar and gzip.
set -euo pipefail

[ $# -ge 2 ] || { sed -n '2,9p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2; exit 2; }
version="$1"
arch="$2"
bindir="${3:-target/release}"
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
    || { echo "stage-tarball: bad version '$version'" >&2; exit 2; }
case "$arch" in x86_64|i686|aarch64|armv7) ;; *) echo "stage-tarball: bad arch '$arch'" >&2; exit 2 ;; esac

project="$(awk '/^\[/{p=($0=="[package]");next} p&&$1=="name"{gsub(/"/,"",$3);print $3;exit}' "$root/Cargo.toml")"
case "$project" in
    wireguard-tui) app=wg-tui ;;
    wireguard-gui) app=wireguard-gui ;;
    *) echo "stage-tarball: unknown project '$project'" >&2; exit 2 ;;
esac

d="${project}-${version}-${arch}-linux"
rm -rf "$d"
mkdir -p "$d/packaging"
cp "$bindir/$app" "$bindir/wg-helper" "$d/"
cp "$root/packaging/49-${project}.rules" "$root/packaging/${project}.desktop" \
    "$root/packaging/${project}.svg" "$d/packaging/"
cp "$root/install.sh" "$root/README.md" "$root/LICENSE" "$d/"
chmod 0755 "$d" "$d/packaging" "$d/$app" "$d/wg-helper" "$d/install.sh"
chmod 0644 "$d/README.md" "$d/LICENSE" "$d"/packaging/*

epoch="$(git -C "$root" log -1 --format=%ct 2>/dev/null || date +%s)"
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@${epoch}" \
    --format=gnu -cf - "$d" | gzip -n -9 >"${d}.tar.gz"
test -s "${d}.tar.gz"
rm -rf "$d"
echo "stage-tarball: ${d}.tar.gz"
