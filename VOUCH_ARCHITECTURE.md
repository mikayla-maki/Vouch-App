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
- **Trivial sync.** "Send me what I don't have" — by relay-local cursor or by
  advisory sequence number, confirmed by a per-log set fingerprint. All of
  these are hints between cooperating clients; none is an invariant.

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
    /// ADVISORY position in the author's log: shared language for ordering
    /// and sync between cooperating clients. Never unique by guarantee;
    /// non-cooperating writers are ignored, not errored. Drift this can't
    /// see is the sync layer's job (set fingerprints), not the header's.
    sequence: u64,
    /// Author-claimed creation time. For display, never for correctness.
    timestamp: Timestamp,
    /// Hash of the canonical body bytes — the pin that lets a body detach.
    body_hash: Hash,
}

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
3. **`Embed` is the engine's business.** Verifying an embedded author's
   signature is a byte-level concern no vocabulary can express, so embedded
   originals are verified, deduplicated, and indexed by the engine itself,
   wherever in a body they appear.

### The vocabulary

The engine moves bytes and indexes edges; the vocabulary is where meaning
lives. It is a short normative document with the same status as the wire
format spec, shipped with conformance vectors. The starter set:

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
paths (the author directly, plus two friends' vouches) deduplicates to one
item with three endorsements — and a disavowal of the original matches all
three paths, because every path hashes to the same id.

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
- **Display order is not log order.** The UI sorts by
  `(timestamp, log_id, sequence, id)` — deterministic across clients,
  but timestamps and sequences are author-claimed, so this order is
  cosmetic. Correctness never depends on it.
- **Local metadata is not state.** Convergent state is exactly the
  replicated substance: headers, bodies, redactions. Which valid signature a
  store holds (an author can mint many; each is equal proof) and how a claim
  was learned (directly vs. embedded) are local facts that id-based sync can
  never reconcile — so they are defined out of the state surface rather than
  papered over with tiebreaks.

This invariant is the engine's contract and gets enforced by property tests:
shuffled replay of any claim set must produce a byte-identical projection.

### No forks: identity is content

There are no slots for two claims to fight over. A claim's identity is the
hash of its signed header, so "two different claims" is the *whole* analysis
— both store, both are true facts ("this key signed this"), and the engine
needs no detection, no branching, no tiebreaks, and no policy. The classic
fork scenario (a writer restored from a mnemonic re-uses a position) produces
two ordinary claims that happen to share an advisory sequence number.
Nothing breaks; nothing needs the user's attention.

What the sequence number is *for*, then (the atproto blend —
content-addressed records plus an advisory monotonic revision): a shared
language for cooperating clients. "Give me everything after ~40" works
against a relay; relay-local arrival cursors work for dumb servers. All
hints. A writer that emits nonsense ordering wastes nobody's correctness,
only its own sync efficiency.

There is deliberately no `prev` hash chain, enforced or advisory. Enforced
chaining gives structural fork prevention but makes deletion impossible —
every removed claim breaks verification of everything after it. An
*advisory* chain earns its 33 bytes per header only as a drift detector, and
the set fingerprint (below) detects strictly more drift from the sync layer,
where that concern belongs. atproto walked the same path: repo v3 deprecated
`prev` and kept `rev`. The header is plumbing, not meaning. The graph that
means something — recs, warnings, entities, vouches, disavowals — lives
entirely *inside* claim bodies as `ClaimRef` values.

### Drift detection: trust the cursor, verify the set

An advisory cursor can't see *silent* divergence. The canonical case: a
writer's device dies, it restores from yesterday's backup, and it re-uses
sequence 6 for a different claim. A follower who already held the old 6
catches up "since 6", receives 7 and 8, and now both sides sit at the same
max sequence with the same claim count — every cursor agrees — while the
follower holds a claim the author lost and is missing one it will never be
sent. The proxy ("are the counters aligned") says yes; the invariant ("do we
hold the same set") says no.

So state itself gets a cheap check. Each store maintains, per subscribed
log, an order-independent **fingerprint**: the XOR of one BLAKE3 digest per
claim, where each digest commits to the claim hash, whether the body is held
("have" means "have the body"), and any redaction applied. Two honest stores
agree on a log's fingerprint exactly when they agree on that log's
convergent state. The catch-up handshake is then:

1. **Fast path.** Pull by cursor: "give me everything since ~N."
2. **Confirm.** "Thanks — by the way, here's my fingerprint for this log."
3. **Reconcile on mismatch.** Fall back to full set reconciliation —
   exchange claim-hash lists, union up. A full sync may take a hot second,
   but it's resumable and interruptible like everything else: it's just
   claims arriving, in any order, tracked by the same cursors.

Like the sequence number, the fingerprint is advisory — XOR-of-digests
detects drift between cooperating clients; it is not a defense against
liars, and we know nothing at this layer is. Equivocation (deliberately
telling different people different histories) remains *observable* — two
claims sharing a sequence — and gossipable as an embeds-backed claim anyone
can verify: evidence for app-layer judgment, not engine state.

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
signature = Ed25519::sign(signing_key, canonical_header_bytes)
id        = BLAKE3(canonical_header_bytes)
body_hash = BLAKE3(canonical_body_bytes)   // pinned inside the header
```

**Rules:**

1. **Sign bytes, verify bytes.** Verifiers MUST check the signature against
   the bytes as received, and only then decode. Never decode → re-encode →
   verify; round-tripping is where canonicalization bugs hide.
2. **Store the original bytes.** Claims persist with their received encoding
   alongside the decoded form, so any claim can be re-transmitted or
   re-verified byte-for-byte. This is also what makes vouching verifiable.
3. **Bodies are capped at 64 KiB.** A normative rule, not a courtesy —
   stores must agree on which claims are valid or their fingerprints diverge.
   Writers refuse to sign past the cap; verifiers refuse to accept past it.
   ~64 KiB is roughly ten thousand words: a short story per claim, or
   thousands of refs. A body is one piece of speech; bulk data (images,
   media) is pinned from the body by hash as content-addressed blobs, which
   is what keeps logs small enough to full-sync and hold in memory.

### Envelope / payload split

Transports see two layers, kept separate from day one even while payloads are
plaintext:

- **Envelope**: `log_id` plus whatever advisory hints a transport wants
  (sequence, fingerprints, arrival cursors) — the minimum needed for routing
  and incremental sync.
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

**Position: the storage backend is concrete, not pluggable.** Materialized
views, reactive queries, and sync bookkeeping all lean on SQLite specifically;
a trait abstracting "SQLite or a flat file" would be satisfied by neither.
The storage *format*, however, is already generic for free: the wire format
defines what a serialized log is, so export/import of a log as a single
file falls out of the spec — and a file is just another pipe (see Transports).

## Sync & Transports

### Transport is a trait; everything is a pipe

```rust
trait Transport {
    /// Publish events to a log you own (auth: signature challenge).
    async fn publish(&self, db: LogId, events: Vec<Envelope>) -> Result<()>;
    /// Incremental pull: everything after a sequence number.
    async fn fetch_since(&self, db: LogId, seq: u64) -> Result<Vec<Envelope>>;
    /// Live tail for reactive sync.
    async fn stream(&self, db: LogId, from: u64) -> Result<EventStream>;
    /// Drift check: the peer's set fingerprint for a log (see Drift
    /// detection). Mismatch after a catch-up → full set reconciliation.
    async fn fingerprint(&self, db: LogId) -> Result<[u8; 32]>;
    /// Blob bytes by hash (verified against the pin on arrival). Publish
    /// uploads blobs BEFORE the claims that pin them, so wherever a claim
    /// came from almost certainly has its media.
    async fn blob(&self, hash: BlobHash) -> Result<Vec<u8>>;
}
```

Planned implementations, in order:

1. **Relay** — a dumb store-and-forward server, for networking ease. Owners
   authenticate via signature challenge (the relay sends a nonce; the client
   signs it; `LogId` is the verification key). Fetching requires no auth.
2. **iroh p2p** — [iroh](https://github.com/n0-computer/iroh)'s
   dial-by-public-key QUIC maps directly onto `LogId`-is-a-pubkey, and an
   iroh relay node is literally "the relay as just another pipe." Strong
   candidate to be the relay's implementation substrate rather than a separate
   transport; decided by prototyping behind the trait.
3. **Files** — a serialized log is a valid transport: backups, sneakernet,
   attach-your-log-to-an-email.

### Sync flow

- **Publish**: create → sign → append locally → index/materialize → push via
  any transport when available.
- **Subscribe**: `fetch_since(last_synced_sequence)` → verify signatures →
  store → index/materialize → bump sync state → compare fingerprints;
  mismatch kicks off a full reconciliation in the background.
- **Offline is the normal case**: the app is fully functional on local data;
  claims are idempotent, so replays and duplicates are harmless.
- **"Have" means "have the body"**: when peers compare what they hold, a
  tombstoned-or-stripped claim counts as wanted — exchanging bare ids would
  leave healable bodies unhealed forever.

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
- **Seen is applied.** A redaction takes effect whenever its verified body is
  seen — even if that body is itself about to be suppressed by a chained
  redaction. Redacting your redaction hides the reason, never the effect; a
  claim can't redact itself (its body would have to contain its own hash).
- **Own log only.** Anyone else's "redact" is mere speech — stored like any
  claim, no engine effect.
- **Best-effort by design.** Bytes embedded inside other people's vouches
  live inside *their* signed claims and cannot be removed — conformant
  stores suppress them from display and indexing, and vouchers can supersede
  their vouches, but this is etiquette backed by good defaults, not
  cryptography. The permanence position above still tells the whole truth.
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
fingerprint covers them. Blob presence is local, like provenance: a store
can be fully synced on claims while missing bytes, and the UI shows a
placeholder. The claim store derives the **want-list** — blobs referenced by
live bodies and not yet held — and wants never expire; bytes verify against
the pinned hash on arrival, so *any* pipe can serve them and a lying pipe
can't poison the cache. Missing media heals exactly like stripped bodies.

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
authoring. The knowledge of *where* to fetch a want from (which relay had
the claim, which peers are online, retry schedules) belongs to the engine
layer that owns long-lived transports. **Claim authoring is the engine's
too**: it mints claims and blobs from app data — put the bytes, pin them in
a body, sign with the writer, ingest, queue the publish — composing the
signing primitive (`Writer`) and the two stores, which otherwise
never touch. One owner for minting also means one owner for sequencing:
blobs land before the claims that pin them (locally and on publish), so GC
never races authoring. The stores hold; the engine composes.

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
  restored from the mnemonic with no memory of its position) just re-uses
  sequence numbers — harmless, mergeable, invisible to correctness, and
  flagged for resync by the set fingerprint. The full plan remains Signal's
  shape (identity key signs
  per-device keys, per-device logs merged under one displayed identity),
  with the header `version` field as the retrofit point.

**Key storage**: OS keychain on every platform (macOS Keychain, Windows
Credential Manager, iOS Keychain, Android Keystore).

## The Library Boundary

```text
vouch-core    claim types, canonical encoding, sign/verify, embed verification,
              fold invariants, claim store + adjacent blob store (pure state).
              No I/O. (this crate + the test vectors IS the cross-language spec)
vouch-store   SQLite claim log, generic link index, materializer framework,
              reactive queries
vouch-sync    the engine: composes writer + claim store + blob store (mints
              claims/blobs from app data), owns transports, cursors, fetch
              policy. Transport trait + sync sessions (relay, iroh, files)
vouch-vocab   the vocabulary: well-known types, fields, rels + lenient parsers
vouch-app     vocabulary-driven UI, naming, invitations UX, GPUI state
```

**Cross-language strategy**: spec-first, not FFI-first. Other-language clients
are independent implementations of the wire format + vocabulary, validated
against the conformance vectors. Bindings (UniFFI etc.) only if a real
consumer shows up.

**Genericness rule**: a seam gets a trait only when it has two real consumers
today. Claim bodies — dynamic by design (the app's vocabulary, the engine's
reserved keys). Transport — trait (relay, iroh, files; all near-term).
Storage backend — concrete (SQLite, full stop).

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
- **Pluggable storage backends** — SQLite is load-bearing
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
- Content-addressed claims: hash identity, advisory sequence, set fingerprints
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
| **Embed**        | Another author's `SignedEvent` carried as a tagged value; verified by the engine |
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
| **Subscription** | Following a log, replicating it locally              |
| **Petname**      | Your local, private name for a log (or entity)            |
| **Transport**    | Any pipe that moves envelopes: relay, iroh, files              |
| **Relay**        | A dumb store-and-forward server; one transport among several   |

## References

- [LiveStore](https://livestore.dev/) — reactive event-sourced store design
- [Petnames Paper](https://files.spritely.institute/papers/petnames.html)
- [Signal Protocol](https://signal.org/docs/) — identity keys, multi-device model
- [SSB](https://handbook.scuttlebutt.nz/concepts/identity), [Nostr](https://github.com/nostr-protocol/nips), [AT Protocol](https://atproto.com/) — prior art for the claim-graph shape
- [iroh](https://github.com/n0-computer/iroh) — dial-by-pubkey QUIC transport
- [mitchellh/vouch](https://github.com/mitchellh/vouch) — the litmus test
- [RFC 8949 §4.2](https://www.rfc-editor.org/rfc/rfc8949#section-4.2) — CBOR core deterministic encoding
