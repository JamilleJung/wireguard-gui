# 🐧 Per-distro guide: what to install, what to set up, what survives a reboot, and firewall

This answers the practical questions for every supported distro:

- **What do I need to install?**
- **What has to be set up (and does the app do it for me)?**
- **Does it stay working after a restart?**
- **Do I need to touch the firewall?**

> **Short version for most people:** if you're a **VPN client** (you dial out to a
> provider or to your own server), you only need `wireguard-tools`, this app, and -
> *if your config has a `DNS =` line* - a resolvconf provider. **You do not touch
> the firewall, and you do not enable IP forwarding.** The app's first-run **Setup**
> window checks all of this and can fix it for you. Everything below the "Client" line
> is only for a box that *accepts* incoming peers (a **server / gateway**).

---

## 1. Client vs. server - this decides almost everything

| | **Client** (laptop/desktop dialing out) | **Server / gateway** (accepts peers) |
|---|---|---|
| `wireguard-tools` | required | required |
| resolvconf provider | only if the `.conf` has `DNS =` | usually not |
| Open a firewall port | **no** | **yes** - the UDP `ListenPort` |
| IP forwarding (sysctl) | **no** | **yes** |
| NAT / masquerade | **no** | **yes**, if peers reach the internet through it |

The overwhelming majority of users are **clients**. If that's you, sections 2-5 are
all you need; section 6 (server) does not apply.

---

## 2. The kernel module - you do *not* install it here

WireGuard has been **in the mainline Linux kernel since 5.6** (early 2020). When you
activate a tunnel, `wg-quick` runs `modprobe wireguard` automatically - this app
**never bundles or loads kernel modules**. You only need an out-of-tree module on
genuinely old kernels (RHEL 7, Debian buster, Ubuntu 18.04 and older), where you'd
install `wireguard-dkms` / `kmod-wireguard` from your distro. On anything current,
there is nothing to do.

The first-run Setup window reports your kernel and whether the module is available.

---

## 3. What the app sets up for you (and what it deliberately does not)

**It sets up:**

- `wireguard-tools` (`wg` / `wg-quick`) - via `./install.sh`, or the Setup window's
  **Fix automatically** button.
- A resolvconf provider when a `DNS =` line would otherwise fail (see section 5).
- The privileged helper + a passwordless **sudoers** drop-in (or **polkit** rule).
- `/etc/wireguard` (mode `0700`).

**It never:**

- loads kernel modules (the kernel does, on demand),
- opens firewall ports,
- enables IP forwarding,
- adds NAT/masquerade,
- auto-connects a tunnel or enables start-on-boot.

Those last items are system-policy decisions and matter only for a server/gateway -
section 6 shows exactly what to add.

---

## 4. Per-distro cheat sheet

`./install.sh` detects your package manager and does the install + setup for you
(including the GUI/graphics build dependencies). This table is the manual equivalent
for the WireGuard side, plus the two distro-specific gotchas (**resolvconf provider**
and **init system**).

| Distro family | Install WireGuard tools | resolvconf provider for `DNS =` | Init system (start-on-boot) | Default firewall |
|---|---|---|---|---|
| **Debian / Raspberry Pi OS** | `apt install wireguard-tools` | `apt install openresolv` *(minimal Debian has none - this is the classic failure)* | systemd ✔ | none active (nftables present, empty) |
| **Ubuntu / Mint / Pop!_OS** | `apt install wireguard-tools` | already covered by **systemd-resolved** (default) | systemd ✔ | `ufw`, installed but **inactive** by default |
| **Fedora** | `dnf install wireguard-tools` | covered by **systemd-resolved** (default) | systemd ✔ | **firewalld active** |
| **RHEL / Rocky / Alma / CentOS Stream** | `dnf install wireguard-tools` | `dnf install systemd-resolvconf` or enable systemd-resolved | systemd ✔ | **firewalld active** |
| **Arch / Manjaro / EndeavourOS** | `pacman -S wireguard-tools` | `pacman -S openresolv` (unless using systemd-resolved) | systemd ✔ | none active |
| **openSUSE Leap / Tumbleweed** | `zypper install wireguard-tools` | `zypper install openresolv` or systemd-resolved | systemd ✔ | **firewalld active** (Leap) |
| **Alpine** | `apk add wireguard-tools` | `apk add openresolv` | **OpenRC** ✗ (no systemd unit) | none active (`awall`/iptables optional) |
| **Void** | `xbps-install -S wireguard-tools` | `xbps-install openresolv` | **runit** ✗ (no systemd unit) | none active |
| **Solus** | `eopkg install wireguard-tools` | `eopkg install openresolv` | systemd ✔ | none active |

**Init-system note (Alpine, Void, and other non-systemd setups):** everything works
*except* the **Start-on-boot** toggle, which flips the `wg-quick@<name>` systemd unit.
On OpenRC/runit you enable boot-time tunnels with the distro's own service manager
(e.g. an OpenRC init script or a runit service that runs `wg-quick up <name>`).

**Desktop note:** the GUI is a windowed app, so on a headless server prefer the
sibling **[wireguard-tui](https://github.com/JamilleJung/wireguard-tui)** - identical
features in the terminal, with `wg-tui doctor` over SSH.

**NixOS:** out of scope for this file-based tool - manage WireGuard declaratively in
`configuration.nix` (`networking.wireguard.*`) instead, or run the app only to
inspect/QR existing tunnels.

---

## 5. The `DNS =` / resolvconf gotcha (especially minimal Debian)

If a tunnel's `[Interface]` has a `DNS =` line, `wg-quick` calls **`resolvconf`** to
apply it. On minimal Debian (and bare Arch/Alpine/Void) there is no `resolvconf`
binary, so activation aborts with:

```
[#] resolvconf -a wg0 -m 0 -x
/usr/bin/wg-quick: line 32: resolvconf: command not found
```

**Fix - install a provider once:**

```sh
sudo apt install openresolv        # Debian/Arch/etc.
# or, if you have no sudo (Debian-minimal):
su root -c 'apt install openresolv'
```

If **systemd-resolved** is active (`/run/systemd/resolve` exists - the default on
Ubuntu and Fedora), it already satisfies this and you need nothing.

You don't have to remember any of that: the first-run **Setup** window shows a
**"DNS for tunnels (resolvconf)"** line, **Fix automatically** installs the provider,
and `./install.sh` installs it automatically on a minimal box. When an activation does
fail this way, the app explains it in plain language instead of showing the raw error.
The fix **persists** - it's a normal installed package.

---

## 6. Server / gateway setup (only if this box *accepts* peers)

A client needs none of this. A server (or a gateway that routes peers to your LAN or
the internet) needs three things. All can be made permanent.

### 6.1 Open the listen port (UDP)

Your `[Interface]` has a `ListenPort` (commonly `51820`). Allow it inbound:

```sh
# firewalld (Fedora / RHEL / openSUSE):
sudo firewall-cmd --permanent --add-port=51820/udp
sudo firewall-cmd --reload

# ufw (Ubuntu):
sudo ufw allow 51820/udp

# nftables / iptables (Debian/Arch): add an inbound UDP accept rule for 51820.
```

`--permanent` / `ufw` rules survive a reboot. A raw `iptables`/`nft` rule typed at the
shell does **not** - save it (`netfilter-persistent save`, `iptables-save`, or an
`nftables.conf`).

### 6.2 Enable IP forwarding (persistently)

```sh
echo 'net.ipv4.ip_forward=1'            | sudo tee /etc/sysctl.d/99-wireguard.conf
echo 'net.ipv6.conf.all.forwarding=1'   | sudo tee -a /etc/sysctl.d/99-wireguard.conf
sudo sysctl --system
```

Writing it under `/etc/sysctl.d/` is what makes it **survive a reboot**; a bare
`sysctl -w` is lost on restart.

### 6.3 NAT / masquerade (so peers reach the internet through this box)

Either put it in the tunnel's `.conf` as PostUp/PostDown (replace `eth0` with your
real WAN interface):

```ini
PostUp   = iptables -t nat -A POSTROUTING -o eth0 -j MASQUERADE
PostDown = iptables -t nat -D POSTROUTING -o eth0 -j MASQUERADE
```

> The app **warns** when a config contains `PostUp`/`PreUp`/`PostDown`/`PreDown` -
> that warning is expected here; review the commands before activating.

...or with firewalld instead of PostUp lines:

```sh
sudo firewall-cmd --permanent --add-masquerade
sudo firewall-cmd --reload
```

---

## 7. "Does it survive a restart?" - the complete checklist

| Thing | Persists across reboot? | What makes it persist |
|---|---|---|
| `wireguard-tools`, `openresolv` | ✔ | normal installed packages |
| The helper + sudoers/polkit rule + `/etc/wireguard` | ✔ | files on disk (installed once) |
| Your tunnel `.conf` files | ✔ | files in `/etc/wireguard` |
| A tunnel **being connected** | only if you enable it | **Start-on-boot** toggle → `wg-quick@<name>` systemd unit |
| IP forwarding *(server)* | only if written to `/etc/sysctl.d/` | section 6.2 |
| Firewall port / masquerade *(server)* | only if made permanent | `--permanent` / `ufw` / saved nft/iptables |

**Rule of thumb:** packages and files are permanent automatically; *runtime state*
(an active tunnel, a live `sysctl -w`, an un-saved `iptables` rule) is not - use
Start-on-boot for tunnels and the persistent forms above for server settings.

---

## 8. Quick verification

The Setup window appears automatically on first run if anything critical is missing,
and you can re-open it any time something looks off. **Fix automatically** installs
`wireguard-tools`, a resolvconf provider and `/etc/wireguard`; **Show commands** prints
the exact commands so you can run them by hand. On a headless server, use the
terminal sibling's read-only `wg-tui doctor` instead.
