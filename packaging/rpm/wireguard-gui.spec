Name:           wireguard-gui
Version:        1.5.1
Release:        1%{?dist}
Summary:        A native Linux GUI for managing WireGuard tunnels

License:        MIT
URL:            https://github.com/JamilleJung/wireguard-gui
Source0:        %{url}/archive/refs/tags/v%{version}.tar.gz#/%{name}-%{version}.tar.gz
ExclusiveArch:  x86_64

BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  gcc
BuildRequires:  fontconfig-devel
BuildRequires:  libxkbcommon-devel
BuildRequires:  dbus-devel
BuildRequires:  mesa-libGL-devel

Requires:       wireguard-tools
Requires:       polkit

%description
A native Linux GUI for managing WireGuard tunnels, modelled on the WireGuard
for Windows client: tunnel list with live status, activate/deactivate, import
from .conf or QR, an inline editor with validation, key generation, live
throughput, and a small auditable privileged helper (sudoers/polkit).

%prep
%autosetup

%build
cargo build --release --locked

%install
install -Dm0755 target/release/wireguard-gui %{buildroot}%{_bindir}/wireguard-gui
install -Dm0755 packaging/wg-helper %{buildroot}%{_prefix}/lib/%{name}/wg-helper
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
* Tue Jun 17 2026 jamillejung <izeystudio@gmail.com> - 1.4.1-1
- Initial RPM packaging (for COPR): live throughput + health, Easy mode,
  hardened helper with timeouts, polkit-based passwordless privilege.
