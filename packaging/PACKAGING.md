# 📦 Packaging wireguard-gui

The current application version is **1.9.1**. The release workflow produces an
x86_64 `.deb`, x86_64/ARM64 Linux tarballs, and an optional x86_64 AppImage
(present in 1.9.1), with signed checksums. See [releases](../docs/RELEASES.md).
The other files here are source-package recipes; their presence does not mean
an AUR, COPR, Alpine or Void publication is current or officially distributed.

## Installed files and authorization

| Method | Application/helper | polkit rule |
|---|---|---|
| Source or extracted bundle `install.sh` | `/usr/local/bin/wireguard-gui`, `/usr/local/lib/wireguard-gui/wg-helper` | Optional `/etc/polkit-1/rules.d/49-wireguard-gui.rules` |
| Release `.deb` | `/usr/bin/wireguard-gui`, `/usr/lib/wireguard-gui/wg-helper` | `/etc/polkit-1/rules.d/49-wireguard-gui.rules` |
| AUR/RPM/APK/Void recipes | `/usr/bin/wireguard-gui`, `/usr/lib/wireguard-gui/wg-helper` | `/usr/share/polkit-1/rules.d/49-wireguard-gui.rules` |

Native packages also install the desktop entry/icon; scripts and recipes use
`755` for executables and `644` for desktop/icon/rule assets. `install.sh` alone
creates the `wg-gui` symlink and defaults to a per-user sudoers grant. Its
`--polkit` option attempts to create the `wireguard` group and enroll the
invoking user instead.

The `.deb` metadata and source recipes have **no post-install user/group
enrollment hook**. Their polkit rule grants passwordless helper execution only
to a member of `wireguard` in an active local session. An administrator must
create the group/add intended users, then users must log out/in. See
[authorization instructions](../README.md#debian-family-package). Do not describe
installation of the rule alone as automatic passwordless access for all users.
SSH sessions do not satisfy the rule's active-local condition.

The rule and helper discovery expect the standard `/usr/local` or `/usr` helper
locations. A custom prefix needs corresponding integration work; changing
`PREFIX` alone is not a complete polkit/helper-discovery configuration.

## Debian / Ubuntu `.deb`

`[package.metadata.deb]` in [Cargo.toml](../Cargo.toml) supplies assets and runtime
dependencies. The release workflow uses `cargo-deb 3.8.0` after the SBOM-enabled
build, tests and compression; output is `wireguard-gui_1.9.1-1_amd64.deb`.

```sh
sudo apt install ./wireguard-gui_1.9.1-1_amd64.deb
```

Inspect the final package's control metadata, helper location and rule path
when changing metadata. A `.deb` build succeeds only if the required artifacts
exist. Group enrollment is a separate administrator task, not a package script.

## Extracted binary bundles and AppImage

Tarballs retain adjacent `wireguard-gui`/`wg-helper`, `install.sh`, README/LICENSE
and `packaging/` desktop/icon/polkit assets. The installer recognizes this layout
when Cargo.toml is absent, stages binaries in `target/release`, installs runtime
dependencies and skips compilers/Rust/development headers. Both architecture
archives are checked by `tests/installer-sanity.sh` during release packaging.
The updater also stages verified binaries and sets `WG_USE_PREBUILT=1` explicitly.

Tarballs are native Linux builds, not a promise of compatibility with every
libc/distribution. Build from source or use a distro-native package when the
release binary's runtime requirements do not match the host.

The AppImage includes uncompressed build copies for linuxdeploy to process;
its packaging job is optional. Do not claim that an AppImage alone configures
passwordless helper authorization. A system helper/native install provides the
usual integration; test privileged actions separately from launching the UI.

## Arch recipe — `aur/PKGBUILD`

The recipe fetches the versioned GitHub **source** archive, fetches locked
Cargo dependencies, builds frozen release binaries, and installs the files above.
Build as an ordinary packaging user in a suitable Arch environment:

```sh
cd packaging/aur
makepkg -si
```

Before publishing, replace `sha256sums=('SKIP')` with verified source checksums,
generate `.SRCINFO` with `makepkg --printsrcinfo`, and test the package. Merely
bumping `pkgver` is not a checksum or publication step.

## RPM / COPR recipe — `rpm/wireguard-gui.spec`

The spec uses the versioned GitHub source archive, builds both native binaries,
and runs release tests. Its BuildRequires cover Rust/C, pkg-config, fontconfig,
xkbcommon/X11, OpenGL/EGL, Wayland and D-Bus development libraries. It installs
the helper under `/usr/lib/wireguard-gui` and the rule under `/usr/share/polkit-1`.
Use a normal RPM build environment and the matching source tarball in SOURCES;
COPR deployment is a separate maintainer operation, not a GitHub release job.

## Alpine and Void templates

`apk/APKBUILD` builds source binaries and runs tests; replace `sha512sums="SKIP"`
with the verified source archive checksum before submission.
`void/template` uses Void's Cargo build style; replace `checksum=@CHECKSUM@`.
Both recipes install the helper, desktop assets and group-restricted polkit rule.
Validate dependencies/toolchain support on the target distro before publishing.

## Flatpak — archived experiment

The Flatpak manifest is an archived, non-functional starting point with historical
SDK/source pins. It is not release-tested or a supported installation route.
The app needs privileged access to `/etc/wireguard`, WireGuard tools and host
service/firewall operations that the sandbox does not provide by itself. Use a
native package, source installer or compatible binary bundle instead.

## Release checklist

Update package versions together with Cargo.toml/Cargo.lock and the changelog.
Verify checksums for source recipes, real package contents, authorization/user
enrollment instructions and source-versus-binary archive layout. Follow
[release automation](../docs/RELEASES.md#how-releases-are-built) and keep the
committed public signing key consistent with the release signing secret.
