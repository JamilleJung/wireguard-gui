Name:           wireguard-gui
Version:        1.10.0
Release:        1%{?dist}
Summary:        A native Linux GUI for managing WireGuard tunnels

# The binaries statically link the Rust crates pinned in Cargo.lock; they are
# shipped in Source1 together with their license files.
License:        MIT
URL:            https://github.com/JamilleJung/wireguard-gui
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz
# Locked crate sources for an offline build: packaging/update-checksums.sh --vendor
Source1:        %{name}-%{version}-vendor.tar.xz
ExclusiveArch:  x86_64 aarch64
# Fedora's default Rust flags ask for full debug info, which for the compiled
# Slint UI needs more memory than common builders have (see Cargo.toml's
# [profile.test]); line tables keep the debuginfo package useful.
%global rustflags_debuginfo 1

BuildRequires:  cargo >= 1.92
BuildRequires:  rust >= 1.92
BuildRequires:  gcc
# The test suite: the helper's process tests spawn python3; the UI tests
# render text and need a font.
BuildRequires:  python3
BuildRequires:  dejavu-sans-fonts
BuildRequires:  pkgconf-pkg-config
BuildRequires:  fontconfig-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  libxkbcommon-x11-devel
BuildRequires:  dbus-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  mesa-libEGL-devel
BuildRequires:  wayland-devel

Requires:       wireguard-tools
Requires:       iproute
Requires:       polkit
# The kill switch prefers nftables and falls back to iptables.
Requires:       (nftables or iptables-nft or iptables-legacy)
# fontconfig is linked (detected automatically); these are loaded at run time.
Requires:       libxkbcommon
Requires:       libxkbcommon-x11
Requires:       libwayland-client
Requires:       libwayland-egl
Requires:       mesa-libEGL
Requires:       mesa-libGL
Requires:       libX11
Requires:       libX11-xcb
Requires:       libXcursor
Requires:       libXi
Requires:       libXrender
Requires:       libxcb
Requires:       dbus-libs
Recommends:     systemd-resolved
Recommends:     xdg-desktop-portal
Suggests:       minisign

%description
A native Linux GUI for managing plain WireGuard tunnels through wg and wg-quick:
tunnel list with live status, activate/deactivate, import from configuration
files or QR codes, an inline editor with validation, key generation, live
throughput, and a small auditable privileged helper authorized through polkit
or a per-user rule.

%prep
%autosetup -n %{name}-%{version} -a 1
# Build only from the vendored, checksummed crate sources (no network).
mkdir -p .cargo
cat >> .cargo/config.toml << 'CARGO_CONFIG'
[source.crates-io]
replace-with = "vendored-sources"

[source.vendored-sources]
directory = "vendor"
CARGO_CONFIG

%build
cargo build --frozen --release

%check
# The test profile: release builds omit the Slint debug info the UI tests need.
cargo test --frozen

%install
install -Dm0755 target/release/wireguard-gui %{buildroot}%{_bindir}/wireguard-gui
install -Dm0755 target/release/wg-helper %{buildroot}%{_prefix}/lib/%{name}/wg-helper
install -Dm0644 packaging/49-wireguard-gui.rules %{buildroot}%{_datadir}/polkit-1/rules.d/49-wireguard-gui.rules
install -Dm0644 packaging/wireguard-gui.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
install -Dm0644 packaging/wireguard-gui.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
# Tells the in-app updater that rpm owns this copy.
printf 'format=1\napp=%s\nmethod=package\nmanager=rpm\n' %{name} \
    > %{buildroot}%{_prefix}/lib/%{name}/install-manifest

%post
if [ -e /usr/local/bin/wireguard-gui ] || [ -e /usr/local/lib/%{name}/wg-helper ]; then
    echo "warning: another %{name} is installed under /usr/local and shadows this package;" >&2
    echo "         remove it with: PREFIX=/usr/local ./install.sh uninstall" >&2
fi
:

%files
%license LICENSE
%doc README.md
%{_bindir}/wireguard-gui
%dir %{_prefix}/lib/%{name}
%{_prefix}/lib/%{name}/wg-helper
%{_prefix}/lib/%{name}/install-manifest
%{_datadir}/polkit-1/rules.d/49-wireguard-gui.rules
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg

%changelog
* Thu Nov 05 2026 jamillejung <izeystudio@gmail.com> - 1.10.0-1
- Update to 1.10.0.

* Mon Nov 02 2026 jamillejung <izeystudio@gmail.com> - 1.9.3-1
- Reuse existing polkit group/membership on Alpine/BusyBox reinstalls.

* Mon Nov 02 2026 jamillejung <izeystudio@gmail.com> - 1.9.2-1
- Deep audit: shared config validation, bounded helpers, safe backups and updates.
- Serialize tunnel/firewall mutations and require complete kill-switch protection.

* Mon Nov 02 2026 jamillejung <izeystudio@gmail.com> - 1.9.1-1
- Fix kill-switch isolation and rule ordering, helper hangs, config comments,
  backup validation and update staging; refresh dependencies.

* Mon Jul 27 2026 jamillejung <izeystudio@gmail.com> - 1.9.0-1
- Version 1.9.0: detect an inactive resolvectl shim on RHEL-family hosts, so
  tunnels with a 'DNS =' line no longer fail after a healthy-looking check.
- Recommends: systemd-resolved.

* Tue Jun 23 2026 jamillejung <izeystudio@gmail.com> - 1.8.0-1
- Version 1.8.0: connection diagnostics, in-app help, updates window.
- Add BuildRequires: mesa-libEGL-devel, wayland-devel, libxkbcommon-x11-devel,
  pkgconf-pkg-config for Fedora 40+ build compatibility.
- Extend ExclusiveArch to x86_64 + aarch64.
- Add %%check section running cargo test.

* Wed Jun 17 2026 jamillejung <izeystudio@gmail.com> - 1.4.1-1
- Initial RPM packaging (for COPR): live throughput + health, Easy mode,
  hardened helper with timeouts, polkit-based passwordless privilege.
