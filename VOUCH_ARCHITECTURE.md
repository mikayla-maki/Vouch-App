# Vouch: Architecture & Vision

**Distributed, privacy-preserving recommendations from people you trust.**

Vouch is three things, deliberately layered:

1. **An engine** — a small sync library for single-writer, signed, append-only
   logs of claims. Publish your log, subscribe to others', and merge any
   number of logs into one reactive, queryable claim graph.
2. **A vocabulary** — a short normative spec of well-known claim types, field
   names, and link relations that give the graph meaning.
3. **An app** — a recommendations-and-warnings client ("distributed Yelp, but
   only people you trust") built on both, targeting desktop and mobile via
   GPUI.

The app is the product, and it drives every design decision. The engine is the
part designed to outlive it: a focused tool that happens to be reusable, not a
framework hunting for use cases.

## The idea in one paragraph

Every user broadcasts an event log. A subscription is a full sync of someone
else's log. Because all logs share one format, a client can merge its N
subscribed logs into the database you'd have if everyone had written into it
together — and the UI is a reactive projection of that merge. Recommendations
are low-volume, low-size data, so full replication of each subscribed log is
not just feasible but the simplest correct design.

## Why this stays simple: single-writer

Each log has exactly one author. This is the load-bearing simplification, and
nearly every other design choice exists to protect it:

- **No merge conflicts.** Two people never edit the same object. Merging N
  logs is a set union, not conflict resolution.
- **No CRDTs, no consensus.** Identity is the content hash, so there is
  nothing to vote on and nothing to converge except "have I seen this claim
  yet?" — set union is the whole merge.
- **Trivial sync.** "Send me what I don't have" — by pipe-local arrival
  cursor ("I have N of yours; send the rest"), confirmed by a per-log set
  fingerprint. Cursors are hints between cooperating clients; the
  fingerprint check is what's relied on.

The temptation as the engine generalizes will be multi-writer logs. Resist it.
Multi-writer is where coordinators, epochs, and consensus live (see n0pe's
architecture for what that costs); Vouch's domain doesn't need it.

### Inspirations

- [LiveStore](https://livestore.dev/) — the store pipeline: commit event →
  materialize into SQLite → reactive queries. Vouch extends it from one log to
  N merged logs.
- [Secure Scuttlebutt](https://handbook.scuttlebutt.nz/concepts/identity),
  [Nostr](https://github.com/nostr-protocol/nips), and
  [AT Protocol](https://atproto.com/) — three independent systems that all
  converged on the same data shape Vouch uses: one record type with a type
  hint and links, optimistically parsed, well-known types driving the UI.
- [Petnames](https://files.spritely.institute/papers/petnames.html) — humane
  decentralized naming, applied here to both logs and entities.
- [Signal](https://signal.org/docs/) — identity-is-a-keypair, TOFU, and (later)
  the multi-device model.
- [mitchellh/vouch](https://github.com/mitchellh/vouch) — proof that "personal
  attestation lists, merged by consumers" is a primitive worth building well.
  Used here as a litmus test, not a target (see Non-Goals).

## Core Model

### Log = Identity = Keypair; Database = the merge

The network unit is the **log**: one keypair, one writer, one append-only
sequence of claims. There is no separate "user" at the network level — a
log IS an identity.

- `LogId` is the Ed25519 public key itself.
- Alice's "Food Recs" and Alice's "Sports Takes" are unlinkable identities on
  the network, connected only inside Alice's app because she holds both keys.

The **database** is what a client builds locally: the merge of its N
subscribed logs — the database you'd have if everyone had written into it
together. You subscribe to *logs*; merging them gives you your *database*.

```rust
/// A log's identity: the Ed25519 public key itself.
struct LogId(PublicKey);

/// The local composition: N merged logs, their media, and the writers for
/// the logs you own — one door in (ingest, from any pipe), a query surface
/// out, minting in between. Pure state; the sync engine is this plus
/// pipes; a relay is this with no writers.
struct Database {
    claims: ClaimStore,
    blobs: BlobStore,
    writers: Map<LogId, Writer>,
}
```

### Claims: one shape for everything

Every entry in a log is a **claim**: a header, a dynamically-typed body, and a
signature. There is no closed enum of event kinds. Recommendations, warnings,
entities, edits, disavowals, profile updates, merges — all are claims,
distinguished by convention (the vocabulary), not by structure.

```rust
/// A claim's identity: the BLAKE3 hash of its canonical header bytes.
/// Identity is content — there is nothing two different claims can
/// collide on, so forks are not a concept in this system.
struct ClaimHash([u8; 32]);

struct EventHeader {
    /// Wire-format version. Structural changes to the signed layout bump
    /// this; new claim types and fields never do.
    version: u16,
    log_id: LogId,
    /// Hash of the canonical body bytes — the pin that lets a body detach.
    body_hash: Hash,
}
// That's the whole header: a version to decode by, a key to verify with,
// a hash to pin the content. Everything the author MEANS — including when
// they claim they said it (the vocabulary's `at` field) — lives in the
// body, transitively signed via body_hash, and redacted with it: a
// tombstone reveals nothing but "this key once signed something with this
// hash". Not even when. There is no sequence number and no prev pointer;
// sync coordinates are pipe-local arrival counts each store keeps about
// itself.
//
// Identity is therefore exactly (author × content): byte-identical bodies
// from one author are ONE claim — saying the same thing twice is saying
// it once. Corollary: redaction retires those exact bytes from that
// author for good; "republishing" is new speech (a superseding claim
// with a fresh `at`) under a new identity.

/// A claim as transmitted and stored. The signature covers the HEADER; the
/// header pins the BODY by hash. So a body can be dropped (redaction) while
/// the claim's existence, position, and authorship stay verifiable forever:
/// a header without its body is a *signed tombstone*.
struct SignedEvent {
    header_bytes: Bytes,
    signature: Signature,
    body_bytes: Option<Bytes>,  // None = tombstone
}
```

**Links are values, not fields.** The body is fully freeform. Instead of
reserving keys, the wire format defines three well-known CBOR-tagged value
types that may appear *anywhere* in a body — a top-level field, a list
entry, a span target inside rich text:

| Tagged value | Content | Purpose |
| ------------ | ------- | ------- |
| `ClaimRef` | `(LogId, ClaimHash)` | an edge to another claim; meaning given by context |
| `Embed`    | `SignedEvent` | a rehosted original, signature-verified by the engine |
| `BlobRef`  | `(BlobHash, size, mime)` | bulk bytes (images, media) pinned by hash, stored and fetched outside the claim |

(`type` is an ordinary body key — `"rec"`, `"warning"`, `"entity"`, ... — a
vocabulary convention, not a structural requirement.)

Rules:

1. **A body MUST be a CBOR map.** The only structural requirement. The store
   indexes links by walking every body's value tree and collecting tagged
   values with their paths — so forward and backward references are indexed
   for *any* claim, including types the client has never seen. A link's
   meaning (its "rel") comes from where it sits — the field name or structure
   around it — which is vocabulary, not wire format.
2. **Tagged values are validated leniently.** A malformed `ClaimRef` drops
   out of the index; the claim itself is stored and re-gossiped regardless.
   Signed bytes are never discarded (see Forward Compatibility).
3. **`Embed` is the engine's business — and embeds are content, not rows.**
   Verifying an embedded author's signature is a byte-level concern no
   vocabulary can express, so the engine verifies embeds in place, wherever
   in a body they appear. A quote is part of the speech that carries it: it
   is never extracted into a store row of its own. Its edges — the embed
   itself (a quote is the strongest form of reference), plus every ref and
   blob inside it — index under the *quoting* claim, and readers recurse
   into the quote (`StoredClaim::embeds()`) to render it. Store rows are
   exactly the top-level events your logs delivered.

### The vocabulary

The engine moves bytes and indexes edges; the vocabulary is where meaning
lives. It is a short normative document with the same status as the wire
format spec, shipped with conformance vectors. The starter set:

Every claim type carries `at` (claimed creation time, Unix ms) — the
engine reads it leniently for display ordering, like `type`/`redacts`.
Because `at` lives in the body it is transitively signed, redacted along
with the content, and makes otherwise-identical re-posts distinct claims.

| `type` | Well-known fields (refs are `ClaimRef` values) |
| ------ | ---------------------------------------------- |
| `rec`       | `subject`, `body`, `location?`, `photo?`, `about?: ClaimRef` → entity |
| `warning`   | same as `rec`, plus `regarding?: ClaimRef` → any claim |
| `entity`    | `name`, `description?`, `location?`, `photo?`, `same-as?: [ClaimRef]` |
| `edit`      | replacement fields, `supersedes: ClaimRef` (own log only) |
| `disavowal` | `body?` (the reason), `disavows: ClaimRef` |
| `vouch`     | `body?` (commentary), `original: Embed` |
| `profile`   | `name`, `description?` — self-description of this log |

The vocabulary also defines well-known *value* shapes, not just claim types —
notably a rich-text value whose styled spans can carry inline `ClaimRef`
targets, so mentions of entities and other claims flow inside prose. The
indexer finds them there the same as anywhere else.

Two properties of this design do real work:

- **Commentary is free everywhere.** A disavowal with a reason, a merge with
  an explanation, a vouch with a note — in a closed-enum design each of these
  forces the enum to grow; here they're just body text on a claim whose links
  carry the semantics.
- **Unknown types degrade gracefully.** A claim type you don't recognize still
  stores, syncs, indexes its links, and renders generically (its fields and
  its edges). Well-known types get hand-built UI; everything else gets the
  generic renderer. New vocabulary deploys without breaking old clients.

The discipline that keeps this from becoming RDF-style schema soup: the
vocabulary stays small, versioned, and normative. Named patterns are
hand-coded in the app; there is no generic graph-query engine in V1.

### Entities and aliases

An entity (a person, business, or place being recommended) is itself a claim
in someone's log — there is no global registry. Recs and warnings link
`about` → an entity claim. When two entities turn out to be the same thing
(Alice's "Joe's Pizza", Bob's "Joes pizza on 5th"), *you* publish or locally
record a `same-as` claim — entity resolution is personal and local, the
petname model applied to subjects. Your merge is yours; nobody has to agree.

### Identity of a claim and cross-path dedup

A claim's canonical identity is its `ClaimHash`; a `ClaimRef` pairs it with
the `LogId` so referenced claims are locatable, not just nameable.
Rehosted copies carry the embedded original, so the same rec seen via three
paths (the author directly, plus two friends' vouches) agrees on one
*identity* with three endorsements: every path hashes to the same id, the
quotes backlink it, and the UI deduplicates by id at render — and a
disavowal of the original matches all three paths for the same reason.

### Vouch semantics

Vouching is both an endorsement AND a durability decision:

| Action        | Meaning                       | Where it lives | Durability    |
| ------------- | ----------------------------- | -------------- | ------------- |
| **Subscribe** | "I want to see this"          | Their log      | Theirs        |
| **Vouch**     | "I endorse this AND host it"  | Your log       | Yours         |

Verification chain: Alice signs a rec → Bob vouches it (embedding her signed
bytes) → Carol, subscribed only to Bob, verifies Bob's signature on the vouch
*and* Alice's signature on the embedded original. Provenance is cryptographic
the whole way down; no trust in intermediaries required.

### Convergence invariants

The projection must be a pure, order-insensitive fold over the union of
claims: any two clients holding the same claim set render identical state,
regardless of arrival order.

The claim-graph model makes this structural rather than clever:

- **Ingest interprets nothing.** Claims are stored as received; semantics are
  resolved at query time by following links.
- **Dangling edges heal.** A link to a claim that hasn't arrived yet is just
  an unresolved edge; when the target arrives, every query that follows the
  link sees it. No tombstone special-casing.
- **Display order is not log order.** The UI sorts by the bodies' claimed
  times — `(at, log_id, id)`, with `at` an engine-recognized optional body
  key read leniently like `type`/`redacts` — deterministic across clients,
  but author-claimed, so this order is cosmetic. Correctness never depends
  on it. Each store also records local `received_at` (clock injected by
  the engine; core reads no clocks) and `arrival` order — local metadata
  for "new since you last looked" and cursors, never part of state.
- **Local metadata is not state.** Convergent state is exactly the
  replicated substance: headers, bodies, redactions. Which valid signature a
  store holds (an author can mint many; each is equal proof), arrival order,
  and receive times are local facts that id-based sync can never
  reconcile — so they are defined out of the state surface rather than
  papered over with tiebreaks.

This invariant is the engine's contract and gets enforced by property tests:
shuffled replay of any claim set must produce a byte-identical projection.

### No forks: identity is content

There are no slots for two claims to fight over. A claim's identity is the
hash of its signed header, so "two different claims" is the *whole* analysis
— both store, both are true facts ("this key signed this"), and the engine
needs no detection, no branching, no tiebreaks, and no policy. The classic
fork scenario (two devices restored from one mnemonic, both writing) just
produces two ordinary claims in one log. A writer carries no position at
all — nothing to restore, nothing to collide on. Nothing breaks; nothing
needs the user's attention.

**Ordering is never signed.** There is no sequence number and no `prev`
chain in the header — the signature covers *speech, not plumbing*: who said
it, when they claim they said it, what they said. Sync coordinates live
where the knowledge actually is: each store keeps its own **arrival
order** — a claim's position is simply the count of that log's claims the
store held when it landed ("the sequence number is always the count").
This is local metadata: recorded in the claim
row in the same transaction as the insert (so it's atomic, durable, and
rolls back with it), excluded from state vectors and fingerprints,
meaningless to any other store. A **cursor** is then just "how many of
this log's claims I've received from this pipe," and serving is "skip that
many in my order, send the rest." This works because both sides are
monotone: stores never delete rows (tombstones keep them), and engines
advance cursors only after a successful ingest. atproto walked the same
road in two steps (repo v3 dropped `prev`; the firehose sequence is
relay-assigned, not author-signed) — we just took it to the end.

### Drift detection: trust the cursor, verify the set

A cursor can't see *silent* divergence when its monotonicity assumption
breaks behind its back. Authors can no longer cause this (they assign no
numbers), but a pipe can: a relay dies, restores from yesterday's backup —
losing a claim a client already received — then ingests new claims at the
recycled arrival positions. The client pulls past its cursor, gets only
the newest claim, and now both sides hold the same *count* — every
cursor-shaped signal agrees — while the client holds a claim the relay
lost and is missing one it skipped. The proxy ("are the counts aligned")
says yes; the invariant ("do we hold the same set") says no. (Transports
can also cheaply prevent stale cursors: a relay mints an instance id at
boot, a cursor is `(instance, count)`, and an instance mismatch resets the
cursor to zero — dedup makes the re-download harmless.)

So state itself gets a cheap check. Each store maintains, per subscribed
log, an order-independent **fingerprint**: the XOR of one BLAKE3 digest per
claim, where each digest commits to the claim hash, whether the body is held
("have" means "have the body"), and any redaction applied. Two honest stores
agree on a log's fingerprint exactly when they agree on that log's
convergent state. The catch-up handshake is then:

1. **Fast path.** Pull by cursor: "I have N of this log from you — send
   the rest."
2. **Confirm.** "Thanks — by the way, here's my fingerprint for this log."
3. **Reconcile on mismatch.** Fall back to full set reconciliation —
   exchange claim-hash lists, union up. A full sync may take a hot second,
   but it's resumable and interruptible like everything else: it's just
   claims arriving, in any order, tracked by the same cursors.

(One known soft spot, by design: body fill-in updates an existing row, so
a cursor client that already passed it doesn't get the body on the fast
path — the fingerprint's body-presence bit catches it and reconciliation
heals it. Author-assigned sequences had the identical behavior.)

Like the cursor, the fingerprint is advisory — XOR-of-digests detects
drift between cooperating clients; it is not a defense against liars, and
we know nothing at this layer is. Equivocation (deliberately serving
different people different subsets) remains *observable* — peers who
compare fingerprints out-of-band see the mismatch — and the claims
themselves can never conflict: evidence for app-layer judgment, not
engine state.

## Canonical Serialization & Wire Format

Signatures are computed over encoded bytes, so the wire format is the real
cross-language contract: every client implementation (Rust, Swift, Kotlin, ...)
must produce byte-identical encodings for the same claim. This section is
normative for all implementations.

### Canonical encoding

All signed structures are encoded with **deterministic CBOR** (RFC 8949 §4.2
core deterministic encoding): definite-length containers, shortest-form
integers, map keys sorted bytewise. CBOR over a Rust-native format (e.g.
postcard) because mature implementations exist in every target language — and
because the dynamically-typed claim body is natively a CBOR map.

```text
SIGNING_DOMAIN = "vouch-claim-sig-v1"            // domain separation
signature = Ed25519::sign(signing_key, SIGNING_DOMAIN ++ canonical_header_bytes)
id        = BLAKE3(canonical_header_bytes)        // a hash needs no domain
body_hash = BLAKE3(canonical_body_bytes)          // pinned inside the header
```

**Rules:**

1. **Sign bytes, verify bytes.** Verifiers MUST check the signature against
   the bytes as received (prefixed with `SIGNING_DOMAIN`), and only then
   decode. Never decode → re-encode → verify; round-tripping is where
   canonicalization bugs hide.
2. **Domain-separate the signature.** The signed message is
   `SIGNING_DOMAIN ++ header_bytes`, never the header alone, so a claim
   signature can't be replayed as a valid signature over another protocol's
   message under a reused key. The id is a plain hash and needs no domain.
3. **Store the original bytes.** Claims persist with their received encoding
   alongside the decoded form, so any claim can be re-transmitted or
   re-verified byte-for-byte. This is also what makes vouching verifiable.
4. **Bodies are capped at 64 KiB.** A normative rule, not a courtesy —
   stores must agree on which claims are valid or their fingerprints diverge.
   Writers refuse to sign past the cap; verifiers refuse to accept past it.
   ~64 KiB is roughly ten thousand words: a short story per claim, or
   thousands of refs. A body is one piece of speech; bulk data (images,
   media) is pinned from the body by hash as content-addressed blobs, which
   is what keeps logs small enough to full-sync and hold in memory.
5. **Sign only what decodes.** Encoding is total but decoding is strict
   (depth cap, integers bounded to i64, canonical form). Writers round-trip
   a body through the decoder before signing, so they can never mint a
   permanently-unverifiable claim (an over-deep body, a `BlobRef` size past
   i64::MAX).

### Envelope / payload split

Transports see two layers, kept separate from day one even while payloads are
plaintext:

- **Envelope**: `log_id` plus whatever advisory hints a transport wants
  (arrival cursors, per-log counts, fingerprints) — the minimum needed for
  routing and incremental sync.
- **Payload**: the canonical bytes of the `SignedEvent` — opaque to all
  transports. Links live inside the payload, so once E2EE lands, the
  relationship graph is as private as the content; all link indexing is
  client-side, after decryption and verification.

When E2EE lands, encryption is a payload transform; the envelope, the sync
protocol, and every transport implementation are unchanged.

### Forward compatibility

A client that cannot interpret a claim (unknown `type`, unrecognized fields,
malformed tagged values) MUST retain and re-gossip the raw bytes rather than
drop it. Rehosting and convergence depend on old clients not silently
discarding data they can't read. In the claim-graph model this is the common
path, not the exception: unknown claims still index their links and render
generically.

### Conformance test vectors

The spec ships with test vectors: fixed keypairs, claims, their canonical byte
encodings, and signatures — plus vocabulary vectors (well-formed and malformed
tagged values in assorted body positions, and the expected lenient-validation
and indexing outcomes). A client
implementation in any language validates against the vectors before anything
else. The vectors are the cheapest durable artifact for keeping N
implementations honest — far cheaper than FFI bindings — and double as
regression tests for the Rust reference implementation.

## Storage & Reactivity

The local store follows the LiveStore pipeline, extended to N logs:

```text
commit claim → append to log (SQLite) → index links → materialize → notify queries → sync
```

- **The claim log is the source of truth.** SQLite tables hold every claim
  (original bytes + decoded columns) from your logs and subscriptions.
- **The link index is generic.** Forward and backward edges are extracted by
  walking every body's value tree for tagged `ClaimRef`s — known claim type or
  not, top-level field or inline rich-text span. "Show all claims referencing
  this one" is a store primitive, not a vocabulary feature.
- **Materializers are vocabulary-aware projections** — pure functions from
  claims to queryable tables (recs, warnings, entities with resolved aliases,
  endorsement counts, naming). Views are disposable: any of them can be
  rebuilt by refolding the log. At "reviews from people you know" scale a full
  refold is milliseconds — no incremental view-maintenance machinery
  (differential dataflow et al.) is warranted.
- **Reactive queries** subscribe to table-change notifications; the UI never
  polls and never shows a loading state for local data.

**Position: storage is a trait cut UNDER the invariants, never at them.**
vouch-core defines two storage seams, and both follow the same discipline —
backends store dumb bytes/rows, the logic that owns the invariants is
written exactly once in core and drives whichever backend it's given:

- `ClaimStorage` (rows + indexes: claims, backlinks, blob referrers,
  redactions) under `ClaimStore`, which owns ALL convergence logic —
  monotone redaction, seen-is-applied, body fill-in, fingerprint semantics.
  Backends: memory (tests, simulations) and **SQLite (vouch-store) — the
  primary target**, since the app is a mobile app: durable, transactional,
  lazy, and redaction's body-drop is a column update, so cooperative
  deletion reaches the disk with zero compaction machinery.
- `BlobStorage` (content-addressed bytes) under `BlobStore`, which owns
  hashing and verify-on-arrival. Not provided trait methods — those are
  overridable, so a backend could shadow the check; verification lives in
  the concrete struct where backends can't reach it. Backends: memory and
  hash-named files (vouch-store).

What is deliberately NOT a trait: `ClaimStore` itself. A trait there would
invite N implementations of the most dangerous code in the system; the seam
below it gives backends nothing to get wrong but storage.

**Robustness against crashes and buggy backends** (the failure class is
logical inconsistency, never UB — there is no unsafe code):

- *Self-authenticating rows.* Every stored artifact carries its signature
  and hash pins, so a backend cannot lie about content — only lose it,
  which sync heals. `verify_integrity()` is the fsck: re-verify everything,
  cross-check index edges both directions.
- *Transactions are required backend API* (`begin/commit/rollback` — no
  defaults; a backend without atomicity must write its no-ops out loud).
  Both shipped backends are atomic: SQLite via its journal, memory via an
  undo log. One ingest (edge indexing included) is one transaction — a crash,
  kill, or power loss mid-ingest persists nothing, and a failed ingest
  *never happened* (pinned by a zero-debris fault-injection sweep). SQLite
  runs `synchronous=FULL` so a *committed* ingest survives power loss too,
  not just a process/OS crash — losing the tail otherwise would both lose a
  user's own writes and silently rewind the store's arrival count below a
  peer's cursor (the relay-restored-from-stale-backup hazard).
- *Commit-point ordering* covers backends WITHOUT transactions: `put_claim`
  lands last and every earlier write is an idempotent upsert, so a partial
  ingest plus at-least-once redelivery converges exactly. This is pinned by
  an exhaustive fault-injection test (fail at write N, for every N, then
  redeliver and demand state equality with a never-crashed control).
- *Panic poisoning*, like `Mutex`: a panic unwinding mid-ingest marks the
  store poisoned and every later call fails loudly, so caught panics can't
  observe half-applied state.
- *The network gets the same treatment* (see Sync & Transports): the sync
  engine is sans-io, so dropped connections, duplicated frames, lying
  pipes, and sessions killed at every message boundary are all plain
  deterministic tests in core::sync — same exhaustive-prefix trick as the
  storage crash sweep, lifted to messages.

Conceptually there are **two SQLite databases**: the engine's (claim
storage, owned by vouch-core's schema via vouch-store) and the app's
(vocabulary projections, FTS, view models — disposable, rebuildable). Same
file via attached databases or separate files — upstream's choice: storage
location and backend are always injected from above
(`Database::with_stores(...)`), never owned by the engine.

The storage *format* is generic for free: the wire format defines what a
serialized log is (a CBOR sequence of signed events, RFC 8742), so
export/import of a log as a single file falls out of the spec — and a file
is just another pipe (see Transports).

## Sync & Transports

### The engine is sans-io; everything is a pipe (core::sync, BUILT)

The same cut as storage, applied to the network: *logic under invariants,
dumb plumbing at the edge*. The protocol is plain data — `Request` and
`Response` enums in `core::sync` ARE the wire format. The engine is a pure
state machine (`SyncSession`) that produces requests and consumes
responses; the server half is one stateless function
(`respond(db, instance, now, request)`). A **transport** is anything that
moves a `Request` to a peer and brings a `Response` back — an HTTP client,
an iroh stream, a function call into another `Database` in the same
process — and carries no protocol logic to get wrong:

```rust
let mut session = SyncSession::new(&mut db, &mut cursors, "relay", now, pull, push);
while let Some(request) = session.next_request() {
    let response = transport.exchange(request)?; // the ONLY I/O in sync
    session.feed(response)?;
}
```

No async runtime, no clock, no sockets in the engine crate: GPUI's executor
drives it in the app, tokio will drive it in the relay, a `while let` loop
drives it in tests — which is why every network fault is a plain
deterministic unit test (see Robustness below).

The message vocabulary (all answerable from a `Database` alone — peers hold
no conversation state):

```text
Status { log }            → count, fingerprint, instance      (open + settle)
Since { log, have, max }  → events                            (cursor pull, paged)
Hashes { log }            → [(id, has_body)]                  (reconciliation)
Claims { ids }            → events                            (reconciliation fetch)
Publish { events }        → ack { stored, rejected }          (idempotent push)
GetBlob { hash }          → bytes?                            (THE media transfer: pull)
PutBlob { bytes }         → ack                               (fast-track: pre-answered pull)
```

One session, per log: **Status** (an unfamiliar `instance` means the peer's
arrival order was reborn — reset cursors, re-pull, dedup flattens it) →
**pull** `Since` batches → **push** `Publish` from our own arrival order →
**settle**: fingerprint match means done; a mismatch equal to the
remembered `settled` fingerprint is the *known benign difference* (claims
we hold that this peer won't take — e.g. a relay only the owner may
publish to) and is skipped; anything else reconciles via `Hashes`/`Claims`,
healing stripped bodies in both directions. An idle re-sync is exactly one
message per log: the settle rides the opening `Status` answer.

**Sessions never carry media.** Blobs are non-syncing by default: a claim
arrives, its `BlobRef` becomes a want, and the want sits until something
asks — the UI demanding a render (`fetch_blob` → one `GetBlob`), an
eager-media pipe taking the photos while a p2p friend is reachable, or a
relay keeping itself stocked (eager by role: claims land via `Publish`,
it pulls their media back up the same duct — which is also how it
restocks bytes it culled under storage pressure). `PutBlob` survives as
the fast-track, exactly as `Notify` is for claims: conceptually still
pull-based, the holder just answers the `GetBlob` it knows is coming (a
p2p mint is two frames — the claim, then its bytes). Advisory and
droppable: the receiver accepts iff its own want-list asks, so the pull
path is the only thing correctness rests on.

The engine's only persistent state is one cursor row per `(peer, log)`:
`(instance, pull, push, settled)` — behind a `SyncState` trait (memory in
tests; `SqliteSyncState` at `dir/sync.db` in vouch-store, which also mints
and keeps the database's own `InstanceId`: durable claims run
`synchronous=FULL`, so arrival order — and therefore the instance — is a
property of the files, not the process). Losing sync.db is always safe:
zero cursors mean a full re-pull that dedup flattens.

Authentication is deliberately not in the protocol. A relay restricting who
may `Publish`/`PutBlob` (signature challenge: nonce signed by the `LogId`
key) enforces it around `respond()`, at the transport layer. Between
friends, gossiping third-party logs is legitimate; published garbage fails
verification at ingest, which is the real gate.

### Push: a fat hint, not a second protocol

Real-time delivery is layered *on* the coordinates above, never woven into
the session machinery. When a `Publish` lands, a sender with a live channel
(WebSocket, SSE, iroh stream) fans out a `Notify` frame — **an unsolicited
`Status` with the new events attached**:

```text
Notify { log, events, count, fingerprint, instance }
```

Applying it (`apply_notify`) is "ingest, then settle", and the outcome
ladder only ever degrades gracefully:

1. **Fingerprints equal** → we provably hold the sender's exact set; the
   pull cursor fast-forwards to `count`. The claim is on screen with
   **zero round trips** — redactions included, so takedowns land at push
   speed.
2. **Matches the remembered `settled` fingerprint** → the known benign
   difference (we hold extras the sender won't take), kept fresh through
   pushes *homomorphically*: the fingerprint is an XOR fold, so the
   sender's new fingerprint is computable from the cached one plus the
   pushed claim — still zero round trips, and the cursor fast-forwards
   here too (a cache match proves the sender's set is one we reconciled to
   and then tracked: a subset of ours).
3. **Anything else** → the frame degrades into a doorbell with a free
   claim attached: hold what was pushed, run an ordinary session.

Safety needs no new machinery: events verify at ingest (a forged frame
can't poison, only be rejected and counted), the coordinates are advisory
(the cursor moves only on fingerprint match; the next session's settle
catches real divergence), and a wrong homomorphic guess just fails the
equality check and costs one session. A `Notify` with no events is a
heartbeat — a cheap anti-entropy ping that confirms an idle subscriber is
still settled, or tells it to ring the doorbell. Media is deliberately not
pushed: a claim pinning a blob lands as content plus a want
(`missing_blobs` in the report); the bytes come by `GetBlob` from any
pipe. **Correctness never depends on the push channel; it only makes the
pull path's answer arrive sooner.**

Planned transports, in order:

1. **Relay** — a dumb store-and-forward server: a peer with no pen, no
   follows, and `ServePolicy::Everything`, behind an HTTP/WebSocket shim
   that shuttles `PipeMsg`s per connection, plus owner auth on publishes.
   Fetching requires no auth; watch state is per-connection and ephemeral.
2. **iroh p2p** — [iroh](https://github.com/n0-computer/iroh)'s
   dial-by-public-key QUIC maps directly onto `LogId`-is-a-pubkey, and an
   iroh relay node is literally "the relay as just another pipe." Strong
   candidate to be the relay's implementation substrate rather than a
   separate transport; decided by prototyping.
3. **Files** — a serialized log is a valid transport: backups, sneakernet,
   attach-your-log-to-an-email.

### Sync invariants

- **Abandoning a session anywhere is free.** Cursors advance only after the
  data they describe has ingested; peers hold no conversation state; ingest
  is idempotent. The recovery story for every crash, timeout, and
  disconnect is the same: start a fresh session. Pinned by fault injection:
  a scenario exercising every phase is killed after every possible message
  prefix, resumed, and must converge bit-identically with a clean fsck.
- **`next_request()` is an idempotent peek** — a transport may retry the
  same request after a timeout without confusing the session.
- **A pipe cannot smuggle logs you didn't subscribe to**: top-level events
  outside the asked-for log are dropped and counted (`off_plan`). Embedded
  claims still ride inside subscribed claims — quoting is the author's
  speech.
- **Peer misbehavior never aborts a session** (it's counted in the
  `SyncReport`: rejected events, corrupt blobs, off-plan artifacts); only
  *local* storage failure does. A want never expires — a blob rejected from
  one pipe heals from the next.
- **Offline is the normal case**: the app is fully functional on local
  data; claims are idempotent, so replays and duplicates are harmless.
- **"Have" means "have the body"**: hash lists carry body bits, fingerprints
  hash them, and reconciliation requests a body the peer holds for a claim
  we hold stripped (unless redacted) — exchanging bare ids would leave
  healable bodies unhealed forever.

### The Peer (core::peer, BUILT): the composition with a name on the network

Everything above is machinery; the **Peer** is what a consumer holds. One
peer == one database == one identity context — multiple personas are
multiple peers, which makes the identity boundary process-shaped: a
persona's pipes can never learn the other persona exists. The user model
is the API: *build a useful database — write claims, get claims — and only
your words leave the house.*

```rust
let (peer, actor) = vouch_store::open_peer(dir, Some(writer), ServePolicy::Owned)?;
executor.spawn(actor.run());                 // touched once, forgotten

peer.claim(Draft::new("rec")                 // speech: body + attachments,
        .text("subject", "Joe's Pizza")      // minted atomically (blob-
        .attach("photo", jpeg, "image/jpeg") // before-claim is internal)
        .embed("original", their_event))     // quoting carries the chain
    .await?;
let p = peer.connect("mom-relay", end).await?;  // host plumbing: a duct
peer.follow(mom_log, p).await?;       // consumption: private, directional
peer.firehose().await?;               // local-only tap: everything (the UI)
peer.authored().await?;               // the ONLY network-facing stream
peer.fetch_blob(hash).await?;         // media on demand (lazy by default;
peer.evict_blob(hash).await?;         //   cull freely — claims keep the want)
```

Media policy is per-pipe: lazy by default (`connect`), eager where it
matters (`connect_with(.., PipeConfig { eager_media: true })` for p2p
pipes — take the photos while the phone is reachable; relays are eager by
role). See "Sessions never carry media" above.

**Input is a relationship; output is a broadcast.** `follow(log, pipe)` is
the one configured relationship — catch-up session now, live frames after,
healing forever. There is no inverse verb: output is two taps
(`firehose()` for the UI, `authored()` for whoever shows up), and the peer
holds no subscriber list — *watches* accumulate from inbound
announcements, ephemeral, gone with the connection. **Publishing is
following your own log somewhere**: the session's push half engages
exactly when the followed log is the one you write, so the push direction
emerges from holding the pen, not from a verb.

**The actor.** All state lives in one task selecting over a command
channel and an inbox of pipes — no locks, nothing held across an await
(the sans-io session never blocks, so it slots into the event loop one
`feed` at a time, sessions on different pipes interleaving freely). Pipes
carry *typed protocol messages* (`PipeMsg`: correlated request/response,
`Notify` frames, watch announcements); sockets and serialization live in
transport tasks, so the actor does no I/O and a test pipe is a pair of
channels. Three tables and their joins are the whole behavior:

| table | meaning | configured by |
| ----- | ------- | ------------- |
| `follows: LogId → {PipeId}` | what I consume, from where | `follow()` |
| the writer (≤ 1) | my voice | construction |
| `watches: PipeId → {LogId}` | others' follows of me | inbound announcements |

Sessions = follows ⋈ pipes. Fan-out = watches ⋈ pipes. A **relay is a peer
with no pen, no follows, and `ServePolicy::Everything`** — same actor, one
constructor argument apart from the app.

**Consumption is private.** With `ServePolicy::Owned` (the app default)
the peer serves — and accepts watches for — only the log it writes. Your
follow graph, your merged database, your reading: structurally not
servable, so not leakable by syncing with you. The one sanctioned path for
third-party content through you is the embed: quoting is speech with your
name on it. The deliberate trade: availability of a log depends on
author-controlled infrastructure (her peer, her relays) — replication for
availability is the author's opt-in, never ambient through readers.

**Scheduling owns when, never what.** Frames dropped under backpressure,
congested pipes aborting sessions, dead connections — all degrade to "a
later session catches up", because convergence lives in the layers below.
The actor's failure budget is latency, not correctness.

### Invitations

All access is granted out-of-band. No in-app discovery.

```text
vouch://invite?db=<base64-pubkey>&relay=<url>[&key=...][&token=...][&expires=...]
```

Sent as a link or QR code over channels you already trust (Signal, email, in
person). The `key` parameter carries the log's symmetric event key once
E2EE lands; in V1 it is absent.

## Naming: the Four-Name Model

Every log has up to four kinds of name, resolved in priority order:

1. **Petname** — your private name for it. Never transmitted. Always wins.
2. **Self-proposed name** — from the log's own signed `profile`
   claims. Verified, shown with a checkmark.
3. **Proposed names** — what vouchers claim the source is called.
   Unverified until you fetch from the source; could be stale or malicious.
   Shown with an "unverified" marker.
4. **The key itself** — truncated, as a last resort.

Naming data is local-only state, never part of any log except via vouches'
source annotations and the log's own `profile` claims. The same
resolution philosophy applies to entities (see Entities and aliases): your
local names and merges always win over anyone's claims.

## Privacy: deferred, not forgotten

V1 ships with plaintext payloads. This is a sequencing decision, not a scope
cut — the envelope/payload split exists from day one precisely so that
encryption can land later as a pure payload transform.

**Planned model** (unchanged from the original design):

- **Per-log symmetric key** (ChaCha20-Poly1305), shared with subscribers
  via the invitation. Relay operators and network observers can't read
  content; subscribers can.
- **Ed25519 signatures** on every claim (this part ships in V1 — signing is
  not deferred, only encryption).
- Relay learns: which logs exist, who owns them (`LogId` is the
  owner's pubkey), publish timing/volume, fetcher IPs. Relay cannot: read
  content or the link graph, tamper (signatures), impersonate (challenge
  auth).
- Out of scope, permanently: device compromise, coerced disclosure,
  nation-state traffic analysis. Vouch defends against curious operators and
  passive observers, not Mossad.

### Position on permanence

Synced is shared. Once a peer has replicated your log, your claims live on
hardware you don't control — the protocol cannot unpublish, and pretending
otherwise would be dishonest. `edit` and `disavowal` claims change what
conformant clients *display*, not what anyone *holds*. The UI must make this
legible at the moment of posting, not bury it in documentation.

### Redaction: cooperative deletion

Display-level retraction isn't enough when someone regrets the content
itself. A `redact` claim — engine-recognized, like embeds — asks conformant
stores to forget:

```text
{ type: "redact", redacts: ClaimRef }    // own log only
```

On ingest of a valid redaction, a store drops the target's *body* and keeps
the **signed tombstone**: the header and signature survive, so the claim's
existence, position in the chain, and authorship stay verifiable forever —
while the content, its outgoing links, and its place in every view are gone.
Backfill serves the tombstone in place of the content: a new subscriber never
downloads a redacted body, and the "marker" needs no special machinery
because it *is* a signed event, ingestible like any other.

The header/body split also separates deletion from censorship. A peer that
serves a claim without its body has merely failed to transfer content — the
body heals from any other pipe (a friend, a file, another relay), because it
verifies against the header's `body_hash` wherever it comes from. Only the
author's signed redact claim makes bodilessness *permanent*. A body-stripping
relay is a recoverable nuisance, not a censor.

Rules that keep it convergent and honest:

- **Monotone.** There is no un-redact; republish the content as a new claim
  instead. Content-then-redact and redact-then-content converge to the same
  tombstone, in any arrival order.
- **Seen is applied — top-level only.** A redaction takes effect whenever
  its verified body is seen as an event of its author's log. A redact claim
  arriving *inside a quote* is mere quotation — quoting is speech, and
  redaction authority flows only through the author's own log, which sync
  delivers anyway. A claim can't redact itself (its body would have to
  contain its own hash).
- **Redacting a quote takes the quoted content with it.** A quote was never
  a row — it is content of the quoting claim — so redacting the quote drops
  the copy, its backlinks, and its media pins in one stroke, with nothing
  orphaned and nothing to clean up. The quoted author's own claim, and
  every other quote of it, are untouched.
- **Redact bodies are never dropped.** A `redact` body is pure machinery — a
  hash pointer, no user content — and the *only* carrier of the fact it
  encodes. Suppressing it would erase that fact from the wire, so a peer
  restoring from a backup of tombstones could never learn it and would
  un-redact the original. So redacting a redaction records the entry but
  keeps the carrier; it hides nothing (there was no content) and resurrects
  nothing.
- **Own log only.** Anyone else's "redact" is mere speech — stored like any
  claim, no engine effect.
- **Best-effort by design.** Bytes embedded inside other people's vouches
  live inside *their* signed claims and cannot be removed by the quoted
  author — a conformant UI checks the redaction registry for quoted ids and
  dims or hides them, and vouchers can redact their own quotes, but this is
  etiquette backed by good defaults, not cryptography. The permanence
  position above still tells the whole truth.
  (V2+: per-epoch encryption keys allow crypto-shredding — delete the key,
  not the data.)

### Media: blobs ride a different rail

Images are essential from v0 (and stand in for all media). They use the same
move the header/body split does, one level down: a claim doesn't *contain*
its media, it **pins** it — a `BlobRef` value (hash, size, mime) anywhere in
a body, with the bytes living in a separate content-addressed **blob store**.
The signature transitively covers the image (header pins body, body pins
blob) without ever carrying it, so embeds stay small (a vouch re-ships 32
bytes, not megapixels), logs stay full-syncable, and `size`/`mime` let a UI
render placeholders and budget fetches before holding a single byte.

**Blobs are cache, not convergent state.** Claims sync eagerly; the per-log
fingerprint covers them. Blob presence is local, like arrival order: a store
can be fully synced on claims while missing bytes, and the UI shows a
placeholder. The claim store derives the **want-list** — blobs referenced by
live bodies and not yet held — and wants never expire; bytes verify against
the pinned hash on arrival, so *any* pipe can serve them and a lying pipe
can't poison the cache. Missing media heals exactly like stripped bodies.
Edges collect *through* quotes: a vouch that shows a photo pins that photo
under the vouch itself, so quoted media is wanted, demanded, and served
wherever the quote travels — a reader who follows only the voucher still
gets the picture, and the voucher's own pipes can serve it.

**Fetching is lazy, locality-first.** Publish discipline makes locality
work: a publisher uploads blobs *before* the claim that pins them, so the
pipe that handed you a claim almost certainly has its blobs — ask it first,
then any transport hosting the author's log, then (p2p later) anyone.
Priority is the engine's policy, not the store's: viewport-visible now,
thumbnails eagerly, big media on demand. Fetch failure is a placeholder,
never an error.

**Deletion is GC.** Redaction already kills a body's outgoing refs, so a
blob referenced by zero live bodies is garbage; sweeping it is how
cooperative deletion extends to media. Conversely a GC'd blob is not
"missing" — nothing live wants it.

The composition boundary: `ClaimStore` and `BlobStore` are *adjacent pure
state* in vouch-core — no I/O, no opinion about provenance of bytes, no
authoring. **Claim authoring is `Database`'s** (vouch-core's composition):
it mints claims and blobs from app data — put the bytes, pin them in a
body, sign with the writer, ingest, hand back the event for publishing —
composing the signing primitive (`Writer`) and the two stores, which
otherwise never touch. One owner for minting also means one owner for
sequencing: locally, blobs land before the claims that pin them, so GC
never races authoring. (On the wire there is no ordering to preserve at
all: sessions move claims only, and media is pulled by whoever wants it,
whenever it wants it — the claim's `BlobRef` IS the want.) The knowledge
of *where* to fetch a want from (which relay had the claim, which peers
are online, retry schedules) belongs above, to the peer actor (see The
Peer). The stores hold; the database composes; the session moves; the
peer talks.

## Keys, Identity & Devices

- **One keypair per log.** `LogId` is the public key, so TOFU is
  trivial — there is no separate trust step and no key/identity mismatch to
  detect.
- **Backup is a 24-word BIP39 mnemonic** of the signing key, shown at log
  creation. No key rotation in V1; the mnemonic is the identity.
- **Compromise = new identity.** Publish a farewell claim in the compromised
  log, create a new one, re-invite out-of-band. Crude, honest, V1.
- **Multi-device is explicitly single-device in V1**, but content-addressed
  identity already removes the sharp edge: a second device (or a writer
  restored from the mnemonic) just writes claims — a writer carries no
  position, so there is nothing to restore and nothing for two devices to
  disagree about. The full plan remains Signal's shape (identity key signs
  per-device keys, per-device logs merged under one displayed identity),
  with the header `version` field as the retrofit point.

**Key storage**: OS keychain on every platform (macOS Keychain, Windows
Credential Manager, iOS Keychain, Android Keystore).

## The Library Boundary

```text
vouch-core    the entire engine, I/O-free. Layered as modules: claim types,
              canonical encoding, sign/verify, stores (pure state under
              invariants); sync (the wire protocol as data, the sans-io
              session, the stateless responder, notify frames); peer (the
              actor: one task, channel pipes, follows/watches, the taps).
              No sockets, no clock, no runtime — channels only. (this
              crate + the test vectors IS the cross-language spec)
vouch-store   the I/O: SQLite claim storage, file blob storage, SQLite
              cursor store, open_peer(dir) wiring it all with the system
              clock; later the materializer framework, reactive queries
vouch-vocab   the vocabulary: well-known types, fields, rels + lenient parsers
vouch-app     vocabulary-driven UI, naming, invitations UX, GPUI state;
              holds Vec<Peer> (one per persona) and the transport tasks
```

**Cross-language strategy**: spec-first, not FFI-first. Other-language clients
are independent implementations of the wire format + vocabulary, validated
against the conformance vectors. Bindings (UniFFI etc.) only if a real
consumer shows up.

**Genericness rule**: a seam gets a trait only when it has two real consumers
today. Claim bodies — dynamic by design (the app's vocabulary, the engine's
reserved keys). Transport — not even a trait: a pipe is a pair of channels
carrying typed protocol messages, so any duct qualifies without
implementing anything (a socket transport is a task that shuttles
`PipeMsg`s; a test transport is the channels themselves). Storage — traits
under the invariants (memory for tests/simulation, SQLite and files for
the app; both real today). The store's *logic*, the session's, and the
actor's — concrete, exactly one implementation, on purpose.

**The litmus test**: could a trustdown-shaped tool
([mitchellh/vouch](https://github.com/mitchellh/vouch)) be built on this
engine? Under the claim-graph model the answer sharpens: it's just a
vocabulary — claim type `attestation`, link rel `denounces`. That question
gets asked of every API boundary, because it keeps the engine/vocabulary/app
split honest. It is *not* a shipped target: trustdown's defining virtues are
hand-editable text and zero-dependency parsing, and a signed claim log is
constitutionally neither.

## Non-Goals (V1)

- **Multi-writer logs** — imports consensus; defeats the core simplification
- **Multi-device** — single device first; Signal-style retrofit planned (see above)
- **Public discovery / search** — invite-only is a feature, not a gap
- **Pluggable store *semantics*** — storage backends are traits, but the
  convergence logic over them is one implementation, not an extension point
- **E2EE** — deferred one phase; the envelope split it needs ships in V1
- **Generic graph-query engine** — named patterns are hand-coded; the
  vocabulary stays small and curated
- **Media / blobs / unbounded data** — claims are small; full-log replication
  depends on keeping it that way (photos are a known want; they arrive with a
  blob story, not before)
- **Key rotation, anonymous publishing, reactions** — see [ROADMAP.md](./ROADMAP.md)

## V1 Scope

- Create, view, edit (supersede), and disavow recs and warnings in your own
  log
- Entity claims with `about` links; local alias resolution (`same-as`)
- One log per user, one device
- Claim log + generic link index + materialized views in SQLite, reactive
  queries to the UI
- Signed claims, canonical CBOR wire format, conformance test vectors,
  starter vocabulary
- Subscribe/unsubscribe via invite links and QR codes
- Vouch (rehost with embedded original) and cross-path dedup
- Content-addressed claims: hash identity, pipe-local arrival cursors, set fingerprints
- Redaction (cooperative deletion): signed tombstones, body fill-in
- Sync through the relay transport; offline-first throughout
- Four-name model with petnames
- BIP39 mnemonic backup

## Terminology

| Term             | Meaning                                                        |
| ---------------- | -------------------------------------------------------------- |
| **Claim**        | The one record shape: a signed header pinning a detachable CBOR body by hash |
| **ClaimHash**    | A claim's identity: BLAKE3 of its canonical header bytes        |
| **Body**        | A claim's deterministic-CBOR map; fully freeform                 |
| **ClaimRef**     | A tagged CBOR value referencing a claim by `(LogId, ClaimHash)`; legal anywhere in a body |
| **Embed**        | Another author's `SignedEvent` carried as a tagged value; verified in place by the engine — content of the quoting claim, not a row; read by recursion |
| **BlobRef**      | Bulk bytes (media) pinned by hash from a body; the bytes are cache, fetched lazily from any pipe |
| **Vocabulary**   | The normative set of well-known claim types, fields, and rels  |
| **Log**          | The network unit: an append-only claim sequence with a single keypair identity |
| **LogId**        | The public key of a log (IS the identity)                       |
| **Database**     | The local composition: N merged logs, their media, and the writers for logs you own |
| **Entity**       | A claim describing a person/place/thing that recs link `about` |
| **Rec / Warning**| The app's core content claims — endorse or caution             |
| **Vouch**        | Rehosting another's claim into your log: endorsement + durability |
| **Disavowal**    | A claim that retracts/distrusts another claim, with optional reason |
| **Redact**       | An engine-level claim asking stores to forget a body in your own log |
| **Tombstone**    | A claim whose body is gone: the signed header remains, verifiable forever |
| **Fingerprint**  | Per-log XOR of per-claim digests; equal iff two honest stores hold the same state |
| **Arrival**      | A claim's position in one store's per-log insertion order ("the count when it landed"); local metadata, never signed |
| **Cursor**       | "How many of this log's claims I've received from this pipe" — pipe-local, advanced only after successful ingest |
| **Peer**         | One database + one identity + the protocol machinery, behind one handle; the thing with a name on the network |
| **Follow**       | The one configured relationship: "this log, from this source" — directional, private, two-way only for the log you write |
| **Watch**        | A follow seen from the serving end: ephemeral, per-connection, never persisted |
| **Pipe**         | A duct carrying typed protocol messages; sockets and serialization live in transport tasks, never in the engine |
| **Draft**        | A claim under construction: body fields plus attachments, minted atomically |
| **Frame**        | A `Notify` push: an unsolicited Status with the events attached — advisory, droppable, zero-round-trip when fingerprints agree |
| **Firehose**     | The local-only tap: everything that lands, for the UI. Never leaves the process |
| **Authored**     | The only network-facing stream: claims from your own writer |
| **Subscription** | Following a log, replicating it locally              |
| **Petname**      | Your local, private name for a log (or entity)            |
| **Transport**    | Any task that moves `PipeMsg`s: relay shim, iroh stream, files |
| **Relay**        | A peer with no pen, no follows, serving everything published to it; one transport among several |

## References

- [LiveStore](https://livestore.dev/) — reactive event-sourced store design
- [Petnames Paper](https://files.spritely.institute/papers/petnames.html)
- [Signal Protocol](https://signal.org/docs/) — identity keys, multi-device model
- [SSB](https://handbook.scuttlebutt.nz/concepts/identity), [Nostr](https://github.com/nostr-protocol/nips), [AT Protocol](https://atproto.com/) — prior art for the claim-graph shape
- [iroh](https://github.com/n0-computer/iroh) — dial-by-pubkey QUIC transport
- [mitchellh/vouch](https://github.com/mitchellh/vouch) — the litmus test
- [RFC 8949 §4.2](https://www.rfc-editor.org/rfc/rfc8949#section-4.2) — CBOR core deterministic encoding
