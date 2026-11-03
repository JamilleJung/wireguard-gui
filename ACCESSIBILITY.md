# Accessibility

This statement covers the Linux desktop application in v1.9.3. Accessibility
reports and contributions are welcome.

## Current support and limitations

- The GUI runs in a graphical Linux session using X11 or Wayland, subject to
  the runtime requirements in the [distro guide](docs/DISTROS.md).
- It includes standard text-editing fields, but custom buttons, tabs, tunnel
  rows and menus do not have complete explicit keyboard navigation and
  accessibility semantics. Some actions may require a pointer.
- Screen-reader operation has not been verified. The project does not claim
  conformance with an accessibility standard.
- The main window has a minimum size of 720 × 480 logical pixels. Text sizes
  and the light color palette are set in the interface; the app has no zoom,
  font-size or high-contrast settings of its own.
- Tunnel details show text such as **Active** and **Inactive** alongside color.
  The tunnel list also uses colored status dots. Status banners disappear
  after a few seconds, which may make brief messages difficult to read.
- Help and diagnostic dialogs offer **Copy all**. Logs can be copied or saved
  for reading in another application. Check copied output for secrets before
  sharing it.

For a keyboard-oriented interface, see
[wireguard-tui](https://github.com/JamilleJung/wireguard-tui) and its
[accessibility notes](https://github.com/JamilleJung/wireguard-tui/blob/main/ACCESSIBILITY.md).
It has its own limitations.

## Report a barrier

Open a [bug report](https://github.com/JamilleJung/wireguard-gui/issues/new?template=bug_report.yml)
and describe the affected control or task, what you expected, and what happened.
Include the app version, distribution, desktop/session, and any assistive
technology or input method involved. A redacted screenshot or short recording
can help, but is optional.

Do not post private keys, preshared keys, tunnel configs, QR codes or backup
archives. If the report contains personal information, email
[izeystudio@gmail.com](mailto:izeystudio@gmail.com) instead. Report security
vulnerabilities using [SECURITY.md](SECURITY.md).
