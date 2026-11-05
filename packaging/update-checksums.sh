#!/usr/bin/env bash
#
# Keep the distro recipes in packaging/ in step with Cargo.toml.
#
#   packaging/update-checksums.sh                 sync versions, download the tagged
#                                                 source archive, write its checksums
#                                                 and regenerate aur/.SRCINFO
#   packaging/update-checksums.sh --tarball=FILE  use a local source archive instead
#   packaging/update-checksums.sh --vendor        also build the RPM's offline crate
#                                                 archive (Source1) in target/packaging/
#   packaging/update-checksums.sh --check         offline: verify versions and .SRCINFO
#                                                 are in sync (CI); add --strict to also
#                                                 require real checksums
#
# Run it after tagging a release (the archive for vX.Y.Z must exist) and commit
# the updated recipes. A version change resets the package release numbers and
# adds an RPM changelog entry.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
PKG="$ROOT/packaging"
CHECK=0; STRICT=0; VENDOR=0; TARBALL=""
for arg in "$@"; do
    case "$arg" in
        --check) CHECK=1 ;;
        --strict) STRICT=1 ;;
        --vendor) VENDOR=1 ;;
        --tarball=?*) TARBALL="${arg#--tarball=}" ;;
        -h|--help) sed -n '3,17p' "$0"; exit 0 ;;
        *) printf 'Unknown option: %s\n' "$arg" >&2; exit 2 ;;
    esac
done

die() { printf 'update-checksums: %s\n' "$*" >&2; exit 1; }

cargo_field() {
    awk -v key="$1" '
        /^\[/ { in_package = ($0 == "[package]") }
        in_package && $1 == key && $2 == "=" { gsub(/"/, "", $3); print $3; exit }
    ' "$ROOT/Cargo.toml"
}

NAME="$(cargo_field name)"
VERSION="$(cargo_field version)"
REPO_URL="$(cargo_field repository)"
[[ "$NAME" =~ ^[a-z0-9-]+$ ]] || die "cannot read the package name from Cargo.toml"
[[ "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "cannot read a X.Y.Z version from Cargo.toml"

PKGBUILD="$PKG/aur/PKGBUILD"
SRCINFO="$PKG/aur/.SRCINFO"
APKBUILD="$PKG/apk/APKBUILD"
TEMPLATE="$PKG/void/template"
SPEC="$PKG/rpm/$NAME.spec"
ARCHIVE="$NAME-$VERSION.tar.gz"
for f in "$PKGBUILD" "$APKBUILD" "$TEMPLATE" "$SPEC"; do
    [ -f "$f" ] || die "missing recipe $f"
done

recipe_versions() {
    printf 'aur %s\n' "$(sed -n 's/^pkgver=//p' "$PKGBUILD")"
    printf 'apk %s\n' "$(sed -n 's/^pkgver=//p' "$APKBUILD")"
    printf 'void %s\n' "$(sed -n 's/^version=//p' "$TEMPLATE")"
    printf 'rpm %s\n' "$(sed -n 's/^Version:[[:space:]]*//p' "$SPEC")"
}

# makepkg --printsrcinfo, without needing an Arch host. Field order and format
# match makepkg's; the result is cross-checked against makepkg in CI.
# shellcheck disable=SC2154 # The fields are assigned by the sourced PKGBUILD.
generate_srcinfo() (
    set +u
    # shellcheck disable=SC1090
    source "$PKGBUILD"
    field() {
        local key="$1" value
        shift
        for value in "$@"; do
            [ -n "$value" ] && printf '\t%s = %s\n' "$key" "$value"
        done
        return 0
    }
    printf 'pkgbase = %s\n' "$pkgname"
    field pkgdesc "$pkgdesc"
    field pkgver "$pkgver"
    field pkgrel "$pkgrel"
    field epoch "${epoch:-}"
    field url "$url"
    field install "${install:-}"
    field changelog "${changelog:-}"
    field arch "${arch[@]}"
    field groups "${groups[@]}"
    field license "${license[@]}"
    field checkdepends "${checkdepends[@]}"
    field makedepends "${makedepends[@]}"
    field depends "${depends[@]}"
    field optdepends "${optdepends[@]}"
    field provides "${provides[@]}"
    field conflicts "${conflicts[@]}"
    field replaces "${replaces[@]}"
    field backup "${backup[@]}"
    field options "${options[@]}"
    field source "${source[@]}"
    field validpgpkeys "${validpgpkeys[@]}"
    field noextract "${noextract[@]}"
    field cksums "${cksums[@]}"
    field md5sums "${md5sums[@]}"
    field sha1sums "${sha1sums[@]}"
    field sha224sums "${sha224sums[@]}"
    field sha256sums "${sha256sums[@]}"
    field sha384sums "${sha384sums[@]}"
    field sha512sums "${sha512sums[@]}"
    field b2sums "${b2sums[@]}"
    printf '\npkgname = %s\n' "$pkgname"
)

if [ "$CHECK" = 1 ]; then
    status=0
    while read -r recipe version; do
        if [ "$version" != "$VERSION" ]; then
            printf '%s recipe version %s != Cargo.toml %s\n' "$recipe" "$version" "$VERSION" >&2
            status=1
        fi
    done < <(recipe_versions)
    if ! diff -u "$SRCINFO" <(generate_srcinfo) >&2; then
        printf 'aur/.SRCINFO is stale: run packaging/update-checksums.sh\n' >&2
        status=1
    fi
    if grep -q "^\* .* - $VERSION-" "$SPEC"; then :; else
        printf 'rpm %%changelog has no entry for %s\n' "$VERSION" >&2
        status=1
    fi
    if grep -Eq "sha256sums=\('SKIP'\)|^SKIP  |@CHECKSUM@" "$PKGBUILD" "$APKBUILD" "$TEMPLATE"; then
        if [ "$STRICT" = 1 ]; then
            printf 'placeholder checksums remain\n' >&2
            status=1
        else
            printf 'note: placeholder checksums remain (run without --check after tagging v%s)\n' "$VERSION" >&2
        fi
    fi
    [ "$status" = 0 ] && printf 'packaging recipes match %s %s\n' "$NAME" "$VERSION"
    exit "$status"
fi

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# --- versions -------------------------------------------------------------
old_version="$(sed -n 's/^pkgver=//p' "$PKGBUILD")"
sed -i.bak "s/^pkgver=.*/pkgver=$VERSION/" "$PKGBUILD" "$APKBUILD"
sed -i.bak "s/^version=.*/version=$VERSION/" "$TEMPLATE"
sed -i.bak "s/^Version:\([[:space:]]*\).*/Version:\1$VERSION/" "$SPEC"
if [ "$old_version" != "$VERSION" ]; then
    sed -i.bak "s/^pkgrel=.*/pkgrel=1/" "$PKGBUILD"
    sed -i.bak "s/^pkgrel=.*/pkgrel=0/" "$APKBUILD"
    sed -i.bak "s/^revision=.*/revision=1/" "$TEMPLATE"
    sed -i.bak "s/^Release:\([[:space:]]*\).*/Release:\11%{?dist}/" "$SPEC"
fi
if ! grep -q "^\* .* - $VERSION-" "$SPEC"; then
    packager="$(sed -n 's/^\* [A-Z][a-z]* [A-Z][a-z]* [0-9]* [0-9]* \(.*\) - [0-9].*/\1/p' "$SPEC" | head -n 1)"
    entry="* $(LC_ALL=C date -u '+%a %b %d %Y') ${packager:-packager} - $VERSION-1"
    awk -v entry="$entry" -v version="$VERSION" '
        { print }
        $0 == "%changelog" { print entry; print "- Update to " version "."; print "" }
    ' "$SPEC" > "$work/spec" && cat "$work/spec" > "$SPEC"
fi

# --- checksums ------------------------------------------------------------
if [ -z "$TARBALL" ]; then
    TARBALL="$work/$ARCHIVE"
    curl -fsSL --proto '=https' --tlsv1.2 -o "$TARBALL" "$REPO_URL/archive/refs/tags/v$VERSION.tar.gz" \
        || die "could not download $REPO_URL/archive/refs/tags/v$VERSION.tar.gz (is v$VERSION tagged?)"
fi
[ -f "$TARBALL" ] || die "no such archive: $TARBALL"
tar -tzf "$TARBALL" "$NAME-$VERSION/Cargo.toml" >/dev/null 2>&1 \
    || die "$TARBALL is not the $NAME $VERSION source archive"
digest() {
    if command -v "sha$1sum" >/dev/null 2>&1; then "sha$1sum" "$2"; else shasum -a "$1" "$2"; fi | cut -d' ' -f1
}
sha256="$(digest 256 "$TARBALL")"
sha512="$(digest 512 "$TARBALL")"

sed -i.bak "s/^sha256sums=.*/sha256sums=('$sha256')/" "$PKGBUILD"
sed -i.bak "s/^checksum=.*/checksum=$sha256/" "$TEMPLATE"
awk -v line="$sha512  $ARCHIVE" '
    /^sha512sums="/ { print "sha512sums=\""; print line; skip = 1; next }
    skip && /^"$/ { skip = 0 }
    !skip { print }
' "$APKBUILD" > "$work/apk" && cat "$work/apk" > "$APKBUILD"
rm -f "$PKGBUILD.bak" "$APKBUILD.bak" "$TEMPLATE.bak" "$SPEC.bak"

# --- .SRCINFO -------------------------------------------------------------
if command -v makepkg >/dev/null 2>&1; then
    (cd "$PKG/aur" && makepkg --printsrcinfo) > "$SRCINFO"
else
    generate_srcinfo > "$SRCINFO"
fi

# --- RPM vendor archive ---------------------------------------------------
if [ "$VENDOR" = 1 ]; then
    command -v cargo >/dev/null 2>&1 || die "--vendor needs cargo"
    tar -xzf "$TARBALL" -C "$work"
    (cd "$work/$NAME-$VERSION" && cargo vendor --locked --versioned-dirs vendor >/dev/null)
    mkdir -p "$ROOT/target/packaging"
    tar -C "$work/$NAME-$VERSION" -cJf "$ROOT/target/packaging/$NAME-$VERSION-vendor.tar.xz" vendor
    printf 'wrote target/packaging/%s-%s-vendor.tar.xz (RPM Source1)\n' "$NAME" "$VERSION"
fi

printf 'updated recipes for %s %s (sha256 %s)\n' "$NAME" "$VERSION" "$sha256"
