# Maintainer release checklist

Work through this list for every release of `wireguard-gui`. Each step names the
command or check that proves it; a step that cannot be ticked stops the
release. The Release workflow repeats the automated checks and refuses to
publish if any of them fails, but this list also covers what automation
cannot judge.

## 1. Before tagging

- [ ] `main` is green: every required check in [REPO_SETTINGS.md](REPO_SETTINGS.md)
      passed on the commit you will tag, including **Kill switch e2e** and
      **Firewall namespace test**.
- [ ] No open Dependabot or security PR is waiting that should be in this
      release (`gh pr list --label dependencies`).
- [ ] `cargo audit --deny warnings` is clean on the release lockfile.
- [ ] The version is bumped everywhere and agrees:
      `bash scripts/check-version.sh` (Cargo.toml, Cargo.lock, top
      CHANGELOG entry, PKGBUILD, RPM spec, APKBUILD, Void template).
- [ ] `CHANGELOG.md` has a dated entry for the version that describes every
      user-visible change, including upgrade notes (for example, a kill switch
      built by an older release is reported as `mismatch` and is rebuilt
      automatically by the frontend).
- [ ] README, `docs/help.md`, `docs/TUTORIAL.md`, `SECURITY.md` and
      `docs/DISTROS.md` match the behaviour of this version (keys, flags,
      environment variables, kill switch rules, installer options).
- [ ] Packaging checksums are updated after the tag exists:
      `bash packaging/update-checksums.sh` and the AUR `.SRCINFO`
      regenerated.

## 2. Manual checks on a real system

Run on at least one systemd distribution with nftables and one with the
iptables fallback (or the container recipe in CONTRIBUTING.md):

- [ ] Fresh install with `./install.sh` (sudoers) and with `./install.sh --polkit`;
      `wireguard-gui doctor` reports no failures.
- [ ] Upgrade from the previous release keeps the install method and the
      authorization mode; `./install.sh --uninstall` leaves no files behind.
- [ ] Import a config, connect, edit it while connected (live sync), and
      confirm `wg show <name> fwmark` is unchanged afterwards.
- [ ] Enable the kill switch, confirm traffic outside the tunnel is rejected,
      reload the firewall (`systemctl reload nftables` or `firewall-cmd --reload`)
      and confirm the app reports the loss and repairs it.
- [ ] Disconnect and confirm no `wg_helper_killswitch` table or
      `WGH-KILLSWITCH` chain remains.
- [ ] Keyboard-only pass through the main screens, the editor and help;
      high-contrast and ASCII themes render correctly.

## 3. Tag and publish

- [ ] `git tag -s vX.Y.Z -m "vX.Y.Z"` on the verified commit, then
      `git push origin vX.Y.Z`.
- [ ] The Release workflow passes every gate: version, lint, tests, audit,
      namespace test, kill switch e2e, per-architecture builds, QEMU smoke
      tests, artifact verification, then publish.
- [ ] The `release` environment approval (if configured) was given by a
      maintainer who reviewed the run.

## 4. After publishing

- [ ] `bash scripts/verify-release.sh --tag vX.Y.Z --download --strict --run`
      passes against the published assets, using the committed
      `minisign.pub`.
- [ ] In-app update from the previous version works and preserves the install
      method and authorization mode.
- [ ] Distribution packages (AUR, RPM, APK, Void) are bumped with the new
      checksums; see `packaging/PACKAGING.md`.
- [ ] Announce the release with a link to its CHANGELOG entry.
