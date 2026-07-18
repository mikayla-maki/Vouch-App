#!/usr/bin/env bash
# Launches a relay and two real, ephemeral (in-memory) Vouch windows
# pointed at each other through it. Author a claim in one window and watch
# it show up in the other's feed. Ctrl-C stops everything.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

PORT="${VOUCH_DEMO_PORT:-7777}"
ADDR="127.0.0.1:${PORT}"

# Tile the two windows across the real screen, the way Zed's
# script/zed-local does for collab testing: read the main display's actual
# resolution and split it down the middle, rather than guessing fixed
# pixel offsets that only look right on one screen size.
#
# GPUI window bounds are in logical points, not physical pixels — on a
# Retina display those differ by the backing scale factor (e.g. a
# 3024x1964 panel is a 1512x982 logical screen), so this reads Finder's
# desktop bounds (already in points) rather than system_profiler's
# reported resolution (physical pixels).
TITLEBAR_HEIGHT=24
BOUNDS="$(osascript -e 'tell application "Finder" to get bounds of window of desktop' 2>/dev/null)"
SCREEN_WIDTH="$(echo "$BOUNDS" | awk -F', ' '{print $3}')"
SCREEN_HEIGHT="$(echo "$BOUNDS" | awk -F', ' '{print $4}')"
if [[ -z "$SCREEN_WIDTH" || -z "$SCREEN_HEIGHT" ]]; then
  echo "==> could not detect screen resolution, falling back to 1440x900"
  SCREEN_WIDTH=1440
  SCREEN_HEIGHT=900
fi
HALF_WIDTH=$((SCREEN_WIDTH / 2))
WINDOW_HEIGHT=$((SCREEN_HEIGHT - TITLEBAR_HEIGHT))

echo "==> building vouch-relay and vouch"
cargo build -p vouch-relay -p vouch >/dev/null

PIDS=()
cleanup() {
  echo
  echo "==> stopping demo"
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT INT TERM

echo "==> starting relay on ${ADDR}"
./target/debug/vouch-relay "$ADDR" &
PIDS+=("$!")
sleep 0.3

echo "==> opening alice's window (left half)"
VOUCH_EPHEMERAL=1 VOUCH_RELAY_ADDR="$ADDR" VOUCH_AUTO_FOLLOW=1 \
  VOUCH_NAME="alice" VOUCH_WINDOW_X=0 VOUCH_WINDOW_Y="$TITLEBAR_HEIGHT" \
  VOUCH_WINDOW_WIDTH="$HALF_WIDTH" VOUCH_WINDOW_HEIGHT="$WINDOW_HEIGHT" \
  ./target/debug/vouch &
PIDS+=("$!")
sleep 0.3

echo "==> opening bob's window (right half)"
VOUCH_EPHEMERAL=1 VOUCH_RELAY_ADDR="$ADDR" VOUCH_AUTO_FOLLOW=1 \
  VOUCH_NAME="bob" VOUCH_WINDOW_X="$HALF_WIDTH" VOUCH_WINDOW_Y="$TITLEBAR_HEIGHT" \
  VOUCH_WINDOW_WIDTH="$HALF_WIDTH" VOUCH_WINDOW_HEIGHT="$WINDOW_HEIGHT" \
  ./target/debug/vouch &
PIDS+=("$!")

echo
echo "==> two windows should now be open, side by side: 'Vouch — alice' and 'Vouch — bob'"
echo "==> author a recommendation in one; it should show up in the other's feed shortly after"
echo "==> press Ctrl-C to stop"
wait
