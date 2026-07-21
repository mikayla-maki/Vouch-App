# Vouch — agent field guide

Read this before touching code. It's the map, the settled doctrine, and the
project's working memory. If you learn something durable while working here
(a gotcha, a decision, a landmine), add it to **Project log** at the bottom —
one dated line, newest first.

## What this is

A local-first, end-to-end-encrypted social recommendations app ("vouch for
things"), built on GPUI (Zed's UI framework). Single-writer append-only logs
sync through a blind mailbox relay; a CRDT-ish fold materializes claims into
recommendations. The public repo is `github.com/mikayla-maki/Vouch-App`; a
live relay runs at `vouch-app.online`. Alpha, shipped, self-updating nightly
DMG on macOS.

## Workspace map

| Crate / dir | What it is |
|---|---|
| `crates/vouch-core` | The engine: claims, storage, fold, sync protocol, e2ee. Portable, no UI deps. |
| `crates/vouch-store` | SQLite persistence backend (`open_peer`), schema in `lib.rs`. |
| `crates/vouch-transport` | WebSocket mailbox client (`connect_mailbox`), rustls. |
| `crates/vouch-relay-server` | The hosted mailbox relay: blind store-and-forward, one mailbox per LogId. |
| `crates/vouch-node` | Headless peer for sync testing (no UI, never decrypts). |
| `src/` | The GPUI app: `app.rs` (root), `feed.rs`, `follows.rs`, `ui/` (modals, sidebar), `auto_update.rs`. |
| `scripts/` | Demos (`gui_mailbox_demo.sh`), bundling (`bundle-mac.sh`), deploy (`deploy-relay.sh`). |

Key vouch-core files: `claim.rs` (wire format: header/auth/body),
`writer.rs` (minting), `store.rs` (ingest + ClaimStore), `storage.rs`
(backend trait), `fold.rs` (materializer), `e2ee.rs` (keys, envelopes,
addresses), `sync/` (sans-io protocol: `session.rs` client half,
`respond.rs` server half, `notify.rs` push frames, `protocol.rs` messages),
`peer.rs` (the actor that owns a database + pipes).

## Core model (things that confuse newcomers)

- **A claim is split**: canonical CBOR header `[version, log_id, body_hash]`
  + authenticity artifact + body bytes. Claim id = BLAKE3(header bytes).
  Identity is (author × content): re-minting identical bytes dedupes.
  A header without its body is a tombstone (redaction keeps existence).
- **Logs are single-writer**: one seed = one log = one account = one
  address = one sharing boundary. "Multiple personas" = multiple Peers.
- **The fold** (`fold.rs`): no minted IDs — a recommendation's identity is
  the connected component of the claim reference graph. Per-field causal
  frontiers (MV-register); conflicts are exposed, not resolved. `edit`
  counts only from the source author's log; `comment` is open to anyone.
- **E2EE**: ALL user content is sealed (`{type:"enc", n, ct}`,
  XChaCha20-Poly1305) — there is no plaintext content path, profiles
  included. Content key `K = HKDF(seed, "vouch content key v1")`.
  **The address IS the capability**: `vouch:` + 128 hex = LogId (routing,
  the only half a relay sees) + content key (reading). Pasting it is the
  grant; follow ⇒ read. The follows list is the feed's keyring
  (`e2ee::keys_for`); decryption happens at read time (`decrypted_view`).
- **Sync is sans-io**: `sync/protocol.rs` messages are the whole wire
  protocol; transports are dumb byte movers. Cursors are pipe-local arrival
  positions; set fingerprints detect drift; ingest is idempotent so
  redelivery is free. A pipe subscribed to log X drops events from other
  logs ("smuggling" check, `session.rs` + `notify.rs`) — provenance is
  enforced by topology, not recorded in storage.
- **The relay is blind and dormant-until-paid**: it stores ciphertext it
  cannot read, materializes a mailbox only after an authenticated publish,
  and GC's by honest TTL (never cursor-driven — cursors know past peers,
  not future followers). Permanence is a property of peers, not relays.

## Settled doctrine (do NOT re-litigate; ask Mikayla if you think you must)

- No plaintext user content on the wire, ever. Only engine vocabulary a
  relay must read pre-key (`redact`) is cleartext.
- Per-claim visibility rejected: granularity is the log. Want a different
  audience, mint a different log.
- Follows/consumption are private: local `follows.json`, never claims.
- Embeds are content, not rows; quoted media routes via the quoter's log.
- No forward secrecy on purpose (durable recommendations ≠ ratchet).
- Relay GC is TTL-only. Transport converges on iroh eventually; direct
  connections and relays must stay isomorphic from the database's view.
- GPUI is the UI framework, including the mobile future. Never suggest
  replacing it.
- CRDT-library adoption deferred; keep-all register semantics live in
  `FieldState.frontier`.

## Claim tiers (built 2026-07-20)

The design (settled in conversation; the working doc was deliberately not
committed), implemented across the stack: deniable-by-default speech. Signatures left
the wire entirely (WIRE_VERSION 2): claims carry
`tag = HMAC-SHA256(K_auth, MAC_DOMAIN ‖ header)` where
`K_auth = HKDF(K, "vouch auth key v1")` (address format unchanged).
`SignedEvent` → `Event {header_bytes, tag, body_bytes}`; `verify()` split
into structural `check()` (ingest/fsck/relay) and keyed `verify_tag()`
(read time, inside `decrypted_view`). Going on the record = an `attest`
claim (`Identity::attest` / `e2ee::verify_attest`; Ed25519 sig inside
ciphertext; binds exact claim id + plaintext content hash; edits past it
are unattested speech). The rec projection collects valid attests
(`Recommendation::attested`, `on_the_record()`, `attested_earlier()`);
detail panel has a "Go on the record" action + badges. Relay publish gate
= deniable DH handshake (`e2ee::publish_challenge/publish_proof/
verify_publish_proof`; hello = LogId‖0x01 → 48-byte challenge → 32-byte
MAC proof; reader sessions can't publish at all).
`connect_mailbox(peer, url, log, publish: Option<Identity>)`. Migration =
alpha wipe: vouch-store `SCHEMA_VERSION=2` via SQLite `user_version`; a
pre-v2 dir is wiped whole on open (claims+sync+blobs; identity.key kept).
Conformance vectors regenerated. Design amendments vs the doc: no `Auth`
enum, no `received_via` column (topology + smuggling check carry
provenance; multi-hop replication of deniable speech deliberately
impossible; attested claims are path-independent), ephemeral tier cut,
revouch/hearsay is client-side content. NOT YET DONE: deploy/wipe of the
production relay at vouch-app.online (old relay speaks the v1 wire).

## Conventions

- **Design first, explicit go.** Mikayla settles design in conversation
  before implementation. "Thoughts?" means assessment only — do not build.
- Never commit or push without being asked; never `git add -A` (stage
  files by name — user deletions in the worktree are theirs). The repo is
  public: committing publishes.
- Comments explain constraints and invariants, not narration. Match the
  existing voice (module docs tell the story; inline comments are sparse
  and load-bearing).
- Tests are behavior-named (`adding_a_follow_decrypts_the_already_synced_backlog`).
  Workspace must stay warning-free.
- macOS has no `timeout(1)`; use background processes + polling. GUI
  demos: `scripts/gui_mailbox_demo.sh`.

## Commands

```sh
cargo test --workspace            # full test suite (~134 tests, fast)
cargo build -p vouch              # the GPUI app
scripts/gui_mailbox_demo.sh       # two windows + local relay, end to end
scripts/deploy-relay.sh           # build + ship relay to vouch-app.online
```

Env wiring for dev instances: `VOUCH_EPHEMERAL=1`, `VOUCH_MAILBOX_URL`,
`VOUCH_FOLLOW` (comma-sep addresses), `VOUCH_NAME`, `VOUCH_WINDOW_{X,Y,WIDTH,HEIGHT}`.

## Project log

Add durable learnings here: one `- YYYY-MM-DD:` line each, newest first.

- 2026-07-20: Field guide created alongside the claim-tiers build (see
  above); capability addresses + E2EE substrate were already shipped and
  live at 1f5b66d.
