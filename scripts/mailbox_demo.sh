#!/usr/bin/env bash
# The store-and-forward demo: Alice publishes to her mailbox and EXITS.
# Bob starts after she's gone, follows her LogId, and receives her claim
# from the relay — the delivery the dumb pairing relay could never do,
# and the whole reason the relay server holds (bounded) state.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

PORT="${VOUCH_DEMO_PORT:-9443}"
URL="ws://127.0.0.1:${PORT}"

RELAY_DIR="$(mktemp -d /tmp/vouch-mailbox-relay.XXXXXX)"
ALICE_DIR="$(mktemp -d /tmp/vouch-mailbox-alice.XXXXXX)"
BOB_DIR="$(mktemp -d /tmp/vouch-mailbox-bob.XXXXXX)"
LOG_DIR="$(mktemp -d /tmp/vouch-mailbox-demo.XXXXXX)"

echo "==> building vouch-relay-server and vouch-node"
cargo build -p vouch-relay-server -p vouch-node >/dev/null

PIDS=()
cleanup() {
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

echo "==> alice publishes to her mailbox, then goes offline"
VOUCH_DATA_DIR="$ALICE_DIR" VOUCH_MAILBOX_URL="$URL" VOUCH_NAME="alice" \
  VOUCH_SEED_CLAIM="Alice's taco place" \
  ./target/debug/vouch-node >"${LOG_DIR}/alice.log" 2>&1 &
ALICE_PID="$!"
PIDS+=("$ALICE_PID")
sleep 3
kill "$ALICE_PID" >/dev/null 2>&1 || true
echo "==> alice is gone"

ALICE_LOG_ID="$(grep -m1 'my log id:' "${LOG_DIR}/alice.log" | sed 's/.*my log id: //')"
echo "==> alice's address was ${ALICE_LOG_ID}"

echo "==> bob starts now, following alice's address"
VOUCH_DATA_DIR="$BOB_DIR" VOUCH_MAILBOX_URL="$URL" VOUCH_NAME="bob" \
  VOUCH_FOLLOW="$ALICE_LOG_ID" \
  ./target/debug/vouch-node >"${LOG_DIR}/bob.log" 2>&1 &
PIDS+=("$!")
sleep 4

echo
echo "--- alice (before she left) ---"
cat "${LOG_DIR}/alice.log"
echo
echo "--- bob (alice was never online with him) ---"
cat "${LOG_DIR}/bob.log"
echo
echo "--- relay ---"
cat "${LOG_DIR}/relay.log"

echo
if grep -q "theirs=1" "${LOG_DIR}/bob.log"; then
  echo "==> STORE-AND-FORWARD WORKS: bob received alice's claim from the relay"
else
  echo "==> FAILED: bob never received alice's claim (see ${LOG_DIR})"
  exit 1
fi
