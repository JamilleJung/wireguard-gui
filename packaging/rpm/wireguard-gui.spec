Name:           wireguard-gui
Version:        1.8.0
Release:        1%{?dist}
Summary:        A native Linux GUI for managing WireGuard tunnels

License:        MIT
URL:            https://github.com/JamilleJung/wireguard-gui
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz
ExclusiveArch:  x86_64 aarch64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  pkgconf-pkg-config
BuildRequires:  fontconfig-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  libxkbcommon-x11-devel
BuildRequires:  dbus-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  mesa-libEGL-devel
# arboard crate (clipboard) with wayland-data-control feature needs wayland-client.
BuildRequires:  wayland-devel

Requires:       wireguard-tools
Requires:       polkit

%description
A native Linux GUI for managing plain WireGuard tunnels through wg and wg-quick:
tunnel list with live status, activate/deactivate, import from .conf or QR, an
inline editor with validation, key generation, live throughput, and a small
auditable privileged helper (sudoers/polkit).

%prep
%autosetup

%build
cargo build --release --locked

%check
cargo test --release --locked

%install
install -Dm0755 target/release/wireguard-gui %{buildroot}%{_bindir}/wireguard-gui
install -Dm0755 target/release/wg-helper %{buildroot}%{_prefix}/lib/%{name}/wg-helper
install -Dm0644 packaging/wireguard-gui.desktop %{buildroot}%{_datadir}/applications/%{name}.desktop
install -Dm0644 packaging/wireguard-gui.svg %{buildroot}%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
install -Dm0644 packaging/49-wireguard-gui.rules %{buildroot}%{_datadir}/polkit-1/rules.d/49-wireguard-gui.rules

%files
%license LICENSE
%doc README.md
%{_bindir}/wireguard-gui
%dir %{_prefix}/lib/%{name}
%{_prefix}/lib/%{name}/wg-helper
%{_datadir}/applications/%{name}.desktop
%{_datadir}/icons/hicolor/scalable/apps/%{name}.svg
%{_datadir}/polkit-1/rules.d/49-wireguard-gui.rules

%changelog
* Mon Jun 23 2026 jamillejung <izeystudio@gmail.com> - 1.8.0-1
- Version 1.8.0: connection diagnostics, in-app help, updates window.
- Add BuildRequires: mesa-libEGL-devel, wayland-devel, libxkbcommon-x11-devel,
  pkgconf-pkg-config for Fedora 40+ build compatibility.
- Extend ExclusiveArch to x86_64 + aarch64.
- Add %check section running cargo test.

* Tue Jun 17 2026 jamillejung <izeystudio@gmail.com> - 1.4.1-1
- Initial RPM packaging (for COPR): live throughput + health, Easy mode,
  hardened helper with timeouts, polkit-based passwordless privilege.
