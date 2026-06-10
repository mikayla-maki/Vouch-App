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
- **No CRDTs, no consensus.** Per-author sequence numbers totally order each
  log. There is nothing to vote on and nothing to converge except "have I seen
  event (author, n) yet?"
- **Trivial sync.** "Send me everything after sequence X" is the entire
  incremental sync protocol.

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
  decentralized naming, applied here to both databases and entities.
- [Signal](https://signal.org/docs/) — identity-is-a-keypair, TOFU, and (later)
  the multi-device model.
- [mitchellh/vouch](https://github.com/mitchellh/vouch) — proof that "personal
  attestation lists, merged by consumers" is a primitive worth building well.
  Used here as a litmus test, not a target (see Non-Goals).

## Core Model

### Database = Identity = Log

There is no separate "user" at the network level. Each database IS an
identity: one database, one keypair, one append-only log.

- `DatabaseId` is the Ed25519 public key itself.
- Alice's "Food Recs" and Alice's "Sports Takes" are unlinkable identities on
  the network, connected only inside Alice's app because she holds both keys.

```rust
/// A database is an identity.
struct DatabaseId(PublicKey);

/// A database you know about (yours or someone else's).
struct Database {
    id: DatabaseId,
    /// Present only for databases you own.
    signing_key: Option<SigningKey>,
    /// Sync state, None if you merely know of this database.
    subscription: Option<Subscription>,
}

/// Sync state for a database you follow.
struct Subscription {
    database_id: DatabaseId,
    last_synced_sequence: u64,
}
```

### Claims: one shape for everything

Every entry in a log is a **claim**: a header, a dynamically-typed body, and a
signature. There is no closed enum of event kinds. Recommendations, warnings,
entities, edits, disavowals, profile updates, merges — all are claims,
distinguished by convention (the vocabulary), not by structure.

```rust
struct EventHeader {
    /// Wire-format version. Structural changes to the signed layout bump
    /// this; new claim types and fields never do. Also the hedge that lets
    /// multi-device land later as a v2 header instead of a format break.
    version: u16,
    database_id: DatabaseId,
    /// Monotonic, per-database. Totally orders this log.
    sequence: u64,
    /// Author-claimed creation time. For display, never for correctness.
    timestamp: Timestamp,
}

struct Claim {
    header: EventHeader,
    /// A deterministic-CBOR map. Free-form, except for reserved keys.
    body: CborMap,
}

/// A claim as transmitted and stored: the canonical bytes it was signed
/// over, plus the signature. Decoding is a view; the bytes are the truth.
struct SignedEvent {
    bytes: Bytes,
    signature: Signature,
}
```

**Links are values, not fields.** The body is fully freeform. Instead of
reserving keys, the wire format defines two well-known CBOR-tagged value
types that may appear *anywhere* in a body — a top-level field, a list
entry, a span target inside rich text:

| Tagged value | Content | Purpose |
| ------------ | ------- | ------- |
| `ClaimRef` | `(DatabaseId, sequence)` | an edge to another claim; meaning given by context |
| `Embed`    | `SignedEvent` | a rehosted original, signature-verified by the engine |

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

A claim's canonical identity is its author's `(DatabaseId, sequence)`.
Rehosted copies carry the embedded original, so the same rec seen via three
paths (the author directly, plus two friends' vouches) deduplicates to one
item with three endorsements — and a disavowal of the original matches all
three paths, because every path resolves to the same canonical id.

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
  `(timestamp, database_id, sequence)` — deterministic across clients, but
  timestamps are author-claimed, so this order is cosmetic. Correctness never
  depends on it.

This invariant is the engine's contract and gets enforced by property tests:
shuffled replay of any claim set must produce a byte-identical projection.

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
signature = Ed25519::sign(signing_key, canonical_bytes(Claim))
```

**Rules:**

1. **Sign bytes, verify bytes.** Verifiers MUST check the signature against
   the bytes as received, and only then decode. Never decode → re-encode →
   verify; round-tripping is where canonicalization bugs hide.
2. **Store the original bytes.** Claims persist with their received encoding
   alongside the decoded form, so any claim can be re-transmitted or
   re-verified byte-for-byte. This is also what makes vouching verifiable.

### Envelope / payload split

Transports see two layers, kept separate from day one even while payloads are
plaintext:

- **Envelope**: `database_id`, `sequence` — the minimum a transport needs for
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
  (original bytes + decoded columns) from your databases and subscriptions.
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
defines what a serialized log is, so export/import of a database as a single
file falls out of the spec — and a file is just another pipe (see Transports).

## Sync & Transports

### Transport is a trait; everything is a pipe

```rust
trait Transport {
    /// Publish events to a database you own (auth: signature challenge).
    async fn publish(&self, db: DatabaseId, events: Vec<Envelope>) -> Result<()>;
    /// Incremental pull: everything after a sequence number.
    async fn fetch_since(&self, db: DatabaseId, seq: u64) -> Result<Vec<Envelope>>;
    /// Live tail for reactive sync.
    async fn stream(&self, db: DatabaseId, from: u64) -> Result<EventStream>;
}
```

Planned implementations, in order:

1. **Relay** — a dumb store-and-forward server, for networking ease. Owners
   authenticate via signature challenge (the relay sends a nonce; the client
   signs it; `DatabaseId` is the verification key). Fetching requires no auth.
2. **iroh p2p** — [iroh](https://github.com/n0-computer/iroh)'s
   dial-by-public-key QUIC maps directly onto `DatabaseId`-is-a-pubkey, and an
   iroh relay node is literally "the relay as just another pipe." Strong
   candidate to be the relay's implementation substrate rather than a separate
   transport; decided by prototyping behind the trait.
3. **Files** — a serialized log is a valid transport: backups, sneakernet,
   attach-your-database-to-an-email.

### Sync flow

- **Publish**: create → sign → append locally → index/materialize → push via
  any transport when available.
- **Subscribe**: `fetch_since(last_synced_sequence)` → verify signatures →
  store → index/materialize → bump sync state.
- **Offline is the normal case**: the app is fully functional on local data;
  claims are idempotent, so replays and duplicates are harmless.

### Invitations

All access is granted out-of-band. No in-app discovery.

```text
vouch://invite?db=<base64-pubkey>&relay=<url>[&key=...][&token=...][&expires=...]
```

Sent as a link or QR code over channels you already trust (Signal, email, in
person). The `key` parameter carries the database's symmetric event key once
E2EE lands; in V1 it is absent.

## Naming: the Four-Name Model

Every database has up to four kinds of name, resolved in priority order:

1. **Petname** — your private name for it. Never transmitted. Always wins.
2. **Self-proposed name** — from the database's own signed `profile`
   claims. Verified, shown with a checkmark.
3. **Proposed names** — what vouchers claim the source is called.
   Unverified until you fetch from the source; could be stale or malicious.
   Shown with an "unverified" marker.
4. **The key itself** — truncated, as a last resort.

Naming data is local-only state, never part of any log except via vouches'
source annotations and the database's own `profile` claims. The same
resolution philosophy applies to entities (see Entities and aliases): your
local names and merges always win over anyone's claims.

## Privacy: deferred, not forgotten

V1 ships with plaintext payloads. This is a sequencing decision, not a scope
cut — the envelope/payload split exists from day one precisely so that
encryption can land later as a pure payload transform.

**Planned model** (unchanged from the original design):

- **Per-database symmetric key** (ChaCha20-Poly1305), shared with subscribers
  via the invitation. Relay operators and network observers can't read
  content; subscribers can.
- **Ed25519 signatures** on every claim (this part ships in V1 — signing is
  not deferred, only encryption).
- Relay learns: which databases exist, who owns them (`DatabaseId` is the
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

## Keys, Identity & Devices

- **One keypair per database.** `DatabaseId` is the public key, so TOFU is
  trivial — there is no separate trust step and no key/identity mismatch to
  detect.
- **Backup is a 24-word BIP39 mnemonic** of the signing key, shown at database
  creation. No key rotation in V1; the mnemonic is the identity.
- **Compromise = new identity.** Publish a farewell claim in the compromised
  database, create a new one, re-invite out-of-band. Crude, honest, V1.
- **Multi-device is explicitly single-device in V1.** The plan is Signal's
  shape when it lands: an identity key signs per-device keys, each device
  writes its own log, and clients merge per-device logs under one displayed
  identity — preserving the single-writer invariant instead of forking
  sequence numbers. The `version` field in the event header is the designated
  retrofit point; this is a planned v2 header, not a redesign.

**Key storage**: OS keychain on every platform (macOS Keychain, Windows
Credential Manager, iOS Keychain, Android Keystore).

## The Library Boundary

```text
vouch-core    claim types, canonical encoding, sign/verify, embed verification,
              fold invariants. No I/O.
              (this crate + the test vectors IS the cross-language spec)
vouch-store   SQLite claim log, generic link index, materializer framework,
              reactive queries
vouch-sync    Transport trait + sync sessions (relay, iroh, files)
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

- **Multi-writer databases** — imports consensus; defeats the core simplification
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
  database
- Entity claims with `about` links; local alias resolution (`same-as`)
- One database per user, one device
- Claim log + generic link index + materialized views in SQLite, reactive
  queries to the UI
- Signed claims, canonical CBOR wire format, conformance test vectors,
  starter vocabulary
- Subscribe/unsubscribe via invite links and QR codes
- Vouch (rehost with embedded original) and cross-path dedup
- Sync through the relay transport; offline-first throughout
- Four-name model with petnames
- BIP39 mnemonic backup

## Terminology

| Term             | Meaning                                                        |
| ---------------- | -------------------------------------------------------------- |
| **Claim**        | The one record shape: header + CBOR body + signature           |
| **Body**         | A claim's deterministic-CBOR map; free-form except reserved keys |
| **ClaimRef**     | A tagged CBOR value referencing a claim by `(DatabaseId, sequence)`; legal anywhere in a body |
| **Embed**        | Another author's `SignedEvent` carried as a tagged value; verified by the engine |
| **Vocabulary**   | The normative set of well-known claim types, fields, and rels  |
| **Database**     | An append-only claim log with a single keypair identity        |
| **DatabaseId**   | The public key of a database (IS the identity)                 |
| **Entity**       | A claim describing a person/place/thing that recs link `about` |
| **Rec / Warning**| The app's core content claims — endorse or caution             |
| **Vouch**        | Rehosting another's claim into your log: endorsement + durability |
| **Disavowal**    | A claim that retracts/distrusts another claim, with optional reason |
| **Subscription** | Following a database, replicating its log locally              |
| **Petname**      | Your local, private name for a database (or entity)            |
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
