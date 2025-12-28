# Vouch Architecture

Vouch is a local-first, privacy-preserving database of recommendations by you and your trusted friends.

## Overview

Vouch enables users to:
- Create and manage recommendations locally
- Connect with other Vouch users via E2E encrypted channels
- Share recommendations with specific contacts or all contacts
- Reshare (revouch) recommendations received from others
- Disconnect from or block contacts and disavow their recommendations

The system is built on three pillars:
1. **Local-first**: Your data lives on your device. The network is for sync, not storage.
2. **Privacy by design**: E2E encryption for all sync. You control what you share.
3. **Trust through relationships**: Recommendations flow through your personal network, not algorithms.

## Core Concepts

### Petnames

User identity follows the [petname system](https://files.spritely.institute/papers/petnames.html):

- **Identifier**: An opaque, globally unique, decentralized identifier (e.g., a public key or UUID)
- **Petname**: A local, human-readable name you assign to a contact (e.g., "Mom", "College Roommate")
- **Self-proposed name**: The name a user broadcasts as their preferred display name

You see your petnames. Others see theirs. There's no global namespace to fight over.

```
┌─────────────────────────────────────────────────┐
│  Your View          │  Alice's View             │
├─────────────────────┼───────────────────────────┤
│  "Mom" ──────────── │ ──────────── "Me"         │
│  "Alice" ────────── │ ──────────── "Best Friend"│
│  "Thai Place Guy" ─ │ ─────────── "Valued Cust" │
└─────────────────────────────────────────────────┘
        Same identifiers, different petnames
```

### Convergent Event Sourcing

Vouch uses event sourcing with **eventually consistent** semantics:

- All state changes are captured as immutable events
- Events are replicated across devices and contacts
- When any two nodes see the same set of events, they converge to the same state
- Order of event arrival doesn't matter—only the final set

This is achieved through:
- **Monotonic operations**: Events only add information (vouches, reactions) or mark things as invalid (disavowals via tombstones)
- **Content-addressed data**: Recommendations are identified by their content hash, enabling deduplication
- **Tombstone strategy**: Disavowals are stored even if the target event hasn't arrived yet

## Data Model

### Content Hash as Primary Identifier

Recommendations are identified by the hash of their content:

```rust
/// The actual recommendation content, hashed for identity
struct RecommendationContent {
    subject: String,        // The entity being discussed
    recommendation: String, // The actual recommendation 
}

/// A content hash uniquely identifies recommendation content
/// ContentHash = hash(subject, recommendation)
struct ContentHash(Vec<u8>);
```

This enables:
- **Deduplication**: Same recommendation from multiple contacts is recognized as identical
- **Efficient sync**: "Do you have content ABC?" rather than replaying full content
- **Stable references**: Reactions and disavowals reference content that won't change

### User Identity

```rust
/// Opaque, globally unique identifier (e.g., public key)
struct UserIdentifier(Vec<u8>);

/// A contact in your local address book
struct Contact {
    identifier: UserIdentifier,
    petname: String,  // Your local name for them
    self_proposed_name: Option<String>,  // What they call themselves
}
```

### Events

All state changes flow through the event system:

```rust
/// A single event in a contact connection
struct VouchEvent {
    /// Who authored this event
    author: UserIdentifier,
    /// Per-author ordering for sync
    sequence: u64,
    /// When this event was created
    timestamp: Timestamp,
    /// The actual event payload
    payload: VouchEventPayload,
}

enum VouchEventPayload {
    /// A new recommendation you're making
    Vouch(RecommendationContent),
    
    /// Resharing someone else's recommendation
    Revouch {
        original_author: UserIdentifier,
        content: RecommendationContent,  // Embedded for simplicity
    },
    
    /// Marking a recommendation as retracted/untrusted
    Disavow {
        author: UserIdentifier,      // Whose recommendation
        content_hash: ContentHash,   // Which one
    },
    
    /// A reaction to a recommendation
    React {
        author: UserIdentifier,
        content_hash: ContentHash,
        reaction: Reaction,
    },
}

/// Simple emoji reactions
enum Reaction {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Heart,
    // etc.
}
```

### Contact Connections

Each contact connection has a single shared event log—the conversation between you:

```rust
/// The sync state with a single contact
struct ContactConnection {
    contact: UserIdentifier,
    
    /// All events in this connection (both yours and theirs)
    events: Vec<VouchEvent>,
    
    /// Sync state: highest sequence we've sent that they've acknowledged
    my_acked_sequence: u64,
    
    /// Sync state: highest sequence we've processed from them
    their_processed_sequence: u64,
}
```

The event log is a unified view of the conversation. Each event is tagged with its `author`, and sequence numbers are per-author for sync purposes.

## Sync Protocol

### Transport Abstraction

The sync layer is transport-agnostic. Implementation can be swapped without changing application logic:

```rust
#[async_trait]
trait ContactTransport {
    /// Send events to a contact
    async fn send_events(
        &self,
        contact: &UserIdentifier,
        events: Vec<VouchEvent>,
    ) -> Result<()>;
    
    /// Receive incoming events (blocks until available)
    async fn receive_events(&self) -> Result<(UserIdentifier, Vec<VouchEvent>)>;
    
    /// Request events from a contact starting from a sequence number
    async fn request_events_since(
        &self,
        contact: &UserIdentifier,
        since_sequence: u64,
    ) -> Result<Vec<VouchEvent>>;
    
    /// Acknowledge receipt of events up to a sequence number
    async fn acknowledge(
        &self,
        contact: &UserIdentifier,
        up_to_sequence: u64,
    ) -> Result<()>;
}
```

### Transport Options

| Phase | Transport | Notes |
|-------|-----------|-------|
| **v1 (now)** | Custom relay server | Simple WebSocket server, routes by user ID |
| **v2** | Add E2EE | Olm/Megolm libraries, still custom relay |
| **Future** | Matrix federation | If we want decentralized relays |
| **Future** | P2P (libp2p) | For true serverless sync |

### Sync Flow

```
┌─────────┐                    ┌─────────┐
│  You    │                    │  Alice  │
└────┬────┘                    └────┬────┘
     │                              │
     │  1. New Vouch event          │
     │  (your sequence: 47)         │
     ├─────────────────────────────►│
     │                              │
     │  2. ACK your sequence 47     │
     │◄─────────────────────────────┤
     │                              │
     │  3. Alice's new Revouch      │
     │  (her sequence: 23)          │
     │◄─────────────────────────────┤
     │                              │
     │  4. ACK her sequence 23      │
     ├─────────────────────────────►│
     │                              │
```

### Handling Offline / Reconnection

When reconnecting after being offline:

1. Exchange current sequence numbers: "I've seen up to your sequence 45"
2. Each side sends events the other hasn't seen
3. Process incoming events, update local state
4. Acknowledge receipt

Events are idempotent—receiving the same event twice is harmless.

### Tombstone Handling

When a `Disavow` event arrives before the target `Vouch`:

1. Store the tombstone: `(author, content_hash) → tombstoned`
2. When the `Vouch` eventually arrives, check tombstone set
3. If tombstoned, mark as disavowed immediately

This ensures convergence regardless of event arrival order.

## Local Storage

### Event Log (Source of Truth)

Events are persisted to SQLite as the authoritative record:

```sql
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    contact_id BLOB NOT NULL,         -- Which contact connection
    author_id BLOB NOT NULL,          -- Who authored this event
    sequence INTEGER NOT NULL,        -- Per-author sequence number
    timestamp INTEGER NOT NULL,
    event_type TEXT NOT NULL,
    payload BLOB NOT NULL,            -- JSON or msgpack
    UNIQUE(contact_id, author_id, sequence)
);

CREATE TABLE tombstones (
    author_id BLOB NOT NULL,
    content_hash BLOB NOT NULL,
    tombstoned_at INTEGER NOT NULL,
    PRIMARY KEY (author_id, content_hash)
);

CREATE TABLE sync_state (
    contact_id BLOB PRIMARY KEY,
    my_acked_sequence INTEGER NOT NULL DEFAULT 0,
    their_processed_sequence INTEGER NOT NULL DEFAULT 0
);
```

### Materialized View (Query Layer)

Events are projected into a queryable format:

```sql
CREATE TABLE recommendations (
    content_hash BLOB PRIMARY KEY,
    subject TEXT NOT NULL,
    recommendation TEXT NOT NULL,
    first_seen_at INTEGER NOT NULL
);

CREATE TABLE recommendation_sources (
    content_hash BLOB NOT NULL,
    author_id BLOB NOT NULL,
    is_revouch BOOLEAN NOT NULL,
    original_author_id BLOB,          -- NULL if not a revouch
    received_at INTEGER NOT NULL,
    is_disavowed BOOLEAN NOT NULL DEFAULT FALSE,
    PRIMARY KEY (content_hash, author_id)
);

CREATE TABLE reactions (
    content_hash BLOB NOT NULL,
    author_id BLOB NOT NULL,
    reactor_id BLOB NOT NULL,
    reaction TEXT NOT NULL,
    reacted_at INTEGER NOT NULL,
    PRIMARY KEY (content_hash, author_id, reactor_id)
);

CREATE TABLE contacts (
    identifier BLOB PRIMARY KEY,
    petname TEXT NOT NULL,
    self_proposed_name TEXT,
    connection_status TEXT NOT NULL,  -- 'connected', 'disconnected', 'blocked'
    created_at INTEGER NOT NULL
);
```

### Reactivity

The UI subscribes to query results. When events arrive and update the materialized view, affected queries re-run and the UI updates automatically. This follows the [LiveStore pattern](https://docs.livestore.dev/evaluation/how-livestore-works/).

## Blocking and Disconnecting

### Disconnect

- Stop syncing with the contact
- Keep their historical recommendations visible
- No events broadcast to other contacts
- Can reconnect later and resume sync

### Block

- Stop syncing with the contact
- Filter all their content from your view (query-time filter, data retained)
- Broadcast `Disavow` tombstones for any content you revouched from them
- Your other contacts see you've retracted those reshares

### Disavow (Single Recommendation)

- Broadcast a `Disavow` event to your contacts
- If you had revouched it, your contacts see the retraction
- Original content remains in event log (tombstoned)

## V1 Scope

For the initial implementation, focus on:

### Must Have
- [ ] Local recommendation CRUD (create, view, edit, delete)
- [ ] SQLite persistence (event log + materialized view)
- [ ] Basic contact management (add, remove, petnames)
- [ ] Simple sync over WebSocket relay
- [ ] Vouch and Revouch events
- [ ] Disavow events (single recommendation)

### Deferred
- E2E encryption (use TLS for relay connection initially)
- Multi-device sync for same user
- Reactions
- Tags/categories for recommendations
- Multi-hop trust queries ("friends of friends")
- Block (vs disconnect) semantics
- Key rotation / device compromise recovery
- Federation / P2P transport

## Future Considerations

### Subject Identity and Linking

#### Embrace Fuzziness, Nudge Toward Convergence

A core UX philosophy: humans are fuzzy, and that's okay.

"That Burger Place" is a perfectly valid subject if your friend group knows what it means. Context is implicit in your network. We don't need global entity resolution—we need to make it easy for humans to converge *when they want to*.

**Creation UX nudges toward reuse:**
- Autocomplete from your network's existing subjects as you type
- Pre-fill link data when you select an existing subject
- Still allow freeform entry—sometimes the informal name *is* the right name

**Viewing UX uses progressive disclosure:**
- Casual: "Related: 3 vouches mention this" (collapsed)
- Curious: Click to expand and see linking vouches
- Power user: Trace the full connection graph

#### Links as Vouches

Links between subjects are themselves vouches—first-class content with reasons and attribution.

Example: Your friend group is tracking a scammer who keeps setting up fake Etsy shops. People vouch individually about Shop X, Shop Y, etc. When someone discovers they're connected:

> "I'm pretty sure **Shop X** and **Shop Y** are the same person because I found matching PayPal accounts"

This linking vouch:
- References the original vouches (auto-linked, like Wikipedia inline links)
- Has a reason explaining *why* they're connected
- Can be revouched, disavowed, or reacted to like any vouch
- Creates backlinks: viewing Shop X shows "mentioned in 2 other vouches"

This approach requires no special linking infrastructure—it emerges from vouches that reference other vouches. The graph is built collaboratively, with full attribution and reasoning at every node.

### Tags and Categories

```rust
struct RecommendationContent {
    subject: String,
    recommendation: String,
    tags: Vec<String>,  // User-defined: ["restaurant", "thai", "portland"]
}
```

Query: "Show me restaurant recommendations from climbing friends"

### Sensitive Recommendations

Some recommendations shouldn't sync freely:
- Medical providers
- Legal services  
- Personal matters

This should be handled by only producing events for a specified audience (All or some selection of contacts). That should be recorded seperately, considered as a projection and cache of data stored in individual event logs

### Multi-Hop Trust

The data model supports this naturally:
- Direct vouch = 1 hop
- Revouch from friend = 2 hops
- Revouch of revouch = 3 hops

We should follow the twitter retweet model, where you can only observe the original vouch and the closest revouch. Disclosing the chain is unnecessary.

### Vouch as Text + Optional Metadata

An idea worth exploring: what if a vouch is fundamentally just text, with everything else as optional typed metadata?

```rust
struct Vouch {
    text: String,  // The only required thing - what did you actually say?
    metadata: Vec<(String, MetadataValue)>,  // Optional structured data
}

enum MetadataValue {
    Text(String),           // subject, notes, etc.
    Link(ContentHash),      // Reference to another vouch
    Location(Address),      // For map features
    Image(ImageRef),        // For galleries
    // etc.
}
```

This solves several problems:
- **Linking vouches aren't special** — they're just vouches with `Link` metadata
- **Regular vouches aren't forced into structure** — "That Burger Place" doesn't need a formal address
- **UI renders metadata smartly** — maps for locations, backlinks for references, galleries for images
- **Core stays simple** — the vouch is what you said, everything else is enhancement

The current `subject` + `recommendation` fields would become optional metadata rather than required structure. Worth deeper thought.

## References

- [Petnames Paper](https://files.spritely.institute/papers/petnames.html) - Humane decentralized naming
- [LiveStore](https://docs.livestore.dev/evaluation/how-livestore-works/) - Event sourcing + reactive SQLite
- [CRDTs](https://crdt.tech/) - Conflict-free replicated data types
- [Automerge](https://automerge.org/) - CRDT library with good documentation
- [Matrix Encryption](https://matrix.org/docs/matrix-concepts/end-to-end-encryption/) - Olm/Megolm for future E2EE
