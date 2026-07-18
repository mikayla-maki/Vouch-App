#!/usr/bin/env bash
# Deploy the relay to a single host, no registry: build the image locally
# for linux/amd64, stream it over ssh, and (re)start compose on the host.
#
#   ./scripts/deploy-relay.sh root@relay.example.com
#
# One-time host setup lives in deploy/README.md. Re-running this is the
# whole deploy story: mailbox data survives (it lives in ./data on the
# host, outside the container).
set -euo pipefail

HOST="${1:?usage: deploy-relay.sh user@host}"
REMOTE_DIR="~/vouch-relay"

cd "$(dirname "${BASH_SOURCE[0]}")/.."

echo "==> building image (linux/amd64)"
docker build --platform linux/amd64 -f Dockerfile-relay -t vouch-relay-server .

echo "==> shipping image to ${HOST}"
docker save vouch-relay-server | gzip | ssh "$HOST" "docker load"

echo "==> syncing compose files"
ssh "$HOST" "mkdir -p ${REMOTE_DIR}"
scp deploy/compose.yml deploy/Caddyfile "$HOST":"${REMOTE_DIR}/"

echo "==> restarting"
ssh "$HOST" "cd ${REMOTE_DIR} && docker compose up -d && docker compose ps"

echo "==> deployed"
