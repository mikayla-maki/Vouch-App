#!/usr/bin/env bash
# Spins up a dumb relay and two headless peers, has each mint a claim, and
# watches them converge over a real TCP connection. Not a test suite — a
# thing to run and watch, to see a sync actually happen.
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

PORT="${VOUCH_DEMO_PORT:-7777}"
ADDR="127.0.0.1:${PORT}"

ALICE_DIR="$(mktemp -d /tmp/vouch-alice.XXXXXX)"
BOB_DIR="$(mktemp -d /tmp/vouch-bob.XXXXXX)"
LOG_DIR="$(mktemp -d /tmp/vouch-sync-demo.XXXXXX)"

echo "==> building vouch-relay and vouch-node"
cargo build -p vouch-relay -p vouch-node >/dev/null

PIDS=()
cleanup() {
  echo "==> stopping demo"
  for pid in "${PIDS[@]:-}"; do
    kill "$pid" >/dev/null 2>&1 || true
  done
}
trap cleanup EXIT INT TERM

echo "==> starting relay on ${ADDR}"
./target/debug/vouch-relay "$ADDR" >"${LOG_DIR}/relay.log" 2>&1 &
PIDS+=("$!")
sleep 0.3

echo "==> starting alice (${ALICE_DIR})"
VOUCH_DATA_DIR="$ALICE_DIR" VOUCH_RELAY_ADDR="$ADDR" VOUCH_NAME="alice" \
  VOUCH_AUTO_FOLLOW=1 VOUCH_SEED_CLAIM="Alice's taco place" \
  ./target/debug/vouch-node >"${LOG_DIR}/alice.log" 2>&1 &
PIDS+=("$!")
sleep 0.3

echo "==> starting bob (${BOB_DIR})"
VOUCH_DATA_DIR="$BOB_DIR" VOUCH_RELAY_ADDR="$ADDR" VOUCH_NAME="bob" \
  VOUCH_AUTO_FOLLOW=1 VOUCH_SEED_CLAIM="Bob's ramen spot" \
  ./target/debug/vouch-node >"${LOG_DIR}/bob.log" 2>&1 &
PIDS+=("$!")

echo "==> logs: ${LOG_DIR}"
echo "==> waiting for convergence (8s)"
sleep 8

echo
echo "--- alice ---"
cat "${LOG_DIR}/alice.log"
echo
echo "--- bob ---"
cat "${LOG_DIR}/bob.log"
echo
echo "--- relay ---"
cat "${LOG_DIR}/relay.log"

echo
echo "==> alice: ${ALICE_DIR}/claims.db"
echo "==> bob:   ${BOB_DIR}/claims.db"
