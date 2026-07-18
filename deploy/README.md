# Hosting the relay

The relay is one process, some SQLite files, and a WebSocket port — a CPU
and a disk. Everything here is the smallest honest version of hosting
that: one DigitalOcean droplet, Docker to sidestep macOS→Linux
cross-compilation, Caddy for automatic TLS, and a registry-free deploy
script that streams the image over ssh.

## One-time setup

1. **Droplet**: cheapest Basic droplet (1 GB is fine — the build happens
   on your Mac, the droplet only runs), Ubuntu LTS. Note the IP.
2. **DNS**: an A record, e.g. `relay.yourdomain.com` → the droplet IP.
   (TLS certs require a name; there's no Let's Encrypt for bare IPs.)
3. **Docker on the droplet**:
   ```sh
   ssh root@relay.yourdomain.com 'curl -fsSL https://get.docker.com | sh'
   ```
4. **Tell Caddy the domain** (this is the only configuration there is):
   ```sh
   ssh root@relay.yourdomain.com 'mkdir -p ~/vouch-relay && echo VOUCH_RELAY_DOMAIN=relay.yourdomain.com > ~/vouch-relay/.env'
   ```

## Deploying (first time and every time after)

```sh
./scripts/deploy-relay.sh root@relay.yourdomain.com
```

Builds the image locally (`Dockerfile-relay` — a server-only workspace, so
none of the GUI's git dependencies are involved), streams it over ssh,
and `docker compose up -d`s the relay + Caddy. Mailbox data lives in
`~/vouch-relay/data` on the host and survives every redeploy; retention
still applies (`VOUCH_RELAY_RETENTION_DAYS`, default 7 — set it in
`compose.yml`'s relay environment if you want a different window).

On Apple Silicon the amd64 build runs under Docker Desktop's Rosetta
virtualization — slower than native, but a few minutes, not tens.

## Pointing clients at it

```sh
VOUCH_MAILBOX_URL=wss://relay.yourdomain.com ./target/debug/vouch
```

The app prints `my address: <hex>` at startup — that's the string you
hand a friend; they put it in `VOUCH_FOLLOW` (comma-separated for several)
and your claims reach them through your mailbox, whether or not you're
online at the time.

## Operating it

```sh
ssh root@relay.yourdomain.com 'cd ~/vouch-relay && docker compose logs -f relay'   # tail
ssh root@relay.yourdomain.com 'cd ~/vouch-relay && docker compose ps'              # status
ssh root@relay.yourdomain.com 'du -sh ~/vouch-relay/data/*'                        # per-mailbox disk
```

Backups are `rsync` of `~/vouch-relay/data` if you ever care — but the
relay is deliberately not the source of truth for anything; every claim
in it also lives on the authoring device, and a lost relay heals by
peers re-publishing (the same reconciliation that handles any cold
catch-up).
