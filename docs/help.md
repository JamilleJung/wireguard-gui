# WireGuard, explained

A friendly guide to what WireGuard is, what every field means, and how to use
this app. You don't need to be a network engineer — read the first two sections
and you can set up a tunnel.

---

## 1. The 60-second mental model

A **WireGuard tunnel** is an encrypted point-to-point link between two machines.
Think of it as a private cable plugged between you and a server: anything that
goes through the cable is encrypted and looks, to the rest of your network, like
it's coming from the machine at the other end.

Each tunnel has exactly two halves:

- **The Interface** = *you* (this machine). It has a private key, an address
  inside the tunnel, and optionally a DNS server to use while connected.
- **The Peer** = *the other machine* (usually your VPN server). It has a public
  key, a public address (the **endpoint**) to reach it at, and a list of which
  traffic should go through it (**AllowedIPs**).

When you **activate** a tunnel, the two halves do a quick **handshake** (a
cryptographic hello) and, once that succeeds, traffic flows. If the handshake
never completes, you're "active" but not actually connected — press **Diagnose**
to find out why.

---

## 2. The keys (this is the whole security model)

WireGuard uses **public-key cryptography**. Every machine has a **key pair**:

- **Private key** — your secret. It *never* leaves this machine and you never
  share it. Generate it with **Generate keypair** in the editor.
- **Public key** — derived from the private key. You *do* share this; it's how the
  server identifies you. The app shows it for you to copy.

The rule is symmetric: **the server must know your public key, and you must know
the server's public key.** That's it. If the server doesn't have your public key
in its peer list, it will silently ignore your handshakes (a very common cause of
"it won't connect").

- **Preshared key (PSK)** — *optional* extra symmetric secret added on top, for
  post-quantum belt-and-suspenders. If your config has one, the server must have
  the *same* PSK or the handshake fails. Generate it with **Generate PSK**.

> You usually don't create keys by hand: download a config from your provider /
> server (e.g. wg-easy) and import it — the keys are already filled in and already
> registered on the server.

---

## 3. Every field, in plain words

**Interface (you):**

- **Name** — just a label for the tunnel (e.g. `home`, `work`). Becomes the
  `wg0`-style interface name.
- **Private key** — your secret (see above).
- **Address** — your IP *inside* the tunnel, e.g. `10.100.0.17/24`. The server
  assigns this; it is not your normal LAN/IP.
- **DNS** — which DNS server(s) to use while the tunnel is up, e.g.
  `1.1.1.1`. Optional; needs a resolvconf provider on your system.
- **Listen port / MTU** — advanced, usually left blank (auto). MTU sometimes needs
  lowering (e.g. `1380`) on flaky links.

**Peer (the server):**

- **Public key** — the server's public key (identifies it).
- **Preshared key** — optional shared secret (see above).
- **Endpoint** — where to reach the server: `host:port`, e.g.
  `82.26.104.2:51820` or `[2001:db8::2]:51820`. This is the only address that goes
  out over your *normal* internet.
- **AllowedIPs** — which destinations get routed *into* the tunnel:
  - `0.0.0.0/0` = **full tunnel** — *all* your IPv4 traffic goes through the VPN.
  - `10.100.0.0/24` = **split tunnel** — only that subnet uses the VPN; everything
    else stays on your normal connection.
- **Persistent keepalive** — send a tiny packet every N seconds (commonly `25`) to
  keep the connection alive through NAT/firewalls. Recommended if you're behind a
  home router.

---

## 4. Using this app

**Get a tunnel in:**

- **Import tunnel(s) from file…** — the usual way: a `.conf` from your provider.
- **Import from QR code…** — point it at a QR image.
- **Create tunnel…** — start from a preset (Interface only / Full tunnel / Split
  tunnel); it generates a key pair for you. You then fill in the peer's public key
  and endpoint (from your server) and Save.

**Day to day:**

- Select a tunnel on the left to see its live status (handshake, transfer, speed).
- **Activate / Deactivate** brings the tunnel up/down (`wg-quick`).
- The **⧉** icons copy a value (public key, endpoint, …) to your clipboard.
- **Start on boot** makes it connect automatically at login (systemd).
- **Advanced ▾** on the tunnel card reveals expert actions — **Kill switch**
  (block all traffic if the tunnel drops, no leaks), **Running cfg** (show the
  live config), **Save live** (write the running state back to disk).

**The other tabs:**

- **Log** — recent WireGuard / app activity from the journal. Filter it, copy it,
  save it.
- **Backup** — snapshot every tunnel config; restore/export/delete later.

**When something's wrong:** press **Diagnose**. It checks, in order of how often
each is the culprit: your **system clock** (a wrong clock silently breaks
handshakes — really), the tunnel being up, the handshake completing, the endpoint
being reachable, and DNS. It tells you which step failed and what to do.

---

## 5. Two gotchas worth knowing

- **"Active but nothing loads."** The tunnel interface is up but the handshake
  isn't completing, so no real traffic flows. Causes, most common first: the
  system clock is off; the server doesn't have your public key; the endpoint is
  unreachable. Diagnose pinpoints it.
- **Don't manage the same tunnel from two places.** If NetworkManager (or another
  tool) is also handling this interface, they'll fight. Pick one.

---

This app never phones home, never sees your keys leave the machine, and only asks
for privilege to run `wg` / `wg-quick`. Your configs live in `/etc/wireguard`.
