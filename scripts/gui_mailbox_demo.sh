#!/usr/bin/env bash
# Two real Vouch windows through the mailbox relay server — the hosted
# shape, run locally. Alice publishes to her mailbox; Bob follows her
# address (grabbed from her startup log). Author a rec in Alice's window
# and it lands in Bob's via the relay. Deliberately one-way: follows are
# non-reciprocal in Vouch, and this shows exactly that — nothing Bob
# writes reaches Alice unless she follows him back.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

PORT="${VOUCH_DEMO_PORT:-9443}"
URL="ws://127.0.0.1:${PORT}"

RELAY_DIR="$(mktemp -d /tmp/vouch-gui-mailbox-relay.XXXXXX)"
LOG_DIR="$(mktemp -d /tmp/vouch-gui-mailbox-demo.XXXXXX)"

# Split the screen down the middle (logical points, not physical pixels —
# see gui_sync_demo.sh for the Retina story).
TITLEBAR_HEIGHT=24
BOUNDS="$(osascript -e 'tell application "Finder" to get bounds of window of desktop' 2>/dev/null)"
SCREEN_WIDTH="$(echo "$BOUNDS" | awk -F', ' '{print $3}')"
SCREEN_HEIGHT="$(echo "$BOUNDS" | awk -F', ' '{print $4}')"
if [[ -z "$SCREEN_WIDTH" || -z "$SCREEN_HEIGHT" ]]; then
  SCREEN_WIDTH=1440
  SCREEN_HEIGHT=900
fi
HALF_WIDTH=$((SCREEN_WIDTH / 2))
WINDOW_HEIGHT=$((SCREEN_HEIGHT - TITLEBAR_HEIGHT))

echo "==> building vouch-relay-server and vouch"
cargo build -p vouch-relay-server -p vouch >/dev/null

PIDS=()
cleanup() {
  echo
  echo "==> stopping demo"
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT INT TERM

echo "==> starting relay server on ${URL}"
VOUCH_RELAY_BIND="127.0.0.1:${PORT}" VOUCH_RELAY_DATA_DIR="$RELAY_DIR" \
  ./target/debug/vouch-relay-server >"${LOG_DIR}/relay.log" 2>&1 &
PIDS+=("$!")
sleep 0.5

echo "==> opening alice's window (left half — the publisher)"
VOUCH_EPHEMERAL=1 VOUCH_MAILBOX_URL="$URL" \
  VOUCH_NAME="alice" VOUCH_WINDOW_X=0 VOUCH_WINDOW_Y="$TITLEBAR_HEIGHT" \
  VOUCH_WINDOW_WIDTH="$HALF_WIDTH" VOUCH_WINDOW_HEIGHT="$WINDOW_HEIGHT" \
  ./target/debug/vouch >"${LOG_DIR}/alice.log" 2>&1 &
PIDS+=("$!")

ALICE_ADDR=""
for _ in $(seq 1 20); do
  ALICE_ADDR="$(grep -m1 'my address:' "${LOG_DIR}/alice.log" 2>/dev/null | sed 's/.*my address: //')" || true
  [[ -n "$ALICE_ADDR" ]] && break
  sleep 0.25
done
if [[ -z "$ALICE_ADDR" ]]; then
  echo "==> could not read alice's address from ${LOG_DIR}/alice.log"
  exit 1
fi
echo "==> alice's address: ${ALICE_ADDR}"

echo "==> opening bob's window (right half — follows alice)"
VOUCH_EPHEMERAL=1 VOUCH_MAILBOX_URL="$URL" VOUCH_FOLLOW="$ALICE_ADDR" \
  VOUCH_NAME="bob" VOUCH_WINDOW_X="$HALF_WIDTH" VOUCH_WINDOW_Y="$TITLEBAR_HEIGHT" \
  VOUCH_WINDOW_WIDTH="$HALF_WIDTH" VOUCH_WINDOW_HEIGHT="$WINDOW_HEIGHT" \
  ./target/debug/vouch >"${LOG_DIR}/bob.log" 2>&1 &
PIDS+=("$!")

echo
echo "==> author a recommendation in ALICE's window; it appears in Bob's feed"
echo "==> via the relay's mailbox — even if you quit Alice first and restart Bob"
echo "==> logs: ${LOG_DIR} · press Ctrl-C to stop"
wait
