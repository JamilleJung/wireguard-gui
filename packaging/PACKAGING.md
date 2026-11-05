# 📦 Packaging wireguard-gui

The release workflow produces an x86_64 `.deb`, x86_64, i686, aarch64 and
armv7 Linux tarballs (glibc 2.35 baseline), and an optional x86_64 AppImage
(attached if its optional build succeeds), with signed checksums. See [releases](../docs/RELEASES.md).
The other files here are source-package recipes; their presence does not mean
an AUR, COPR, Alpine or Void publication is current or officially distributed.

## Installed files and authorization

| Method | Application/helper | polkit rule |
|---|---|---|
| Source or extracted bundle `install.sh` | `/usr/local/bin/wireguard-gui`, `/usr/local/lib/wireguard-gui/wg-helper` | Optional `/etc/polkit-1/rules.d/49-wireguard-gui.rules` |
| Release `.deb` | `/usr/bin/wireguard-gui`, `/usr/lib/wireguard-gui/wg-helper` | `/etc/polkit-1/rules.d/49-wireguard-gui.rules` |
| AUR/RPM/APK/Void recipes | `/usr/bin/wireguard-gui`, `/usr/lib/wireguard-gui/wg-helper` | `/usr/share/polkit-1/rules.d/49-wireguard-gui.rules` |

`install.sh` records its installation in
`/usr/local/lib/wireguard-gui/install-manifest` (`method=installer`); the
recipes install `/usr/lib/wireguard-gui/install-manifest` (`method=package`),
and a `.deb` copy is detected with `dpkg-query -S`. The in-app updater only
updates `install.sh` copies, keeping their prefix and authorization mode, and
refuses package-managed copies with the package-manager command to use.

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
Existing app/helper targets must be root-owned regular files with one link and
no group/other write bits; the installed helper must be executable before an
authorization grant is written.

## Debian / Ubuntu `.deb`

`[package.metadata.deb]` in [Cargo.toml](../Cargo.toml) supplies assets and runtime
dependencies. The release workflow uses `cargo-deb 3.8.0` after the SBOM-enabled
build, tests and compression; output is `wireguard-gui_1.10.0-1_amd64.deb`.

```sh
sudo apt install ./wireguard-gui_1.10.0-1_amd64.deb
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

[`aur/PKGBUILD`](aur/PKGBUILD) is ready for AUR submission together with the
generated [`aur/.SRCINFO`](aur/.SRCINFO) and
[`aur/wireguard-gui.install`](aur/wireguard-gui.install). The binary links only
fontconfig; xkbcommon(-x11), Wayland, libglvnd (EGL/GL), libX11/Xcursor/Xi/
Xrender/xcb and libdbus are loaded at run time, so they are listed as explicit
dependencies along with wireguard-tools, iproute2 and polkit. nftables,
openresolv, sudo, xdg-desktop-portal and minisign are optional dependencies.
Build as an ordinary packaging user:

```sh
cd packaging/aur
makepkg -si
```

## RPM / COPR recipe — `rpm/wireguard-gui.spec`

The spec builds from the tagged `Source0` and the vendored crates in `Source1`
(`packaging/update-checksums.sh --vendor`), so mock/COPR builds need no
network; `%check` runs the tests. BuildRequires cover Rust/C, pkg-config and
the fontconfig, xkbcommon/X11, OpenGL/EGL, Wayland and D-Bus development
packages. Because most GUI libraries are loaded at run time, the runtime
`Requires` name them explicitly (libxkbcommon(-x11), libwayland-client/-egl,
mesa-libEGL/-libGL, libX11(-xcb), libXcursor, libXi, libXrender, libxcb,
dbus-libs) plus wireguard-tools, iproute, polkit and nftables or iptables.
The helper lives under `/usr/lib/wireguard-gui`, labelled by the distro's
SELinux policy; see [DISTROS.md](../docs/DISTROS.md#selinux).

## Alpine and Void templates

[`apk/APKBUILD`](apk/APKBUILD) follows aports conventions (Contributor and
Maintainer lines, `cargo fetch` in `prepare()`, `cargo auditable build --frozen`,
`check()`, a `-doc` subpackage, post-install/post-upgrade messages).
[`void/template`](void/template) uses Void's cargo build style but installs from
`target/$RUST_TARGET/release` with its own `do_install`, so the helper goes to
`/usr/lib/wireguard-gui/` rather than `/usr/bin`. Both list the run-time loaded
GUI libraries explicitly. Release tarballs are glibc builds and do not run on
Alpine; the installer's helper check reports that clearly — build from source
or use the APKBUILD there.

### Recipe maintenance

Run `packaging/update-checksums.sh` after tagging a release. It reads the
version from `Cargo.toml`, syncs `pkgver`/`version`/`Version` in all four
recipes (resetting the package release numbers and adding an RPM changelog
entry on a version change), downloads the tagged source archive (or takes
`--tarball=FILE`), writes its SHA-256 (AUR, Void) and SHA-512 (Alpine)
checksums, and regenerates `aur/.SRCINFO`. `--vendor` also writes the RPM's
offline crate archive (`Source1`) to `target/packaging/`.
`packaging/update-checksums.sh --check` is the offline CI check that versions
and `.SRCINFO` match (`--strict` also rejects placeholder checksums).
`tests/install-packaging-lint.sh` runs `makepkg --printsrcinfo`, `namcap` and
`makepkg --verifysource` (Arch), `rpmspec -P` and `rpmlint` (Fedora),
`apkbuild-lint` and `abuild verify` (Alpine) and `xlint` (Void) in containers;
`--build` also builds the packages.

`tests/package-build.sh arch|rpm|apk|void` builds the real package from the
current checkout, not from a release. It creates `wireguard-gui-VERSION.tar.gz` with the
release archive's name and layout: `git archive` of `HEAD` for a clean tree,
otherwise the tracked and untracked, non-ignored files. It then points temporary
copies of the recipe at that archive, with checksums from `update-checksums.sh
--tarball`; the committed recipes keep their release URLs. The script runs the
distribution's own tool in a container, and each tool also runs the package's
tests:

- `makepkg -s --noconfirm` + `namcap` (`archlinux:latest`, amd64)
- `rpmbuild -ba` with the `Source1` from `update-checksums.sh --vendor` (`fedora:latest`)
- `abuild -r` with a throwaway `abuild-keygen -a -n` key (`alpine:latest`)
- `xbps-src -Q pkg` as an unprivileged user, after `binary-bootstrap`, in a
  void-packages checkout (`void-glibc`, run `--privileged` for `xbps-uchroot`,
  as in void-packages CI)

It then installs the package with the package manager in a clean container and
runs `wireguard-gui --version` and `tests/helper-validation.sh` against the installed
helper. Packages are written to `target/package-build/FORMAT/`, or to the
directory given with `--out=DIR`. If a build runs out of memory, set
`CARGO_BUILD_JOBS=2`. On a non-x86_64 host the Arch build runs under amd64
emulation. The emulator does not report a failed exec from a spawned child, so
the script skips the tests that depend on that, and logs the skips. The
`Package builds` workflow runs all four formats and
uploads the packages. The helper's process tests spawn `python3` and the UI tests
render text, so every recipe declares `python3` and a DejaVu font as check
dependencies. The UI tests need the Slint debug info that release builds
omit, so `%check` and the Void `do_check` run them in the test profile, like the
AUR and Alpine `check()`. The Void `do_check` also skips the one test that needs
a root-owned `/bin/sh`, because xbps-src builds in a masterdir that the build
user owns. The PKGBUILD depends on `hicolor-icon-theme` for the icon
directory and sets `!debug`, because Cargo already strips the binaries.

Every recipe fetches crates only in its prepare step (`cargo fetch --locked`),
then builds and tests offline with `--frozen`; the RPM builds from the vendored
`Source1` archive with no network at all. Each package installs the license,
README, helper under `/usr/lib/wireguard-gui/`, polkit rule, desktop entry and icon,
plus `/usr/lib/wireguard-gui/install-manifest` with `method=package` so the in-app
updater refuses to replace package-owned files and names the package-manager
command instead. AUR (`wireguard-gui.install`), RPM (`%post`), Alpine (`.post-install`)
and Void (`INSTALL`) warn when an `install.sh` copy under `/usr/local` shadows
the package and print the `wireguard` group setup the polkit rule needs.

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

## 1.9.3 compatibility and validation

Before privileged writes or helper grants, `PREFIX` must be an absolute ASCII
path containing only letters, digits, `/`, `_`, `.` and `-`, with no empty,
`.` or `..` components or trailing slash. Existing directory ancestors must be
root-owned without group/other write permission; symlinks are refused. A
group-writable `/usr/local` can therefore fail these checks. An administrator
must choose or prepare a protected path; the installer does not take ownership
of caller directories. `--polkit` supports only `/usr` or `/usr/local`, matching
the bundled rule. Custom sudoers prefixes still require matching frontend helper
discovery configuration.

Installer options are validated; unknown options fail instead of being ignored.
Source builds use the lockfile. Switching sudoers/polkit modes installs and
validates the selected grant before removing the previous grant. Failed sudoers
validation or group enrollment reports failure and keeps prior authorization.
`--polkit` requires pkexec up front; it does not install a rule and then claim
success without the escalation tool. Packages still do not enroll users.

Repeating `--polkit` checks the existing `wireguard` group and account
membership before invoking group-management tools. An already configured
Alpine/BusyBox system can reinstall without optional Shadow `groupadd`/`usermod`.
Missing groups or membership must still be created and verified before the
previous authorization is removed.

Release helper discovery uses this project's own standard `/usr/local/lib/`
and `/usr/lib/` paths, or an adjacent **trusted** executable. The executable must
be root-owned, executable and not group/other writable; its directory ancestry
must also be trusted. A normal user-owned extracted helper is not automatically
trusted: install the verified bundle first. Release builds no longer fall back
to the source build directory or another application's helper. Debug overrides
remain available for development; they do not create an authorization grant.

Background read-only requests use a noninteractive polkit check before pkexec,
so missing authorization produces an actionable error instead of repeated
password dialogs. Explicit mutating actions can still request authentication.
Helper IPC drains input/output concurrently, retains at most 4 MiB per output
stream, and has an overall 90-second deadline plus bounded pipe cleanup. Local
key-generation commands have a 10-second deadline. A privileged process that
the UI user cannot signal may finish independently after a UI-side timeout;
check actual tunnel state before retrying.

Ship the matching helper with the app: release discovery no longer borrows a
helper from the sibling project or an old source build tree. Background reads
need an already authorized sudo/polkit path. The conditional kill-switch
fallback needs iptables-restore and, on IPv6-capable kernels, ip6tables plus
ip6tables-restore; no package recipe should promise a complete firewall setup
from installing the rule alone. User backups require owned private directories
and single-link regular archives; root configs reject symlinks/hardlinks/FIFOs
and group/other write permission. Run the regression and isolated Linux checks
before publishing a new package.
