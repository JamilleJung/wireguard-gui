#!/usr/bin/env bash
# Write the WireGuard config fixtures under tests/fixtures/configs/.
#
# Every key in the fixtures is FAKE: base64(SHA-256("wireguard fixture <label>")).
# The output is byte-for-byte deterministic, so CI can prove that nobody has
# dropped a real config (or real key) into the fixture tree.
#
# Usage:
#   scripts/gen-fixtures.sh            rewrite tests/fixtures/configs/
#   scripts/gen-fixtures.sh --check    fail if the committed fixtures differ
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/tests/fixtures/configs"
mode="write"
case "${1:-}" in
    "") ;;
    --check) mode=check ;;
    -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "gen-fixtures: unknown argument: $1" >&2; exit 2 ;;
esac

command -v python3 >/dev/null || { echo "gen-fixtures: python3 is required" >&2; exit 2; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

python3 -I - "$work" <<'PY'
import base64
import hashlib
import os
import sys

out = sys.argv[1]
HEADER = (
    "# TEST FIXTURE ONLY - fake keys derived from SHA-256 of a fixed label.\n"
    "# Never use this file for a real tunnel. Regenerate: scripts/gen-fixtures.sh\n"
)


def key(label: str) -> str:
    digest = hashlib.sha256(f"wireguard fixture {label}".encode()).digest()
    return base64.b64encode(digest).decode()


PRIV = key("interface private")
PUB_A = key("peer a public")
PUB_B = key("peer b public")
PUB_C = key("peer c public")
PSK_A = key("peer a preshared")
# Same alphabet and length as a key, but the final character carries nonzero
# padding bits, which wg rejects.
BAD_PADDING = PUB_A[:42] + ("B" if PUB_A[42] != "B" else "C") + "="

fixtures = {
    "valid/interface-only.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32
""",
    "valid/client-full-tunnel.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32
DNS = 1.1.1.1

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = vpn.example.com:51820
PersistentKeepalive = 25
""",
    "valid/client-split-tunnel.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/24
Endpoint = 192.0.2.10:51820
PersistentKeepalive = 25
""",
    "valid/server-multi-peer.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.8.0.1/24, fd00:8::1/64
ListenPort = 51820

[Peer]
PublicKey = {PUB_A}
PresharedKey = {PSK_A}
AllowedIPs = 10.8.0.2/32
AllowedIPs = fd00:8::2/128

[Peer]
PublicKey = {PUB_B}
AllowedIPs = 10.8.0.3/32

[Peer]
PublicKey = {PUB_C}
AllowedIPs = 10.8.0.4/32, fd00:8::4/128
PersistentKeepalive = off
""",
    "valid/dual-stack-repeated.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/24
Address = fd00::2/64
DNS = 1.1.1.1 2606:4700:4700::1111 search.example

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 0.0.0.0/0
AllowedIPs = ::/0
Endpoint = [fe80::1%eth0]:51820
""",
    "valid/inline-comments.conf": f"""[Interface] # home
PrivateKey = {PRIV} # never share
Address = 10.0.0.2/24 # client
[Peer] # gateway
PublicKey = {PUB_A} # server
AllowedIPs = 0.0.0.0/0 # all IPv4
Endpoint = vpn.example.com:51820 # remote
PersistentKeepalive = 25 # seconds
""",
    "valid/wg-quick-options.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32
ListenPort = 0
MTU = 1420
Table = off
FwMark = 0xca6c
SaveConfig = false

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/8
Endpoint = [2001:db8::1]:51820
""",
    "valid/lowercase-keys.conf": f"""[interface]
privatekey = {PRIV}
address = 10.0.0.2/32

[peer]
publickey = {PUB_A}
allowedips = 10.0.0.0/24
""",
    "hooks/postup-hook.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32
PostUp = iptables -A FORWARD -i %i -j ACCEPT
PreDown = true
""",
    "invalid/missing-interface.conf": f"""[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/24
""",
    "invalid/missing-private-key.conf": """[Interface]
Address = 10.0.0.2/32
""",
    "invalid/bad-key-padding.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {BAD_PADDING}
AllowedIPs = 10.0.0.0/24
""",
    "invalid/short-key.conf": f"""[Interface]
PrivateKey = {PRIV[:20]}=
""",
    "invalid/bad-endpoint-port.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/24
Endpoint = vpn.example.com:70000
""",
    "invalid/unbracketed-ipv6-endpoint.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {PUB_A}
AllowedIPs = ::/0
Endpoint = 2001:db8::1:51820
""",
    "invalid/bad-cidr.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/33
""",
    "invalid/unknown-section.conf": f"""[Interface]
PrivateKey = {PRIV}

[Server]
Address = 10.0.0.1/24
""",
    "invalid/unknown-directive.conf": f"""[Interface]
PrivateKey = {PRIV}
Adress = 10.0.0.2/32
""",
    "invalid/spaced-section-header.conf": f"""[Interface]
PrivateKey = {PRIV}
[ Interface ]
Address = 10.0.0.2/32
""",
    "invalid/peer-missing-allowedips.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {PUB_A}
""",
    "invalid/peer-missing-publickey.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
AllowedIPs = 10.0.0.0/24
""",
    "invalid/hidden-earlier-endpoint.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/24
Endpoint = not-a-port
Endpoint = vpn.example.com:51820
""",
    "invalid/keepalive-out-of-range.conf": f"""[Interface]
PrivateKey = {PRIV}

[Peer]
PublicKey = {PUB_A}
AllowedIPs = 10.0.0.0/24
PersistentKeepalive = 65536
""",
    "invalid/control-character.conf": f"""[Interface]
PrivateKey = {PRIV}
Address = 10.0.0.2/32\x01
""",
    "invalid/unicode-whitespace.conf": f"""[Interface]
PrivateKey = {PRIV}
""",
    "invalid/directive-before-section.conf": f"""PrivateKey = {PRIV}
[Interface]
PrivateKey = {PRIV}
""",
}

for rel, body in fixtures.items():
    path = os.path.join(out, rel)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8", newline="\n") as fh:
        fh.write(HEADER + body)
PY

if [ "$mode" = check ]; then
    if ! diff -r "$work" "$dest" >/dev/null 2>&1; then
        echo "gen-fixtures: tests/fixtures/configs differs from the fixture script output:" >&2
        diff -r "$work" "$dest" >&2 || true
        echo "Run scripts/gen-fixtures.sh and commit the result (fixtures must stay fake)." >&2
        exit 1
    fi
    echo "gen-fixtures: fixtures are up to date ($(find "$dest" -type f | wc -l | tr -d ' ') files)"
else
    rm -rf "$dest"
    mkdir -p "$(dirname "$dest")"
    cp -R "$work" "$dest"
    chmod -R u+rwX,go+rX,go-w "$dest"
    echo "gen-fixtures: wrote $(find "$dest" -type f | wc -l | tr -d ' ') files to tests/fixtures/configs"
fi
