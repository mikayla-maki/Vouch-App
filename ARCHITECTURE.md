# Vouch Architecture

Vouch is a local-first, privacy-preserving database of recommendations by you and your trusted friends.

## Terminology

| Term | Meaning |
|------|---------|
| **Rec** | A recommendation—the core content unit (short for "recommendation") |
| **Database** | An event log owned by an identity. One user can have multiple databases for different topics. |
| **Subscription** | Following a database to replicate its events locally. |
| **Revouch** | Rehosting a rec from a subscribed database into your own database. An endorsement AND a durability decision. |

## Overview

Vouch enables users to:
- Create and manage recommendations locally in their own database(s)
- Subscribe to other users' databases to see their recs
- Revouch (rehost) recs from subscriptions into their own database
- Maintain multiple databases for different topics or audiences
- Disconnect from or block databases and disavow recs

The system is built on three pillars:
1. **Local-first**: Your data lives on your device. The network is for sync, not storage.
2. **Privacy by design**: E2E encryption for all sync. You control what you share.
3. **Trust through relationships**: Recs flow through your personal network, not algorithms.

## Core Concepts

### The Database Model

```
┌─────────────────────────────────────────────────────────────────┐
│                        YOUR LOCAL STORAGE                        │
├─────────────────────────────────────────────────────────────────┤
│  ┌──────────────────┐   ┌──────────────────┐                    │
│  │  YOUR DATABASE   │   │  YOUR DATABASE   │                    │
│  │  (Food Recs)     │   │  (Sports Recs)   │   ← LOCAL          │
│  │                  │   │                  │     (you are the   │
│  │  - Your recs     │   │  - Your recs     │      source of     │
│  │  - Your revouches│   │  - Your revouches│      truth)        │
│  └──────────────────┘   └──────────────────┘                    │
│                                                                  │
│  ┌──────────────────┐   ┌──────────────────┐                    │
│  │ ALICE'S DATABASE │   │  CONSUMER CORP   │   ← SUBSCRIBED     │
│  │                  │   │    DATABASE      │     (they are the  │
│  │  - Her recs      │   │                  │      source of     │
│  │  - Her revouches │   │  - Their reviews │      truth)        │
│  └──────────────────┘   └──────────────────┘                    │
└─────────────────────────────────────────────────────────────────┘
```

**Key semantics:**
- **Local databases**: Append-only event logs you control. This is YOUR data. You can have multiple for different topics/audiences. You are the source of truth.
- **Subscribed databases**: Local replicas of others' databases. They are the source of truth. You store these events locally for offline access.
- **Revouch = rehost**: Explicitly copies a rec from a subscribed database INTO your local database. Now it's yours. You're endorsing it AND taking storage responsibility.

**Why databases instead of per-user connections?**

The previous model defined event logs *between* individual users, requiring explicit per-user decisions for every rec. This made several things awkward:
- Where do your recs go when you have no connections? (Special-cased "personal log")
- How do you share with many people at once? (User groups, complex ACLs)
- How do you subscribe to public sources like Consumer Reports? (Different mechanism)

With databases:
- Your database exists whether you have zero subscribers or a million
- Subscribing to a friend works exactly like subscribing to Consumer Reports
- Topic separation = multiple databases (no per-rec ACLs needed)
- Revouch is an explicit trust/durability boundary, not automatic sync

### Petnames

User identity follows the [petname system](https://files.spritely.institute/papers/petnames.html):

- **Identifier**: An opaque, globally unique, decentralized identifier (e.g., a public key)
- **Petname**: A local, human-readable name you assign to a contact (e.g., "Mom", "College Roommate")
- **Self-proposed name**: The name a user broadcasts as their preferred display name

You see your petnames. Others see theirs. There's no global namespace to fight over.

```
┌─────────────────────────────────────────────────────────────┐
│  Your View              │  Alice's View                     │
├─────────────────────────┼───────────────────────────────────┤
│  "Mom" ──────────────── │ ──────────────────────── "Me"     │
│  "Alice" ────────────── │ ──────────────────── "Best Friend"│
│  "Thai Place Guy" ───── │ ─────────────────── "Valued Cust" │
└─────────────────────────────────────────────────────────────┘
            Same identifiers, different petnames
```

### Convergent Event Sourcing

Vouch uses event sourcing with **eventually consistent** semantics:

- All state changes are captured as immutable events
- Events are replicated across devices and subscriptions
- When any two nodes see the same set of events, they converge to the same state
- Order of event arrival doesn't matter—only the final set

This is achieved through:
- **Monotonic operations**: Events only add information (recs, reactions) or mark things as invalid (disavowals via tombstones)
- **Content-addressed data**: Recs are identified by their content hash, enabling deduplication
- **Tombstone strategy**: Disavowals are stored even if the target event hasn't arrived yet

## Data Model

### Content Hash as Primary Identifier

Recs are identified by the hash of their content:

```rust
/// The actual rec content, hashed for identity
struct RecommendationContent {
    subject: String,     // The entity being discussed
    body: String,        // The actual recommendation text
}

/// A content hash uniquely identifies rec content
/// ContentHash = hash(subject, body)
struct ContentHash(Vec<u8>);
```

This enables:
- **Deduplication**: Same rec from multiple databases is recognized as identical
- **Stable references**: Reactions and disavowals reference content that won't change
- **Verification**: When Bob revouches "Alice's rec," you can verify he didn't alter the text by checking `hash(content) == original_hash`

### User Identity and Cryptographic Signing

User identifiers are public keys. Each user generates a keypair—the public key *is* their identity, and the private key signs all their events.

```rust
/// A user's public key - this IS their identity
struct UserIdentifier(PublicKey);

/// A contact in your local address book
struct Contact {
    identifier: UserIdentifier,
    petname: String,
    self_proposed_name: Option<String>,
}
```

**Why signatures matter:**

Content hashes verify *what* was said. Signatures verify *who* said it.

Without signatures, Bob could fabricate "Alice's rec" and Carol would have no way to verify Alice actually authored it. With signatures, every event carries cryptographic proof of authorship that anyone can verify using the author's public key.

### Databases

A database is an event log owned by an identity. Each user has at least one database (their default), but can create additional databases for topic separation.

```rust
/// A database is an event log owned by an identity
struct Database {
    /// Unique identifier for this database
    id: DatabaseId,
    
    /// The identity that owns this database (signs all events)
    owner: UserIdentifier,
    
    /// Human-readable name for the database
    name: String,
    
    /// Optional description
    description: Option<String>,
    
    /// Whether this is a local (owned) or subscribed database
    is_local: bool,
}

/// Unique database identifier (could be hash of owner + name + creation time)
struct DatabaseId(Vec<u8>);
```

**Multiple databases per user:**

Bob might have:
- "Bob's Food Recs" (shared with foodie friends)
- "Bob's Sports Takes" (shared with sports friends)
- "Bob's Private Notes" (no subscribers)

These are treated as independent entities on the network. Carol can subscribe to one without the other.

**Identity linking:**

For v1, databases from the same owner are linkable (same `UserIdentifier`). This is convenient ("show me all of Bob's databases") but reduces privacy. We may revisit this for users who want unlinkable pseudonymous databases.

### Events

All state changes flow through the event system. Every event is signed by the database owner.

```rust
/// A single event in a database
struct VouchEvent {
    /// Which database this event belongs to
    database_id: DatabaseId,
    
    /// Who authored this event (must be database owner)
    author: UserIdentifier,
    
    /// Per-database sequence number for sync
    sequence: u64,
    
    /// When this event was created
    timestamp: Timestamp,
    
    /// The actual event payload
    payload: VouchEventPayload,
    
    /// Cryptographic signature
    signature: Signature,
}

enum VouchEventPayload {
    /// A new rec you're making
    Rec(RecommendationContent),
    
    /// Rehosting someone else's rec into your database
    Revouch {
        /// The database this rec came from
        source_database: DatabaseId,
        /// Original author's signature (proves authenticity)
        original_signature: Signature,
        /// The rec content (embedded for simplicity)
        content: RecommendationContent,
    },
    
    /// Update your database metadata
    UpdateDatabase {
        name: String,
        picture: Option<ContentHash>,
    },
    
    /// Mark a rec as retracted/untrusted
    Disavow {
        author: UserIdentifier,
        content_hash: ContentHash,
    },
    
    /// A reaction to a rec
    React {
        author: UserIdentifier,
        content_hash: ContentHash,
        reaction: Reaction,
    },
}

enum Reaction {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Heart,
}
```

**Revouch verification chain:**

1. Alice creates a rec in her database, signs with her private key
2. Bob subscribes to Alice's database, sees her rec
3. Bob revouches into his database, including Alice's original signature
4. Carol subscribes to Bob's database and verifies:
   - Bob's signature on the revouch event ✓
   - Alice's original signature on the content ✓
5. Carol now has cryptographic proof of the entire chain

### Subscriptions

A subscription is a sync relationship with another database.

```rust
/// A subscription to another database
struct Subscription {
    /// The database you're subscribed to
    database_id: DatabaseId,
    
    /// The database owner (for verification)
    owner: UserIdentifier,
    
    /// Your petname for this subscription
    petname: String,
    
    /// Sync state: highest sequence you've received
    last_synced_sequence: u64,
    
    /// Whether sync is active
    status: SubscriptionStatus,
}

enum SubscriptionStatus {
    Active,
    Paused,
    Blocked,
}
```

**Local vs Subscribed databases:**

All events are stored the same way. The difference is ownership:
- **Local databases**: You create events. You are the source of truth.
- **Subscribed databases**: You replicate events. They are the source of truth.

**Subscribe vs Revouch:**

| Action | Meaning | Where it lives | Durability |
|--------|---------|----------------|------------|
| **Subscribe** | "I want to see this" | Subscribed database | Depends on source |
| **Revouch** | "I endorse this AND host it" | Your local database | You control |

## Sync Protocol

### Transport Abstraction

The sync layer is transport-agnostic:

```rust
#[async_trait]
trait DatabaseTransport {
    /// Fetch events from a database since a sequence number
    async fn fetch_events(
        &self,
        database: &DatabaseId,
        since_sequence: u64,
    ) -> Result<Vec<VouchEvent>>;
    
    /// Publish events to your own database (for subscribers to fetch)
    async fn publish_events(
        &self,
        database: &DatabaseId,
        events: Vec<VouchEvent>,
    ) -> Result<()>;
    
    /// Subscribe to real-time updates from a database
    async fn subscribe(
        &self,
        database: &DatabaseId,
    ) -> Result<EventStream>;
}
```

### Sync Flow

**Publishing (your databases):**
1. Create event locally, sign it, append to your database
2. Publish to relay/storage for subscribers to fetch
3. Subscribers pull on their own schedule

**Subscribing (others' databases):**
1. Request events since your last synced sequence
2. Verify signatures on received events
3. Store events locally
4. Update sync state

**Handling offline:**
1. Work with locally stored data while offline
2. On reconnect, fetch missed events from subscribed databases
3. Publish any events you created while offline

Events are idempotent—receiving the same event twice is harmless.

### Tombstone Handling

When a `Disavow` event arrives before the target `Rec`:

1. Store the tombstone: `(author, content_hash) → tombstoned`
2. When the `Rec` eventually arrives, check tombstone set
3. If tombstoned, mark as disavowed immediately

This ensures convergence regardless of event arrival order.

## Local Storage

### Event Log (Source of Truth)

Events are persisted to SQLite as the authoritative record:

```sql
-- All databases (local and subscribed)
CREATE TABLE databases (
    id BLOB PRIMARY KEY,
    owner_id BLOB NOT NULL,
    name TEXT NOT NULL,
    description TEXT,
    is_local BOOLEAN NOT NULL,  -- TRUE = you own it, FALSE = subscribed
    last_synced_sequence INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL
);

-- All events (from local and subscribed databases)
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    database_id BLOB NOT NULL,
    author_id BLOB NOT NULL,
    sequence INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    payload BLOB NOT NULL,
    signature BLOB NOT NULL,
    UNIQUE(database_id, sequence),
    FOREIGN KEY (database_id) REFERENCES databases(id)
);

-- Tombstones (applies to all databases)
CREATE TABLE tombstones (
    author_id BLOB NOT NULL,
    content_hash BLOB NOT NULL,
    tombstoned_at INTEGER NOT NULL,
    PRIMARY KEY (author_id, content_hash)
);
```

Events are events—whether from a local or subscribed database, they're stored the same way. The `databases.is_local` field tells you who the source of truth is.

### Materialized View (Query Layer)

Events are projected into a queryable format:

```sql
-- Deduplicated rec content
CREATE TABLE recs (
    content_hash BLOB PRIMARY KEY,
    subject TEXT NOT NULL,
    body TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL
);

-- Who has this rec (original or revouch)
CREATE TABLE rec_sources (
    content_hash BLOB NOT NULL,
    database_id BLOB NOT NULL,
    author_id BLOB NOT NULL,
    is_revouch BOOLEAN NOT NULL,
    source_database_id BLOB,  -- NULL if original, set if revouch
    event_sequence INTEGER NOT NULL,
    is_disavowed BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (content_hash, database_id)
);

-- Reactions
CREATE TABLE reactions (
    content_hash BLOB NOT NULL,
    reactor_id BLOB NOT NULL,
    reaction TEXT NOT NULL,
    reacted_at INTEGER NOT NULL,
    PRIMARY KEY (content_hash, reactor_id)
);

-- Contact book (identities you've named)
CREATE TABLE contacts (
    identifier BLOB PRIMARY KEY,
    petname TEXT NOT NULL,
    self_proposed_name TEXT,
    created_at INTEGER NOT NULL
);
```

### Reactivity

The UI subscribes to query results. When events arrive and update the materialized view, affected queries re-run and the UI updates automatically. This follows the [LiveStore pattern](https://docs.livestore.dev/evaluation/how-livestore-works/).

## The Merged Feed View

The UI shows a unified view of all databases (local + subscribed):

```
┌─────────────────────────────────────────────────────────────────┐
│  Your Feed (merged view)                                        │
├─────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ 🍜 Thai Place Downtown                                  │    │
│  │ "Best pad thai I've ever had..."                        │    │
│  │ from: Alice's Recs • 2 hours ago                        │    │
│  └─────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ 🔧 John's Auto                                          │    │
│  │ "Honest mechanic, fair prices"                          │    │
│  │ from: Your Food Recs (revouched from Bob) • 1 day ago   │    │
│  └─────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │ ⭐ Consumer Reports Rating                               │    │
│  │ "Top-rated vacuum cleaner 2024"                         │    │
│  │ from: Consumer Reports • 3 days ago                     │    │
│  └─────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────┘
```

Each rec shows:
- The content (subject + body)
- The source database (with your petname)
- If it's a revouch, the original source
- Timestamp

Users can filter by database, search, etc.

## Blocking and Disconnecting

### Unsubscribe

- Stop syncing with the database
- Keep stored events for now (can view history)
- Can resubscribe later and resume sync

### Block

- Stop syncing with the database
- Delete all events from that database
- Broadcast `Disavow` events for any recs you revouched from them
- Add to blocklist (won't accidentally resubscribe)

### Disavow (Single Rec)

- Append a `Disavow` event to your database
- If you had revouched it, subscribers see the retraction
- Original content remains tombstoned

## V1 Scope

### Must Have
- [ ] Local rec CRUD (create, view, edit via disavow + new rec)
- [ ] Single default database per user
- [ ] SQLite persistence (event log + materialized view)
- [ ] Basic subscription management (subscribe, unsubscribe)
- [ ] Rec and Revouch events
- [ ] Disavow events
- [ ] Simple sync over WebSocket relay

### Deferred
- Multiple databases per user
- E2E encryption (use TLS for relay connection initially)
- Multi-device sync for same user
- Reactions
- Tags/categories for recs
- Multi-hop trust queries ("friends of friends")
- Block semantics
- Key rotation / device compromise recovery
- Relay-hosted storage for availability

## Future Considerations

### Subject Identity and Linking

#### Embrace Fuzziness, Nudge Toward Convergence

A core UX philosophy: humans are fuzzy, and that's okay.

"That Burger Place" is a perfectly valid subject if your friend group knows what it means. Context is implicit in your network. We don't need global entity resolution—we need to make it easy for humans to converge *when they want to*.

**Creation UX nudges toward reuse:**
- Autocomplete from your subscribed recs' subjects as you type
- Pre-fill link data when you select an existing subject
- Still allow freeform entry—sometimes the informal name *is* the right name

**Viewing UX uses progressive disclosure:**
- Casual: "Related: 3 recs mention this" (collapsed)
- Curious: Click to expand and see related recs
- Power user: Trace the full connection graph

#### Links as Recs

Links between subjects are themselves recs—first-class content with reasons and attribution.

Example: Your friend group is tracking a scammer who keeps setting up fake Etsy shops. People rec individually about Shop X, Shop Y, etc. When someone discovers they're connected:

> "I'm pretty sure **Shop X** and **Shop Y** are the same person because I found matching PayPal accounts"

This linking rec:
- References the original recs (auto-linked)
- Has a reason explaining *why* they're connected
- Can be revouched, disavowed, or reacted to like any rec
- Creates backlinks: viewing Shop X shows "mentioned in 2 other recs"

### Tags and Categories

```rust
struct RecommendationContent {
    subject: String,
    body: String,
    tags: Vec<String>,  // User-defined: ["restaurant", "thai", "portland"]
}
```

Query: "Show me restaurant recs from climbing friends"

### Sensitive Recs

Some recs shouldn't sync broadly:
- Medical providers
- Legal services
- Personal matters

With the database model, this is straightforward: create a separate database with limited subscribers. No per-rec ACLs needed.

### Multi-Hop Trust

The data model supports this naturally:
- Direct rec = 1 hop
- Revouch from subscription = 2 hops
- Revouch of revouch = 3 hops

Following the retweet model: display shows original author and closest revoucher. Full chain available on demand but not prominently displayed.

### Rec as Text + Optional Metadata

An idea worth exploring: what if a rec is fundamentally just text, with everything else as optional typed metadata?

```rust
struct Rec {
    text: String,  // The only required thing - what did you actually say?
    metadata: Vec<(String, MetadataValue)>,  // Optional structured data
}

enum MetadataValue {
    Text(String),           // subject, notes, etc.
    Link(ContentHash),      // Reference to another rec
    Location(Address),      // For map features
    Image(ImageRef),        // For galleries
}
```

This solves several problems:
- **Linking recs aren't special** — they're just recs with `Link` metadata
- **Regular recs aren't forced into structure** — "That Burger Place" doesn't need a formal address
- **UI renders metadata smartly** — maps for locations, backlinks for references
- **Core stays simple** — the rec is what you said, everything else is enhancement

### Content-Addressable Storage for Heavy Content

For text recs, we inline content everywhere—duplication is cheap. But heavy content like images changes the calculus.

**Approach:**
- Add `DefineContent` events to the event log for heavy content
- Events reference images by hash
- Content store is a projection from `DefineContent` events
- Images load lazily; missing content shows placeholder until fetched

```rust
enum VouchEventPayload {
    DefineContent { hash: ContentHash, content: ContentBlob },
    UpdateDatabase { name: String, picture: Option<ContentHash> },
    Rec(RecommendationContent),
    // ...
}
```

**For v1:** Skip images entirely, or use external URLs.

### Collaborative Databases

Future extension: databases with multiple authorized writers.

A friend group could have a shared "Our Picks" database where multiple people can publish. Auth becomes "who has write keys?" This extends naturally from the single-owner model.

### Database Forking

See a database you like but want to curate it? Fork it (revouch everything into a new database), add your own commentary, publish your version.

## References

- [Petnames Paper](https://files.spritely.institute/papers/petnames.html) - Humane decentralized naming
- [LiveStore](https://docs.livestore.dev/evaluation/how-livestore-works/) - Event sourcing + reactive SQLite
- [CRDTs](https://crdt.tech/) - Conflict-free replicated data types
- [Automerge](https://automerge.org/) - CRDT library with good documentation
- [Matrix Encryption](https://matrix.org/docs/matrix-concepts/end-to-end-encryption/) - Olm/Megolm for future E2EE