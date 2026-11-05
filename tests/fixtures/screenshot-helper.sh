#!/bin/sh
# A stand-in for wg-helper used only to render the README screenshots
# (scripts/render-screenshots.sh). It answers the helper verbs with fictional
# tunnels: documentation addresses (RFC 5737 / RFC 3849), example host names
# and keys that spell out that they are not real. Nothing touches the system.
#
# Environment (set by the render script):
#   SHOT_T0         start of the session (epoch seconds); counters grow from it
#   SHOT_KILLSWITCH kill switch status reported for "home" (default: enabled)
#   SHOT_SAVE_FAIL  when non-empty, "save" fails like a read-only /etc
set -eu

verb=${1:-}
name=${2:-}
now=$(date +%s)
t0=${SHOT_T0:-$now}
elapsed_ms=$(($(date +%s%N) / 1000000 - t0 * 1000))

# key LABEL: a syntactically valid, obviously fake WireGuard key.
key() {
	printf '%s' "$1" | awk '{ s = $0; while (length(s) < 42) s = s "0"; printf "%sA=", substr(s, 1, 42) }'
}

# A handshake that keeps renewing like a busy peer: 5-94 seconds ago.
fresh() {
	echo $((now - 5 - now % 90))
}

config() {
	case $1 in
	home)
		cat <<EOF
[Interface]
PrivateKey = $(key EXAMPLE+home+priv+key+not+real)
Address = 192.0.2.10/32, 2001:db8:10::a/128
DNS = 192.0.2.1
ListenPort = 51820

[Peer]
PublicKey = $(key EXAMPLE+home+server+key+not+real)
PresharedKey = $(key EXAMPLE+home+preshared+not+real)
AllowedIPs = 0.0.0.0/0, ::/0
Endpoint = vpn.home.example:51820
PersistentKeepalive = 25
EOF
		;;
	office)
		cat <<EOF
[Interface]
PrivateKey = $(key EXAMPLE+office+priv+key+not+real)
Address = 198.51.100.20/24
DNS = 198.51.100.1
ListenPort = 51821

[Peer]
# gateway
PublicKey = $(key EXAMPLE+office+gateway+not+real)
AllowedIPs = 198.51.100.0/24
Endpoint = gw.office.example:51820
PersistentKeepalive = 25

[Peer]
# printer
PublicKey = $(key EXAMPLE+office+printer+not+real)
AllowedIPs = 198.51.100.40/32
Endpoint = 203.0.113.40:51820

[Peer]
# backup
PublicKey = $(key EXAMPLE+office+backup+not+real)
AllowedIPs = 198.51.100.60/32
Endpoint = 203.0.113.60:51820
PersistentKeepalive = 25
EOF
		;;
	travel)
		cat <<EOF
[Interface]
PrivateKey = $(key EXAMPLE+travel+priv+key+not+real)
Address = 203.0.113.77/32
DNS = 203.0.113.1

[Peer]
PublicKey = $(key EXAMPLE+travel+exit+key+not+real)
AllowedIPs = 0.0.0.0/0
Endpoint = exit.travel.example:443
PersistentKeepalive = 25
EOF
		;;
	lab-v6)
		cat <<EOF
[Interface]
PrivateKey = $(key EXAMPLE+lab+priv+key+not+real)
Address = 2001:db8:42::2/64

[Peer]
PublicKey = $(key EXAMPLE+lab+router+key+not+real)
AllowedIPs = 2001:db8:42::/48
Endpoint = [2001:db8:42::1]:51820
EOF
		;;
	*)
		echo "wg-helper: no tunnel named '$1'" >&2
		exit 1
		;;
	esac
}

# wg show NAME dump: interface line, then one line per peer.
dump() {
	tab=$(printf '\t')
	case $1 in
	home)
		echo "$(key EXAMPLE+home+priv+key+not+real)$tab$(key EXAMPLE+home+publ+key+not+real)${tab}51820${tab}off"
		echo "$(key EXAMPLE+home+server+key+not+real)$tab$(key EXAMPLE+home+preshared+not+real)${tab}192.0.2.200:51820${tab}0.0.0.0/0,::/0$tab$(fresh)$tab$((1331439862 + elapsed_ms * 2516))$tab$((90502103 + elapsed_ms * 319))${tab}25"
		;;
	office)
		echo "$(key EXAMPLE+office+priv+key+not+real)$tab$(key EXAMPLE+office+publ+key+not+real)${tab}51821${tab}off"
		echo "$(key EXAMPLE+office+gateway+not+real)${tab}(none)${tab}198.51.100.2:51820${tab}198.51.100.0/24$tab$(fresh)$tab$((327576371 + elapsed_ms * 47))$tab$((43075420 + elapsed_ms * 12))${tab}25"
		echo "$(key EXAMPLE+office+printer+not+real)${tab}(none)${tab}203.0.113.40:51820${tab}198.51.100.40/32$tab$((t0 - 2710))${tab}90235${tab}105011${tab}off"
		echo "$(key EXAMPLE+office+backup+not+real)${tab}(none)${tab}203.0.113.60:51820${tab}198.51.100.60/32$tab$((t0 - 431))${tab}4225761${tab}13390315${tab}25"
		;;
	*)
		echo "Unable to access interface: No such device" >&2
		exit 1
		;;
	esac
}

case $verb in
list)
	printf '%s\n' home office travel lab-v6
	;;
active)
	echo "home office"
	;;
read)
	config "$name"
	;;
showconf)
	config "$name" | grep -v -e '^Address' -e '^DNS'
	;;
dump)
	dump "$name"
	;;
is-enabled)
	if [ "$name" = home ]; then echo enabled; else echo disabled; fi
	;;
killswitch-status)
	if [ "$name" = home ]; then echo "${SHOT_KILLSWITCH:-enabled}"; else echo disabled; fi
	;;
killswitch-repair)
	echo "nft: Operation not permitted" >&2
	exit 1
	;;
save)
	cat >/dev/null
	if [ -n "${SHOT_SAVE_FAIL:-}" ]; then
		echo "cannot write /etc/wireguard/$name.conf: Read-only file system" >&2
		exit 1
	fi
	;;
log)
	cat <<EOF
Nov 05 09:12:04 wg-helper[2101]: up home
Nov 05 09:12:05 wg-quick[2104]: [#] ip link add home type wireguard
Nov 05 09:12:05 wg-quick[2104]: [#] ip -4 address add 192.0.2.10/32 dev home
Nov 05 09:12:05 wg-quick[2104]: [#] ip link set mtu 1420 up dev home
Nov 05 09:14:31 wg-helper[2188]: up office
Nov 05 09:14:31 wg-quick[2190]: [#] ip -4 address add 198.51.100.20/24 dev office
EOF
	;;
*)
	echo "wg-helper: '$verb' is not available in the screenshot environment" >&2
	exit 1
	;;
esac
