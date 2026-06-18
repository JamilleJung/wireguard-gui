# 📦 Packaging wireguard-gui

Distro packages so users don't have to run `install.sh` by hand. Each installs:

- the `wireguard-gui` binary → `/usr/bin`
- the privileged helper → `/usr/lib/wireguard-gui/wg-helper`
- the `.desktop` launcher + icon
- the **polkit** rule → `/usr/share/polkit-1/rules.d/49-wireguard-gui.rules`
  (passwordless helper for an active local session)

`wireguard-tools` is a runtime dependency; `polkit` provides the privilege.

## Arch (AUR) - `aur/PKGBUILD`

```sh
# locally:
cd packaging/aur && makepkg -si
```

To publish to the AUR: run `makepkg -g` to fill in real `sha256sums`, then push
`PKGBUILD` + `.SRCINFO` (`makepkg --printsrcinfo > .SRCINFO`) to
`ssh://aur@aur.archlinux.org/wireguard-gui.git`. Bump `pkgver` per release.

## Fedora / RHEL / Rocky (COPR) - `rpm/wireguard-gui.spec`

```sh
# locally:
rpmbuild -ba packaging/rpm/wireguard-gui.spec   # after putting the source tarball in ~/rpmbuild/SOURCES
```

For **COPR**: create a project, add this spec (or a SCM build pointing at the
repo + `Source0` tarball), and COPR builds RPMs for Fedora/EPEL automatically on
each tag. Bump `Version` per release.

## Debian / Ubuntu (.deb)

Already produced by the release workflow via `cargo deb`
(`[package.metadata.deb]` in `Cargo.toml`). Install with
`sudo apt install ./wireguard-gui_*_amd64.deb` - it sets up the polkit rule.

## Alpine - `apk/APKBUILD`

Template for Alpine maintainers. It builds both native binaries from source and
installs `wireguard-gui`, `wg-helper`, the desktop file/icon, and the polkit
rule. Replace `sha512sums="SKIP"` with the real release tarball checksum before
submitting to an Alpine repository.

## Void Linux - `void/template`

Template for Void maintainers. It uses Void's Cargo build style and installs the
same native files as the other distro packages. Replace `checksum=@CHECKSUM@`
with the real release tarball checksum before submitting.

## Flatpak - `flatpak/...yaml` (EXPERIMENTAL, not recommended)

A WireGuard manager is a **privileged system tool**: it needs root, `/etc/wireguard`
write access, and to drive `wg-quick`/systemd via sudoers/polkit. The Flatpak
sandbox deliberately blocks all of that, so Flatpak is **not a good fit** and the
manifest here is a non-functional starting point only (see its header comment).
Use the native install, the AUR package, the RPM/COPR build, or the `.deb`/AppImage
instead.
