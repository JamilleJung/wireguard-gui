#!/bin/sh
# Render the README screenshots in docs/images/ from the real application:
# a debug build runs under Xvfb against a stand-in helper
# (tests/fixtures/screenshot-helper.sh) that serves fictional tunnels, and
# xdotool drives it to each scene. Nothing on the host is read or changed.
#
# Runs inside the development container only (it installs a few packages and
# puts the stand-in helper at /usr/lib/wireguard-gui/wg-helper):
#   docker run --rm --init -v "$PWD":/src -v wgtarget-shots-wireguard-gui:/src/target \
#     -w /src wgdev scripts/render-screenshots.sh
#
# Usage: scripts/render-screenshots.sh [output-dir]   (default: docs/images)
set -eu

if [ ! -f /.dockerenv ] && [ -z "${WG_SHOTS_IN_CONTAINER:-}" ]; then
	echo "render-screenshots: run this inside the development container (see the header)" >&2
	exit 1
fi

root=$(cd "$(dirname "$0")/.." && pwd)
out=${1:-"$root/docs/images"}
mkdir -p "$out"
out=$(cd "$out" && pwd)

missing=
for tool in Xvfb xdotool optipng import convert; do
	command -v "$tool" >/dev/null 2>&1 || missing=1
done
for lib in libxkbcommon-x11.so.0 libXcursor.so.1 libXi.so.6 libXrandr.so.2; do
	ldconfig -p | grep -q "$lib" || missing=1
done
if [ -n "$missing" ]; then
	apt-get update -qq
	DEBIAN_FRONTEND=noninteractive apt-get install -y -qq --no-install-recommends \
		xvfb xdotool optipng imagemagick libxkbcommon-x11-0 libxcursor1 libxi6 \
		libxrandr2 fonts-dejavu-core fonts-noto-core >/dev/null
fi

cd "$root"
cargo build --locked --bin wireguard-gui
app="$root/target/debug/wireguard-gui"

work=$(mktemp -d)
cleanup() {
	pkill -x wireguard-gui 2>/dev/null || true
	if [ -n "${xvfb:-}" ]; then kill "$xvfb" 2>/dev/null || true; fi
	rm -rf "$work"
}
trap cleanup EXIT INT TERM

# Stand-ins: the helper at its packaged path, and wg / wg-quick on PATH
# (wg pubkey maps the fictional private keys to their public twins).
install -D -m 0755 "$root/tests/fixtures/screenshot-helper.sh" /usr/lib/wireguard-gui/wg-helper
mkdir -p "$work/bin" "$work/home"
cat >"$work/bin/wg" <<'EOF'
#!/bin/sh
case $1 in
pubkey) sed 's/+priv+/+publ+/' ;;
genkey) echo "EXAMPLE+generated+priv+key+not+real0000000A=" ;;
genpsk) echo "EXAMPLE+generated+psk+not+real000000000000A=" ;;
*) echo "wg: not available in the screenshot environment" >&2; exit 1 ;;
esac
EOF
printf '#!/bin/sh\nexit 1\n' >"$work/bin/wg-quick"
chmod 0755 "$work/bin/wg" "$work/bin/wg-quick"

export DISPLAY=:99
Xvfb :99 -screen 0 1600x1000x24 -nolisten tcp >"$work/xvfb.log" 2>&1 &
xvfb=$!
sleep 1

# window_named REGEX: the id of the first visible window whose title matches,
# once it has had a moment to draw.
window_named() {
	tries=0
	until id=$(xdotool search --onlyvisible --name "$1" 2>/dev/null | head -1) && [ -n "$id" ]; do
		tries=$((tries + 1))
		if [ "$tries" -gt 100 ]; then
			echo "render-screenshots: no window matching $1; visible windows:" >&2
			for w in $(xdotool search --onlyvisible --name . 2>/dev/null); do
				echo "  $w $(xdotool getwindowname "$w" 2>/dev/null)" >&2
			done
			pgrep -a wireguard-gui >&2 || echo "  (no wireguard-gui process)" >&2
			cat "$work/app.log" >&2
			exit 1
		fi
		sleep 0.2
	done
	sleep 1
	echo "$id"
}

# launch [VAR=value...] [-- app arguments...]: start a fresh app instance and
# print its main window id once the first live refresh has been drawn.
launch() {
	pkill -x wireguard-gui 2>/dev/null || true
	# Wait for live instances only: an exited one may linger as a zombie when
	# nothing reaps it (run the container with --init to avoid that).
	while ps -eo stat=,comm= | awk '$2 == "wireguard-gui" && $1 !~ /^Z/ { found = 1 } END { exit !found }'; do
		sleep 0.2
	done
	# The single-instance socket must be free too, or the new instance only
	# asks the (gone) old one to raise its window and exits.
	waited=0
	while grep -qa '@wireguard-gui-' /proc/net/unix; do
		waited=$((waited + 1))
		if [ "$waited" -eq 50 ]; then
			echo "render-screenshots: single-instance socket still held by:" >&2
			for fd in /proc/[0-9]*/fd/*; do
				if readlink "$fd" 2>/dev/null | grep -q socket; then echo "$fd"; fi
			done | cut -d/ -f3 | sort -u | while read -r pid; do
				echo "  $pid $(tr '\0' ' ' <"/proc/$pid/cmdline" 2>/dev/null)" >&2
			done
		fi
		sleep 0.2
	done
	vars=
	while [ $# -gt 0 ] && [ "$1" != -- ]; do
		vars="$vars $1"
		shift
	done
	[ $# -gt 0 ] && shift
	# shellcheck disable=SC2086 # $vars is a list of VAR=value words
	env HOME="$work/home" XDG_CONFIG_HOME="$work/home/.config" \
		XDG_STATE_HOME="$work/home/.state" PATH="$work/bin:$PATH" \
		WG_HELPER=/usr/lib/wireguard-gui/wg-helper WG_NO_UPDATE_CHECK=1 \
		NO_AT_BRIDGE=1 SHOT_T0="$(date +%s)" $vars \
		"$app" "$@" >"$work/app.log" 2>&1 &
	window_named '^WireGuard$' >/dev/null
	sleep 6
	# Resolve the window again: the first one can be replaced while starting.
	window=$(window_named '^WireGuard$')
	xdotool mousemove 500 300
	xdotool windowfocus "$window" 2>/dev/null || true
	echo "$window"
}

# capture WINDOW FILE: grab one window, strip metadata, optimise losslessly.
capture() {
	import -window "$1" "$work/raw.png"
	convert "$work/raw.png" -strip "$out/$2"
	optipng -quiet -o5 "$out/$2"
	echo "wrote $out/$2 ($(wc -c <"$out/$2") bytes)"
}


# Main window: home up, healthy peer, live throughput.
main=$(launch)
capture "$main" gui-main.png

# QR dialog for home ("Show QR" in the interface card).
xdotool mousemove 671 313 click 1
capture "$(window_named '^Tunnel QR code$')" gui-qr.png

# Small window.
main=$(launch)
xdotool windowsize "$main" 720 480
sleep 2
capture "$(window_named '^WireGuard$')" gui-small.png

# Editor (Ctrl+E on the selected tunnel).
main=$(launch)
xdotool key ctrl+e
editor=$(window_named '^Edit Tunnel$')
sleep 1
capture "$editor" gui-editor.png

# Failed save: change the keepalive, Ctrl+S, the helper refuses.
main=$(launch SHOT_SAVE_FAIL=1)
xdotool key ctrl+e
editor=$(window_named '^Edit Tunnel$')
sleep 1
xdotool windowfocus "$editor" 2>/dev/null || true
xdotool mousemove 200 422 click 1
sleep 0.3
xdotool key End BackSpace BackSpace
xdotool type 15
xdotool key ctrl+s
sleep 3
capture "$editor" gui-draft.png

# High-contrast theme.
main=$(launch -- --theme high-contrast)
capture "$main" gui-high-contrast.png

# Kill switch rules missing: the app repairs automatically (the stand-in
# refuses), backs off, then gives up and offers Repair now.
main=$(launch SHOT_KILLSWITCH=missing)
sleep 110
capture "$main" gui-killswitch.png
