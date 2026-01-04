# Vouch Architecture

Vouch is a local-first, privacy-preserving database of recommendations by you and your trusted friends.

## Overview

Vouch enables users to:
- Create and manage recommendations in their own database(s)
- Subscribe to others' databases to see their recommendations
- Vouch for (rehost) recommendations from subscriptions into their own database
- Maintain full privacy from relay operators and network observers

The system is built on four pillars:

1. **Local-first**: Your data lives on your device. The network is for sync
2. **Privacy by design**: E2E encryption is the foundation
3. **Trust through relationships**: Recommendations flow through your personal network
4. **Invite-only**: No public discovery. All access is explicitly granted out-of-band.

## Core Concepts

### Database = Identity

There is no separate "user" concept at the network level. Each database IS an identity:

- One database = one keypair = one public identity
- `DatabaseId` is the public key itself (following Signal's model)
- Alice's "Food Recommendations" and Alice's "Sports Takes" are separate identities on the network
- They're only connected in Alice's local app because she manages both keypairs
- Others cannot tell these databases belong to the same person

```rust
/// A database is an identity.
#[derive(Clone, PartialEq, Eq, Hash)]
struct DatabaseId(PublicKey);

/// A database you know about (local or remote)
struct Database {
    /// The identity (public key)
    id: DatabaseId,

    /// Private key for signing (only present if we own this database)
    signing_key: Option<SigningKey>,

    /// Symmetric key for E2EE (from invitation or self-generated)
    event_key: SymmetricKey,

    /// Metadata from latest UpdateDatabase event
    name: String,
}
```

### Events

All state changes flow through the event system. Every event is signed by the database owner.

```rust
/// A single event in a database's append-only log
struct VouchEvent {
    /// Which database this event belongs to
    database_id: DatabaseId,

    /// Monotonically increasing sequence number
    sequence: u64,

    /// When this event was created
    timestamp: Timestamp,

    /// The actual event data
    payload: DatabaseEvent,
}

/// A signed event ready for transmission or storage
struct SignedVouchEvent {
    /// The event content
    event: VouchEvent,

    /// Signature over the event
    /// Verifiable using event.database_id (which IS the public key)
    signature: Signature,
}

/// Event payloads
enum DatabaseEvent {
    /// A new recommendation you're making
    Recommendation(RecommendationContent),

    /// Vouching for someone else's recommendation
    Vouch {
        /// The database this recommendation came from (also the verification key)
        source_database: DatabaseId,
        /// Original author's signature (proves authenticity)
        original_signature: Signature,
        /// The recommendation content
        content: RecommendationContent,
        /// What we claim the source database is called (for display)
        source_name: Option<String>,
    },

    /// Update database metadata (name, description)
    UpdateDatabase {
        name: String,
        description: Option<String>,
    },

    /// Mark a recommendation as retracted/disavowed
    Disavow {
        /// Which database's recommendation we're disavowing
        database_id: DatabaseId,
        /// Which event (by sequence) we're disavowing
        sequence: u64,
    },
}

/// Recommendation content
#[non_exhaustive]
enum RecommendationContent {
    /// Simple text recommendation
    Simple {
        /// The entity being recommended (restaurant, book, person, etc.)
        subject: String,
        /// The recommendation text
        body: String,
    },
}
```

### The Four-Name Model

Each database identity has four types of names, a petname system:

**1. Identifier (DatabaseId)**
- The public key itself
- Globally unique, cryptographically verifiable, permanent
- Not human-readable

**2. Self-Proposed Name**
- Set by database owner via `UpdateDatabase` events
- Cryptographically signed (part of event log)
- Verifiable by anyone who fetches the events
- This is the authoritative name from the source

**3. Proposed Names**
- Names claimed by others in vouches (`source_name` field)
- Unverified until you fetch from the source
- Could be stale or malicious

**4. Petname**
- Your personal, private name for a database
- Never leaves your device
- Takes precedence in your UI
- You control what you call things

```rust
/// Local-only naming data for a database
struct DatabaseNaming {
    /// Your personal name (never transmitted)
    petname: Option<String>,

    /// Verified name from UpdateDatabase events (if fetched)
    verified_name: Option<String>,

    /// Names proposed by others in vouches
    /// Sorted by recency
    proposed_names: Vec<ProposedName>,
}

struct ProposedName {
    name: String,
    proposed_by: DatabaseId,
    seen_at: Timestamp,
}
```

**Name Resolution:**

```rust
fn display_name(db: &Database, naming: &DatabaseNaming) -> String {
    if let Some(petname) = &naming.petname {
        return petname.clone();
    }

    if let Some(verified) = &naming.verified_name {
        return format!("{} ✓", verified);
    }

    if let Some(proposed) = naming.proposed_names.first() {
        return format!("{} (unverified)", proposed.name);
    }

    format!("Unknown ({}...)", db.id.short())
}
```

### Subscriptions

A subscription is local state tracking your sync relationship with a remote database.

```rust
/// Subscription state for a remote database
struct Subscription {
    /// The database you're subscribed to
    database_id: DatabaseId,

    /// Highest sequence number you've received
    last_synced_sequence: u64,
}
```

**Key distinction:** You can know about a database (have a `Database` entry with naming info) without being subscribed to it. This happens when you see a vouch referencing a database you don't follow.

### Vouch Semantics

Vouching is both an endorsement AND a durability decision:

| Action | Meaning | Where it lives | Durability |
|--------|---------|----------------|------------|
| **Subscribe** | "I want to see this" | Their database (synced locally) | Depends on source |
| **Vouch** | "I endorse this AND host it" | Your database | You control |

**Verification chain:**
1. Alice creates a recommendation, signs with her private key
2. Bob subscribes to Alice's database, sees her recommendation
3. Bob vouches for it into his database, including Alice's original signature
4. Carol subscribes to Bob's database and verifies:
   - Bob's signature on the vouch event ✓
   - Alice's original signature using `source_database` as the public key ✓
5. Carol now has cryptographic proof of the entire chain

## Data Model

### Primary Identifier

Recommendations are identified by `(DatabaseId, sequence)`:

```rust
/// Unique identifier for a recommendation
struct RecommendationId {
    database_id: DatabaseId,
    sequence: u64,
}
```

### Local Storage

All local data is persisted in a SQLite database. Conceptually, local storage has three layers:

**1. Event Log (Source of Truth)**

```rust
/// Stored events from all databases (local and subscribed)
struct StoredEvent {
    /// The decrypted and verified event
    event: SignedVouchEvent,

    /// Encrypted payload (as received from relay, for retransmission)
    encrypted_payload: Vec<u8>,

    /// When we received this event
    received_at: Timestamp,
}
```

**2. Database Registry**

```rust
/// All databases we know about
struct DatabaseEntry {
    id: DatabaseId,

    /// E2EE key (from invitation or self-generated)
    event_key: SymmetricKey,

    /// Current metadata (from latest UpdateDatabase)
    name: String,

    /// Local naming
    petname: String,
    verified_name: Option<String>,
    proposed_names: Vec<ProposedName>,

    created_at: Timestamp,

    /// Subscription state (None if not subscribed)
    subscription: Option<Subscription>,
}
```

**3. Materialized Views (Query Layer)**

Events are projected into queryable structures for the UI:

```rust
/// A rec ready for display
struct Recommendation {
    /// Primary identifier
    id: RecommendationId,

    /// The content
    content: RecommendationContent,

    /// When it was created
    timestamp: Timestamp,

    /// Source database
    database_id: DatabaseId,

    /// If this is a vouch, the original source
    original_source: Option<DatabaseId>,

    /// Is this disavowed?
    disavowed_by: Vec<DatabaseId>,
}
```

### Convergent Event Sourcing

Vouch uses event sourcing with eventually consistent semantics:

- All state changes are captured as immutable events
- Events are replicated across devices via subscriptions
- When any two nodes see the same set of events, they converge to the same state
- Order of event arrival doesn't matter—only the final set

This is achieved through:
- **Monotonic operations**: Events only add information or mark things as invalid
- **Sequence numbers**: Events within a database are totally ordered
- **Tombstone strategy**: Disavowals are stored even if the target event hasn't arrived yet

## Encryption & Privacy

### Threat Model

**Attackers we protect against:**
- Relay operator trying to read content
- Relay operator trying to map social graph
- Passive network observer (ISP, corporate firewall)
- Compromised relay attempting traffic analysis
- Byzantine faults from subscribed databases.

**Attack vectors prevented:**
- ✅ Content reading (E2EE)
- ✅ Content tampering (signatures)
- ✅ Publisher impersonation (signature verification)

### Per-Database Symmetric Keys

Each database has a symmetric key shared among all authorized subscribers:

```rust
/// Keys for a database
struct DatabaseKeys {
    /// The identity (public key)
    id: DatabaseId,

    /// Symmetric key for encrypting all events
    /// Shared via invitation
    event_key: SymmetricKey,

    /// Signing key (only for owned databases)
    signing_key: Option<SigningKey>,
}
```

**Encryption flow (publishing):**
```
VouchEvent (unsigned)
  → Sign with signing_key → SignedVouchEvent
  → Encrypt with event_key → EncryptedEvent
  → Publish to relay (relay verifies ownership via signature challenge)
```

**Decryption flow (subscribing):**
```
EncryptedEvent from relay
  → Decrypt with event_key → SignedVouchEvent
  → Verify signature using database_id (the public key)
  → Store as StoredEvent
  → Update materialized views
```

```rust
/// An encrypted event for transmission over the relay
struct EncryptedEvent {
    /// Which database (needed for routing)
    database_id: DatabaseId,

    /// Encrypted SignedVouchEvent
    ciphertext: Vec<u8>,
}
```

### Relay Authentication (V1)

For v1, only the database owner can publish events. The relay verifies this via signature challenge:

**Publishing flow:**
1. Client connects to relay, requests to publish to database X
2. Relay sends a random challenge nonce
3. Client signs nonce with their signing key
4. Relay verifies signature using DatabaseId (which IS the public key)
5. If valid, relay accepts the encrypted events

**What the relay learns:**
- Which database is being published to
- That the publisher owns that database (since DatabaseId = PublicKey)
- When events are published

**What the relay cannot learn:**
- Event content (encrypted with event_key)
- Who subscribes to what (fetch requests don't require auth)

**Note:** Since DatabaseId = owner's public key and only owners publish, the relay inherently knows "the owner of database X published." True publisher anonymity would require anonymous credentials, which is deferred to a future version with multi-writer databases.

### What the Relay Can and Cannot Do (V1)

**Cannot:**
- ❌ Read event content (encrypted with event_key)
- ❌ Tamper with events (clients verify signatures)
- ❌ Impersonate publishers (signature challenge auth)

**Can:**
- ✅ Know which databases exist
- ✅ Know who owns each database (DatabaseId = PublicKey)
- ✅ Know when owners publish events
- ✅ Know rough activity levels per database
- ✅ See who fetches events (IP addresses)
- ✅ Enforce rate limits per database
- ✅ Delete old events (retention policy)
- ✅ Block entire databases (for abuse)

## Sync Protocol

### Invitations

All access is granted via out-of-band invitation. No in-app discovery.

```rust
/// Database invitation
struct DatabaseInvite {
    /// The database identity (also the verification key)
    database_id: DatabaseId,

    /// Symmetric key for decrypting events
    event_key: SymmetricKey,

    /// Where to fetch events
    relay_url: String,

    /// Optional: who's inviting you (for display)
    inviter_name: Option<String>,

    /// Optional: single-use relay access token
    access_token: Option<AccessToken>,

    /// Optional: expiration
    expires_at: Option<Timestamp>,
}

// Serialized as URL or QR code:
// vouch://invite?db=<base64>&key=<base64>&relay=<url>&...
```

**Invitation flows:**

*link (Signal, email):*
1. Alice generates invite link: `vouch://invite?...`
2. Alice sends via Signal/WhatsApp/email
3. Bob clicks link → Opens Vouch app
4. Same subscription flow

### Relay Protocol

```rust
#[async_trait]
trait VouchRelay {
    /// Authenticate as database owner (signature challenge)
    async fn authenticate(
        &self,
        database_id: DatabaseId,
        challenge_response: Signature,
    ) -> Result<AuthToken>;

    /// Publish encrypted events (requires auth)
    async fn publish(
        &self,
        auth: AuthToken,
        events: Vec<EncryptedEvent>,
    ) -> Result<()>;

    /// Fetch events since a sequence number (no auth required)
    async fn fetch_events(
        &self,
        database_id: DatabaseId,
        since_sequence: u64,
    ) -> Result<Vec<EncryptedEvent>>;
}
```

### Sync Flow

**Publishing (your databases):**
1. Create event locally, sign it, append to your database
2. Encrypt with database event_key
3. Authenticate with relay (signature challenge)
4. Publish encrypted events
5. Subscribers pull on their own schedule

**Subscribing (others' databases):**
1. Request events since your last synced sequence
2. Decrypt with event_key (from invitation)
3. Verify signatures using database_id
4. Store events locally
5. Update materialized views
6. Update sync state

**Handling offline:**
1. Work with locally stored data while offline
2. On reconnect, fetch missed events from subscribed databases
3. Publish any events you created while offline
4. Events are idempotent—receiving duplicates is harmless

### Tombstone Handling

When a `Disavow` event arrives before the target event:

1. Store the tombstone: `(target_database, target_sequence) → tombstoned`
2. When the target event eventually arrives, check tombstone set
3. If tombstoned, mark as disavowed immediately

This ensures convergence regardless of event arrival order.

## Key Management

### Single Key Model (V1)

Following Signal's approach, each database has exactly one keypair:

```rust
struct Database {
    /// DatabaseId IS the public key
    id: DatabaseId,

    /// Private key for signing (only for owned databases)
    signing_key: Option<SigningKey>,
}
```

**Verification is straightforward:**
```rust
fn verify_event(signed: &SignedVouchEvent) -> Result<()> {
    // The database_id IS the public key
    let public_key = &signed.event.database_id;

    // Verify signature over the event content
    verify_signature(&signed.signature, &signed.event, public_key)
}
```

### Key Backup

Since keys cannot be rotated in v1, backup is essential.

**BIP39 Mnemonic:**
```rust
impl SigningKey {
    fn to_mnemonic(&self) -> String {
        // Convert 32-byte key to 24-word mnemonic
        bip39::encode(self.secret_bytes())
    }

    fn from_mnemonic(words: &str) -> Result<Self> {
        let bytes = bip39::decode(words)?;
        SigningKey::from_bytes(bytes)
    }
}
```

**UX flow:**
```
[Create Database]
→ Generate keypair
→ Show backup screen:
  "Your database identity is tied to this key. Back it up!"

  [Show Recovery Phrase]
  → Display 24 words
  → "Write these down and store safely"
  → [ ] I've written down my recovery phrase
  → [Continue]
```

### Trust On First Use (TOFU)

When you first encounter a database, you learn its public key and trust it:

```rust
fn process_invitation(invite: DatabaseInvite) -> Result<()> {
    // The database_id IS the public key - no separate trust step needed
    // Just store and use it for verification

    databases.insert(DatabaseEntry {
        id: invite.database_id,
        event_key: invite.event_key,
        // ...
    });

    Ok(())
}
```

Since `DatabaseId` is the public key itself, TOFU is trivial—you can't have a mismatch.

### Key Compromise (V1 Mitigation)

If a private key is compromised:
1. User has lost control of that database identity
2. Must create new database with new key
3. Publish farewell message in compromised database
4. Share new invite links via out-of-band channels
5. Subscribers manually migrate

## Security Considerations

### Cryptographic Primitives

- **Symmetric encryption**: ChaCha20-Poly1305 for event payloads
- **Signatures**: Ed25519 for event signing
- **Key derivation**: HKDF for deriving keys
- **Secure random**: OS-provided CSPRNG

### Key Storage

- **iOS**: Keychain with `kSecAttrAccessibleWhenUnlocked`
- **Android**: Android Keystore with `ENCRYPT` purpose
- **Desktop**: OS keychain (macOS Keychain, Windows Credential Manager)

### Known Limitations

**Not protected against:**
- Device compromise (malware can read local database)
- Coerced disclosure (can't deny you have the data)
- Traffic analysis by nation-state adversaries
- Social engineering (user tricked into inviting attacker)
- Screenshots/screen recording
- Quantum computers (pre-quantum crypto)

**When to use Vouch:**
- ✅ Protecting against curious relay operators
- ✅ Protecting against corporate surveillance
- ✅ Protecting against passive network monitoring
- ❌ Against nation-state adversaries (use Tor + Tails)
- ❌ Against device seizure (use full-disk encryption)

## Roadmap

### V1 Scope

**Must have:**
- Local rec CRUD (create, view, disavow)
- Single database per user (simplify multi-database for later)
- Event log + materialized view persistence
- Basic subscription management (subscribe, unsubscribe)
- Rec and Vouch events
- Disavow events
- E2EE with per-database symmetric keys
- Invite-only subscriptions (QR code + links)
- Simple sync over WebSocket relay
- Four-name model (petnames, verified names, proposed names)
- Mnemonic phrase backup

See [ROADMAP.md](./ROADMAP.md) for V2+ enhancements.

## Terminology

| Term | Meaning |
|------|---------|
| **Rec** | A recommendation—the core content unit |
| **Database** | An event log with a single keypair identity |
| **DatabaseId** | The public key of a database (IS the identity) |
| **Subscription** | Following a database to replicate its events locally |
| **Vouch** | Endorsing someone else's recommendation into your own database (endorsement + durability) |
| **Petname** | Your local, private name for a database |
| **Disavow** | Mark a rec as retracted/untrusted |
| **Event** | An immutable, signed state change in a database |
| **Relay** | Store-and-forward server for syncing events |

## References

- [Petnames Paper](https://files.spritely.institute/papers/petnames.html) - Humane decentralized naming
- [Signal Protocol](https://signal.org/docs/) - Identity keys, encryption
- [SSB Identity](https://handbook.scuttlebutt.nz/concepts/identity) - Single key model
