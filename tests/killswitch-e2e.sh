#!/usr/bin/env bash
# End-to-end test of a real WireGuard tunnel protected by the helper's kill
# switch, run in disposable network namespaces.
#
#   sudo bash tests/killswitch-e2e.sh path/to/wg-helper
#
# Every combination of endpoint family (IPv4, IPv6) and firewall backend
# (nftables, iptables) is exercised:
#   * traffic flows through the tunnel and a bypass of the tunnel is rejected
#   * IPv6 Neighbor Discovery keeps the endpoint reachable after the neighbor
#     cache is flushed (as when entries expire)
#   * an existing administrator deny rule still applies with the switch on
#   * a live config update (save + sync) keeps the runtime fwmark, listen port
#     and kill switch intact; a raw `wg syncconf` that drops the mark is
#     detected as a mismatch and repaired
#   * a firewall reload that wipes the rules is reported as missing and repaired
#   * losing the interface does not leak traffic to the underlying network
#   * a full disk during save leaves the previous config intact
#   * disconnecting removes every rule
#
# The client runs in its own network AND mount namespace with private tmpfs
# instances over /etc/wireguard and /run/wg-helper, so nothing on the host is
# read or changed. Requires root, the wireguard kernel module, wireguard-tools,
# iproute2, nftables and iptables.
set -euo pipefail

if [ "${1:-}" = "--client" ]; then
    MODE=client
    shift
else
    MODE=host
fi

HELPER="$(readlink -f "${1:?usage: killswitch-e2e.sh path/to/wg-helper}")"
SRV=wgks-srv
CLI=wgks-cli
pass=0

log() { printf '  %s\n' "$*"; }
ok() { pass=$((pass + 1)); printf '  ok   %s\n' "$*"; }
fail() { printf '  FAIL %s\n' "$*" >&2; exit 1; }
srv() { ip netns exec "$SRV" "$@"; }

if [ "$MODE" = host ]; then
    [ "$(uname -s)" = Linux ] || { echo "requires Linux" >&2; exit 2; }
    [ "$(id -u)" = 0 ] || { echo "run as root (sudo)" >&2; exit 2; }
    for tool in ip wg wg-quick nft iptables ip6tables iptables-restore ip6tables-restore unshare ping; do
        command -v "$tool" >/dev/null || { echo "missing tool: $tool" >&2; exit 2; }
    done
    [ -x "$HELPER" ] || { echo "helper not executable: $HELPER" >&2; exit 2; }
    # The helper only runs tools from fixed system paths and must be root-owned
    # to be trusted by the frontends; copy it so the test runs as installed.
    work="$(mktemp -d)"
    cleanup() {
        ip netns del "$CLI" 2>/dev/null || true
        ip netns del "$SRV" 2>/dev/null || true
        rm -rf "$work"
    }
    trap cleanup EXIT
    install -m 0755 "$HELPER" "$work/wg-helper"
    status=0
    for family in 4 6; do
        for backend in nft iptables; do
            cleanup_ns() { ip netns del "$CLI" 2>/dev/null || true; ip netns del "$SRV" 2>/dev/null || true; }
            cleanup_ns
            echo "== endpoint IPv$family, backend $backend"
            ip netns add "$SRV"
            ip netns add "$CLI"
            ip link add veth-s netns "$SRV" type veth peer name veth-c netns "$CLI"
            srv ip link set lo up
            srv ip link set veth-s up
            srv ip addr add 192.0.2.1/24 dev veth-s
            srv ip addr add 2001:db8:1::1/64 dev veth-s nodad
            # A host only reachable over the underlying network: the target
            # of the leak checks.
            srv ip addr add 198.51.100.1/32 dev lo
            srv ip addr add 2001:db8:ff::1/128 dev lo
            srv sysctl -qw net.ipv4.ip_forward=1
            ip netns exec "$CLI" ip link set lo up
            ip netns exec "$CLI" ip link set veth-c up
            ip netns exec "$CLI" ip addr add 192.0.2.2/24 dev veth-c
            ip netns exec "$CLI" ip addr add 2001:db8:1::2/64 dev veth-c nodad
            ip netns exec "$CLI" ip route add default via 192.0.2.1
            ip netns exec "$CLI" ip -6 route add default via 2001:db8:1::1

            srv_key="$(wg genkey)"; cli_key="$(wg genkey)"
            srv_pub="$(printf '%s' "$srv_key" | wg pubkey)"
            cli_pub="$(printf '%s' "$cli_key" | wg pubkey)"
            srv ip link add wg0 type wireguard
            # Pass the key through a pipe like wg-quick does: confined `wg`
            # builds (AppArmor) may not open arbitrary files under /tmp.
            srv wg set wg0 private-key <(printf '%s' "$srv_key") listen-port 51820 \
                peer "$cli_pub" allowed-ips 10.10.0.2/32,fd00:10::2/128
            srv ip addr add 10.10.0.1/24 dev wg0
            srv ip addr add fd00:10::1/64 dev wg0 nodad
            srv ip link set wg0 up
            if [ "$family" = 4 ]; then endpoint="192.0.2.1:51820"; else endpoint="[2001:db8:1::1]:51820"; fi
            cat > "$work/wg0.conf" <<EOF
[Interface]
PrivateKey = $cli_key
Address = 10.10.0.2/24, fd00:10::2/64

[Peer]
PublicKey = $srv_pub
Endpoint = $endpoint
AllowedIPs = 0.0.0.0/0, ::/0
EOF
            if ip netns exec "$CLI" unshare --mount --propagation private \
                bash "$0" --client "$work/wg-helper" "$work" "$family" "$backend"; then
                :
            else
                status=1
                echo "== FAILED: endpoint IPv$family, backend $backend" >&2
            fi
            cleanup_ns
        done
    done
    exit "$status"
fi

# ---------------------------------------------------------------- client side
WORK="${2:?}"; FAMILY="${3:?}"; BACKEND="${4:?}"
H="$HELPER"

mkdir -p /etc/wireguard /run/wg-helper
mount -t tmpfs -o mode=0700,size=4m tmpfs /etc/wireguard
mount -t tmpfs -o mode=0700 tmpfs /run/wg-helper
if [ "$BACKEND" = iptables ]; then
    # Hide nft from both the helper and wg-quick so the fallback is used:
    # overlay each system directory holding it with a copy that lacks it.
    for dir in /usr/sbin /usr/bin /sbin /bin; do
        [ -e "$dir/nft" ] && [ ! -L "$dir" ] || continue
        copy="$(mktemp -d)"
        cp -a "$dir/." "$copy/"
        rm -f "$copy/nft"
        mount --bind "$copy" "$dir"
    done
    if command -v nft >/dev/null; then fail "could not hide nft for the iptables run"; fi
fi
install -m 0600 "$WORK/wg0.conf" /etc/wireguard/wg0.conf

ping4() { ping -4 -c 1 -W 1 "$@" >/dev/null 2>&1; }
ping6() { ping -6 -c 1 -W 1 "$@" >/dev/null 2>&1; }
retry() { local _; for _ in 1 2 3 4 5 6 7 8; do "$@" && return 0; sleep 0.5; done; return 1; }
status_is() {
    local got
    got="$("$H" killswitch-status wg0)"
    [ "$got" = "$1" ] || fail "kill switch status: expected $1, got $got${2:+ ($2)}"
}
tunnel_works() {
    retry ping4 10.10.0.1 || fail "IPv4 through the tunnel ($1)"
    retry ping6 fd00:10::1 || fail "IPv6 through the tunnel ($1)"
}
no_rules() {
    if nft list table inet wg_helper_killswitch >/dev/null 2>&1; then fail "nft table left behind"; fi
    if iptables -S 2>/dev/null | grep -q -e WGH-KILLSWITCH -e wg-helper-killswitch; then fail "iptables rules left behind"; fi
    if ip6tables -S 2>/dev/null | grep -q -e WGH-KILLSWITCH -e wg-helper-killswitch; then fail "ip6tables rules left behind"; fi
}

"$H" up wg0
"$H" active | grep -qx wg0 || fail "wg0 is not active after up"
tunnel_works "before kill switch"
ok "tunnel up"

status_is disabled
"$H" killswitch-enable wg0
status_is enabled
mark="$(wg show wg0 fwmark)"
port="$(wg show wg0 listen-port)"
[ "$mark" != off ] || fail "no fwmark after enabling the kill switch"
tunnel_works "with kill switch"
ok "kill switch enabled (mark $mark)"

if ping4 -I veth-c 192.0.2.1; then fail "IPv4 bypass of the tunnel was not rejected"; fi
if ping6 -I veth-c 2001:db8:1::1; then fail "IPv6 bypass of the tunnel was not rejected"; fi
ok "bypass of the tunnel is rejected"

ip neigh flush dev veth-c
ip -6 neigh flush dev veth-c
tunnel_works "after neighbor cache flush"
# Wait for a fresh handshake: it must also resolve the endpoint's next hop.
wg set wg0 peer "$(wg show wg0 peers)" persistent-keepalive 1
sleep 2
ip neigh flush dev veth-c
ip -6 neigh flush dev veth-c
sleep 2
tunnel_works "after neighbor cache expiry"
ip -"$FAMILY" neigh show dev veth-c | grep -q -e REACHABLE -e STALE -e DELAY -e PROBE \
    || fail "endpoint next hop was not resolved again (IPv$FAMILY)"
wg set wg0 peer "$(wg show wg0 peers)" persistent-keepalive 0
ok "endpoint reachable after neighbor cache expiry (IPv$FAMILY)"

if [ "$BACKEND" = nft ]; then
    nft -f - <<'EOF'
add table inet admin
add chain inet admin output { type filter hook output priority 10; policy accept; }
add rule inet admin output ip daddr 10.10.0.1 icmp type echo-request drop
EOF
else
    iptables -A OUTPUT -d 10.10.0.1 -p icmp --icmp-type echo-request -j DROP
fi
if ping4 10.10.0.1; then fail "kill switch exceptions overrode an administrator deny rule"; fi
ping6 fd00:10::1 || fail "unrelated tunnel traffic stopped working"
if [ "$BACKEND" = nft ]; then nft delete table inet admin; else iptables -D OUTPUT -d 10.10.0.1 -p icmp --icmp-type echo-request -j DROP; fi
tunnel_works "after removing the deny rule"
ok "existing deny rules still apply"

sed -i 's/^AllowedIPs = .*/&\nPersistentKeepalive = 25/' /etc/wireguard/wg0.conf
"$H" save wg0 < /etc/wireguard/wg0.conf
"$H" sync wg0
[ "$(wg show wg0 fwmark)" = "$mark" ] || fail "sync changed fwmark from $mark to $(wg show wg0 fwmark)"
[ "$(wg show wg0 listen-port)" = "$port" ] || fail "sync changed the listen port"
wg show wg0 persistent-keepalive | grep -q 25 || fail "sync did not apply the edit"
status_is enabled "after live update"
tunnel_works "after live update"
ok "live config update keeps fwmark, port and kill switch"

cp /etc/wireguard/wg0.conf "$WORK/current.conf"
sed 's/^\(\[Interface\]\)$/\1\nFwMark = 4242/' "$WORK/current.conf" > "$WORK/changed.conf"
"$H" save wg0 < "$WORK/changed.conf"
if "$H" sync wg0 2>"$WORK/err"; then fail "sync accepted a FwMark change on a running tunnel"; fi
grep -q reconnect "$WORK/err" || fail "FwMark refusal does not ask for a reconnect: $(cat "$WORK/err")"
[ "$(wg show wg0 fwmark)" = "$mark" ] || fail "refused sync still changed the fwmark"
"$H" save wg0 < "$WORK/current.conf"
ok "a FwMark change on a running tunnel is refused"

wg syncconf wg0 <(wg-quick strip wg0)
if [ "$(wg show wg0 fwmark)" = off ]; then
    status_is mismatch "after a raw syncconf reset the mark"
    "$H" killswitch-repair wg0
    status_is enabled "after repair"
    [ "$(wg show wg0 fwmark)" != off ] || fail "repair left the transport unmarked"
    tunnel_works "after repair"
    ok "raw syncconf mark reset is detected and repaired"
else
    log "note: this wireguard-tools build keeps the fwmark on syncconf"
fi

if [ "$BACKEND" = nft ]; then
    nft delete table inet wg_helper_killswitch
else
    for t in iptables ip6tables; do
        "$t" -D OUTPUT 1
        "$t" -F WGH-KILLSWITCH
        "$t" -X WGH-KILLSWITCH
    done
fi
status_is missing "after a firewall reload"
"$H" killswitch-repair wg0
status_is enabled "after repair"
tunnel_works "after repair"
ok "rules lost to a firewall reload are detected and repaired"

ip link del wg0
if ping4 198.51.100.1; then fail "IPv4 leaked to the underlying network after losing wg0"; fi
if ping6 2001:db8:ff::1; then fail "IPv6 leaked to the underlying network after losing wg0"; fi
status_is mismatch "with the interface gone"
ok "no leak after losing the interface"

"$H" down wg0 2>/dev/null || true
status_is disabled "after down"
no_rules
retry ping4 198.51.100.1 || fail "underlying network not usable after down"
ok "down removes every rule"

cp /etc/wireguard/wg0.conf "$WORK/before.conf"
mount -o remount,size=64k /etc/wireguard
{ cat /etc/wireguard/wg0.conf; head -c 900000 /dev/zero | tr '\0' '#'; echo; } > "$WORK/big.conf"
if "$H" save wg0 < "$WORK/big.conf" 2>"$WORK/err"; then fail "save succeeded on a full disk"; fi
cmp -s /etc/wireguard/wg0.conf "$WORK/before.conf" || fail "full disk corrupted the saved config"
for leftover in /etc/wireguard/.*.tmp.*; do
    [ -e "$leftover" ] && fail "staging file left behind on a full disk: $leftover"
done
ok "full disk during save keeps the previous config"

echo "  $pass checks passed (IPv$FAMILY endpoint, $BACKEND)"
