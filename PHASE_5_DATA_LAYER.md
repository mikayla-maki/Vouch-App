# Phase 5: Data Layer Implementation Plan

## Overview

This phase implements the foundational data layer for Vouch: an **event-sourced** architecture with SQLite storage, cryptographic signing, and the foundation for multi-database sync.

**Scope**: Local CRUD for recommendations in your own database, with the data model ready for subscriptions and sync.

See [VOUCH_ARCHITECTURE.md](./VOUCH_ARCHITECTURE.md) for the full system design.

## Core Data Model

Based on the architecture where **Database = Identity**:

```rust
use ed25519_dalek::{SigningKey, VerifyingKey, Signature};

/// A database identity IS its public key
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct DatabaseId(pub VerifyingKey);

impl DatabaseId {
    /// Short hex representation for display
    pub fn short(&self) -> String {
        hex::encode(&self.0.as_bytes()[..4])
    }
}

/// A database you know about (owned or subscribed)
pub struct Database {
    /// The identity (public key)
    pub id: DatabaseId,

    /// Private key for signing (only present if we own this database)
    pub signing_key: Option<SigningKey>,

    /// Symmetric key for E2EE (from invitation or self-generated)
    pub event_key: [u8; 32],

    /// Current metadata (from latest UpdateDatabase event)
    pub name: String,
    pub description: Option<String>,

    /// When we first learned about this database
    pub created_at: u64,
}

/// Local naming for a database (four-name model)
pub struct DatabaseNaming {
    pub database_id: DatabaseId,

    /// Your private petname (never transmitted)
    pub petname: Option<String>,

    /// Verified name from their UpdateDatabase events
    pub verified_name: Option<String>,

    /// Names proposed by others in vouches
    pub proposed_names: Vec<ProposedName>,
}

pub struct ProposedName {
    pub name: String,
    pub proposed_by: DatabaseId,
    pub seen_at: u64,
}

/// Subscription state for a remote database
pub struct Subscription {
    pub database_id: DatabaseId,
    pub last_synced_sequence: u64,
}
```

## Event Types

Events are the source of truth. Each event is signed by the database owner.

```rust
/// A single event in a database's append-only log
pub struct VouchEvent {
    /// Which database this event belongs to
    pub database_id: DatabaseId,

    /// Monotonically increasing sequence number (per database)
    pub sequence: u64,

    /// When this event was created (unix ms)
    pub timestamp: u64,

    /// The actual event data
    pub payload: DatabaseEvent,
}

/// A signed event ready for storage/transmission
pub struct SignedVouchEvent {
    pub event: VouchEvent,

    /// Signature over the event, verifiable using event.database_id
    pub signature: Signature,
}

/// Event payloads
#[non_exhaustive]
pub enum DatabaseEvent {
    /// A new recommendation
    Recommendation(RecommendationContent),

    /// Vouching for someone else's recommendation
    Vouch {
        /// The database this came from (also verification key)
        source_database: DatabaseId,
        /// Original author's signature
        original_signature: Signature,
        /// The recommendation content
        content: RecommendationContent,
        /// What we claim the source is called
        source_name: Option<String>,
    },

    /// Update database metadata
    UpdateDatabase {
        name: String,
        description: Option<String>,
    },

    /// Mark a recommendation as retracted
    Disavow {
        /// Which database's rec we're disavowing
        database_id: DatabaseId,
        /// Which event (by sequence) we're disavowing
        sequence: u64,
    },
}

/// Recommendation content (extensible)
#[non_exhaustive]
pub enum RecommendationContent {
    Simple {
        /// The entity being recommended
        subject: String,
        /// The recommendation text
        body: String,
    },
}
```

## Database Schema

All local data is persisted in SQLite.

### Event Log Table (Source of Truth)

```sql
-- Events from all databases (owned and subscribed)
CREATE TABLE events (
    -- Local auto-increment for ordering
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Event identification
    database_id BLOB NOT NULL,          -- 32-byte public key
    sequence INTEGER NOT NULL,          -- Per-database sequence

    -- Event data
    timestamp INTEGER NOT NULL,         -- Unix ms
    event_type TEXT NOT NULL,           -- 'recommendation', 'vouch', 'update_database', 'disavow'
    payload TEXT NOT NULL,              -- JSON-encoded payload

    -- Cryptographic proof
    signature BLOB NOT NULL,            -- Ed25519 signature

    -- For retransmission to relay
    encrypted_payload BLOB,             -- ChaCha20-Poly1305 ciphertext

    -- When we received this
    received_at INTEGER NOT NULL,

    UNIQUE(database_id, sequence)
);

CREATE INDEX idx_events_database ON events(database_id);
CREATE INDEX idx_events_database_sequence ON events(database_id, sequence);
CREATE INDEX idx_events_timestamp ON events(timestamp);
```

### Database Registry Table

```sql
-- All databases we know about
CREATE TABLE databases (
    id BLOB PRIMARY KEY,                -- 32-byte public key (DatabaseId)

    -- Keys
    signing_key BLOB,                   -- Private key (only for owned databases)
    event_key BLOB NOT NULL,            -- 32-byte symmetric key for E2EE

    -- Current metadata (from latest UpdateDatabase)
    name TEXT NOT NULL,
    description TEXT,

    -- Local naming (four-name model)
    petname TEXT,                       -- Your private name
    verified_name TEXT,                 -- From their UpdateDatabase events

    -- Ownership
    is_owned INTEGER NOT NULL DEFAULT 0,

    -- Timestamps
    created_at INTEGER NOT NULL,

    -- Subscription state (NULL if not subscribed)
    last_synced_sequence INTEGER,
    relay_url TEXT
);

CREATE INDEX idx_databases_is_owned ON databases(is_owned);
```

### Proposed Names Table

```sql
-- Names proposed by others in vouches
CREATE TABLE proposed_names (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    database_id BLOB NOT NULL,          -- The database being named
    name TEXT NOT NULL,
    proposed_by BLOB NOT NULL,          -- Who proposed this name
    seen_at INTEGER NOT NULL,

    FOREIGN KEY (database_id) REFERENCES databases(id)
);

CREATE INDEX idx_proposed_names_database ON proposed_names(database_id);
```

### Materialized View: Recommendations

```sql
-- Recommendations projected from events
CREATE TABLE recommendations (
    -- Primary key: (database_id, sequence)
    database_id BLOB NOT NULL,
    sequence INTEGER NOT NULL,

    -- Content
    subject TEXT NOT NULL,
    body TEXT NOT NULL,

    -- Source tracking (for vouches)
    original_database_id BLOB,          -- NULL if this is the original
    original_sequence INTEGER,

    -- Timestamps
    timestamp INTEGER NOT NULL,         -- From event

    -- Status
    is_disavowed INTEGER NOT NULL DEFAULT 0,

    PRIMARY KEY (database_id, sequence)
);

CREATE INDEX idx_recommendations_subject ON recommendations(subject);
CREATE INDEX idx_recommendations_timestamp ON recommendations(timestamp);

-- Full-text search
CREATE VIRTUAL TABLE recommendations_fts USING fts5(
    subject,
    body,
    content='recommendations',
    content_rowid='rowid'
);

-- FTS sync triggers
CREATE TRIGGER recommendations_ai AFTER INSERT ON recommendations BEGIN
    INSERT INTO recommendations_fts(rowid, subject, body)
    VALUES (NEW.rowid, NEW.subject, NEW.body);
END;

CREATE TRIGGER recommendations_ad AFTER DELETE ON recommendations BEGIN
    INSERT INTO recommendations_fts(recommendations_fts, rowid, subject, body)
    VALUES ('delete', OLD.rowid, OLD.subject, OLD.body);
END;

CREATE TRIGGER recommendations_au AFTER UPDATE ON recommendations BEGIN
    INSERT INTO recommendations_fts(recommendations_fts, rowid, subject, body)
    VALUES ('delete', OLD.rowid, OLD.subject, OLD.body);
    INSERT INTO recommendations_fts(rowid, subject, body)
    VALUES (NEW.rowid, NEW.subject, NEW.body);
END;
```

### Materialized View: Disavowals

```sql
-- Disavowals (may arrive before target event)
CREATE TABLE disavowals (
    -- Who disavowed
    disavowing_database_id BLOB NOT NULL,
    disavowing_sequence INTEGER NOT NULL,

    -- What was disavowed
    target_database_id BLOB NOT NULL,
    target_sequence INTEGER NOT NULL,

    timestamp INTEGER NOT NULL,

    PRIMARY KEY (disavowing_database_id, disavowing_sequence)
);

CREATE INDEX idx_disavowals_target ON disavowals(target_database_id, target_sequence);
```

### Projection State

```sql
-- Track projection progress for crash recovery
CREATE TABLE projection_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    last_projected_event_id INTEGER NOT NULL DEFAULT 0
);

INSERT INTO projection_state (id, last_projected_event_id) VALUES (1, 0);
```

## Rust Module Structure

```
src/store/
├── mod.rs                    # Public API exports
├── error.rs                  # StoreError enum
├── database.rs               # SQLite connection, migrations
├── event.rs                  # VouchEvent, SignedVouchEvent, DatabaseEvent
├── event_store.rs            # Append and read events
├── projector.rs              # Apply events to materialized views
├── recommendation_store.rs   # Query recommendations
└── database_registry.rs      # Query/manage known databases

src/crypto/
├── mod.rs                    # Crypto exports
├── keys.rs                   # Ed25519 keypair generation, DatabaseId
├── signing.rs                # Sign/verify events
└── encryption.rs             # ChaCha20-Poly1305 encrypt/decrypt
```

## Public API

### Database Initialization

```rust
/// Initialize the database at the given path
pub fn init_database(path: &Path) -> Result<VouchDatabase, StoreError>;

/// In-memory database for testing
pub fn init_memory_database() -> Result<VouchDatabase, StoreError>;
```

### VouchDatabase (High-Level API)

```rust
impl VouchDatabase {
    // === Owned Database Management ===

    /// Create a new owned database (generates keypair)
    pub fn create_database(&self, name: String) -> Result<Database, StoreError>;

    /// Get all owned databases
    pub fn owned_databases(&self) -> Result<Vec<Database>, StoreError>;

    /// Get the "default" owned database (first created, or create one)
    pub fn default_database(&self) -> Result<Database, StoreError>;

    // === Recommendations (in your database) ===

    /// Create a recommendation in your database
    pub fn create_recommendation(
        &self,
        database_id: DatabaseId,
        subject: String,
        body: String,
    ) -> Result<SignedVouchEvent, StoreError>;

    /// Disavow a recommendation
    pub fn disavow(
        &self,
        database_id: DatabaseId,  // Your database
        target_database_id: DatabaseId,
        target_sequence: u64,
    ) -> Result<SignedVouchEvent, StoreError>;

    /// Vouch for someone else's recommendation
    pub fn vouch(
        &self,
        database_id: DatabaseId,  // Your database
        source_event: &SignedVouchEvent,
        source_name: Option<String>,
    ) -> Result<SignedVouchEvent, StoreError>;

    // === Queries ===

    /// List all recommendations (from all databases, not disavowed)
    pub fn list_recommendations(&self) -> Result<Vec<Recommendation>, StoreError>;

    /// Search recommendations by text/subject
    pub fn search(&self, query: &str) -> Result<Vec<Recommendation>, StoreError>;

    /// Get recommendations from a specific database
    pub fn recommendations_from(
        &self,
        database_id: DatabaseId,
    ) -> Result<Vec<Recommendation>, StoreError>;

    /// Get distinct subjects for autocomplete
    pub fn subjects(&self) -> Result<Vec<String>, StoreError>;

    // === Database Registry ===

    /// Get display name for a database (four-name resolution)
    pub fn display_name(&self, database_id: DatabaseId) -> Result<String, StoreError>;

    /// Set petname for a database
    pub fn set_petname(
        &self,
        database_id: DatabaseId,
        petname: Option<String>,
    ) -> Result<(), StoreError>;

    /// Get all known databases
    pub fn all_databases(&self) -> Result<Vec<Database>, StoreError>;
}
```

### EventStore (Low-Level)

```rust
impl EventStore {
    /// Append a signed event and project it
    pub fn append(&self, event: SignedVouchEvent) -> Result<(), StoreError>;

    /// Read all events for a database
    pub fn events_for(&self, database_id: DatabaseId) -> Result<Vec<SignedVouchEvent>, StoreError>;

    /// Read events since a sequence (for sync)
    pub fn events_since(
        &self,
        database_id: DatabaseId,
        since_sequence: u64,
    ) -> Result<Vec<SignedVouchEvent>, StoreError>;
}
```

### Crypto Module

```rust
/// Generate a new Ed25519 keypair
pub fn generate_keypair() -> (SigningKey, VerifyingKey);

/// Sign an event
pub fn sign_event(event: &VouchEvent, signing_key: &SigningKey) -> SignedVouchEvent;

/// Verify an event signature
pub fn verify_event(signed: &SignedVouchEvent) -> Result<(), CryptoError>;

/// Verify a vouch chain (original signature + vouch signature)
pub fn verify_vouch_chain(vouch: &SignedVouchEvent) -> Result<(), CryptoError>;

/// Generate a random symmetric key for E2EE
pub fn generate_event_key() -> [u8; 32];

/// Encrypt an event for transmission
pub fn encrypt_event(
    signed: &SignedVouchEvent,
    event_key: &[u8; 32],
) -> Result<Vec<u8>, CryptoError>;

/// Decrypt an event
pub fn decrypt_event(
    ciphertext: &[u8],
    event_key: &[u8; 32],
) -> Result<SignedVouchEvent, CryptoError>;
```

## GPUI Integration

### Store Entity

```rust
/// Wraps VouchDatabase and provides GPUI reactivity
pub struct Store {
    db: VouchDatabase,

    // Cached data for UI
    recommendations: Vec<Recommendation>,
    databases: Vec<Database>,

    // Current state
    active_database: Option<DatabaseId>,
}

impl Store {
    pub fn new(db: VouchDatabase, cx: &mut Context<Self>) -> Self {
        let mut store = Self {
            db,
            recommendations: vec![],
            databases: vec![],
            active_database: None,
        };
        store.refresh(cx);
        store
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        self.recommendations = self.db.list_recommendations().unwrap_or_default();
        self.databases = self.db.all_databases().unwrap_or_default();
        cx.notify();
    }

    /// Create a recommendation and refresh
    pub fn create_recommendation(
        &mut self,
        subject: String,
        body: String,
        cx: &mut Context<Self>,
    ) -> Result<(), StoreError> {
        let db_id = self.active_database
            .or_else(|| self.db.default_database().ok().map(|d| d.id))
            .ok_or(StoreError::NoDatabaseSelected)?;

        self.db.create_recommendation(db_id, subject, body)?;
        self.refresh(cx);
        Ok(())
    }

    /// Disavow and refresh
    pub fn disavow(
        &mut self,
        target_database_id: DatabaseId,
        target_sequence: u64,
        cx: &mut Context<Self>,
    ) -> Result<(), StoreError> {
        let db_id = self.active_database
            .or_else(|| self.db.default_database().ok().map(|d| d.id))
            .ok_or(StoreError::NoDatabaseSelected)?;

        self.db.disavow(db_id, target_database_id, target_sequence)?;
        self.refresh(cx);
        Ok(())
    }

    // Read access
    pub fn recommendations(&self) -> &[Recommendation] {
        &self.recommendations
    }

    pub fn databases(&self) -> &[Database] {
        &self.databases
    }

    pub fn search(&self, query: &str) -> Result<Vec<Recommendation>, StoreError> {
        self.db.search(query)
    }
}
```

### VouchApp Integration

```rust
struct VouchApp {
    store: Entity<Store>,
    // ... other fields
}

impl VouchApp {
    fn new(cx: &mut Context<Self>) -> Self {
        let db_path = app_data_dir().join("vouch.db");
        let db = init_database(&db_path).expect("Failed to init database");
        let store = cx.new(|cx| Store::new(db, cx));

        Self {
            store,
            // ...
        }
    }
}
```

## Implementation Steps

### Step 5.1: Dependencies & Crypto Foundation
- [ ] Add dependencies to Cargo.toml
- [ ] Create `src/crypto/mod.rs` with exports
- [ ] Create `src/crypto/keys.rs` - DatabaseId, keypair generation
- [ ] Create `src/crypto/signing.rs` - sign/verify events
- [ ] Create `src/crypto/encryption.rs` - ChaCha20-Poly1305
- [ ] Unit tests for crypto operations

### Step 5.2: Database Foundation
- [ ] Create `src/store/mod.rs` with exports
- [ ] Create `src/store/error.rs` - StoreError enum
- [ ] Create `src/store/database.rs` - connection, migrations
- [ ] Implement schema creation
- [ ] Test database initialization

### Step 5.3: Event Types & Store
- [ ] Create `src/store/event.rs` - event types, JSON serialization
- [ ] Create `src/store/event_store.rs` - append, read events
- [ ] Implement signature verification on read
- [ ] Test event round-trip (create → sign → store → read → verify)

### Step 5.4: Projector
- [ ] Create `src/store/projector.rs`
- [ ] Implement projection for Recommendation events
- [ ] Implement projection for Vouch events
- [ ] Implement projection for Disavow events
- [ ] Implement projection for UpdateDatabase events
- [ ] Handle tombstones (disavow before target arrives)
- [ ] Wire automatic projection after append
- [ ] Test idempotent replay

### Step 5.5: Query Layer
- [ ] Create `src/store/recommendation_store.rs`
- [ ] Implement `list()`, `get()`, `search()`
- [ ] Implement `by_database()`
- [ ] Implement `subjects()` for autocomplete
- [ ] Create `src/store/database_registry.rs`
- [ ] Implement four-name resolution
- [ ] Implement petname management

### Step 5.6: High-Level API
- [ ] Implement `VouchDatabase` with all CRUD methods
- [ ] Create default database on first run
- [ ] Generate UUIDs for sequence numbers? No - use auto-increment per database
- [ ] Return projected `Recommendation` after mutations

### Step 5.7: GPUI Integration
- [ ] Create `Store` entity wrapper
- [ ] Implement reactive refresh
- [ ] Update `VouchApp` to use `Store`
- [ ] Wire NewRecommendationModal save to store
- [ ] Wire disavow button to store

### Step 5.8: Migration from Mock Data
- [ ] Update `data.rs` to re-export store types
- [ ] Remove `MockData` usage from UI components
- [ ] Update UI to read from `Store` entity
- [ ] Add dev seed data option

### Step 5.9: Testing
- [ ] Unit tests for event serialization
- [ ] Unit tests for crypto operations
- [ ] Integration tests with in-memory database
- [ ] Test projection idempotency
- [ ] Test four-name resolution
- [ ] Test search functionality

## Dependencies to Add

```toml
[dependencies]
# Existing
gpui = "0.2"
gpui-component = "0.5"

# Data layer
rusqlite = { version = "0.32", features = ["bundled"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Crypto
ed25519-dalek = { version = "2.1", features = ["serde"] }
chacha20poly1305 = "0.10"
rand = "0.8"
hex = "0.4"

# Utilities
thiserror = "1.0"
```

## Key Design Decisions

### Sequence Numbers vs UUIDs
Events use per-database sequence numbers (not UUIDs):
- Enables efficient sync: "give me events since sequence 47"
- Total ordering within a database
- Simpler than UUIDs for the append-only model

### Tombstone Strategy
Disavowals are stored even if the target event hasn't arrived:
- Ensures convergence regardless of event arrival order
- When target arrives, check disavowal table
- Important for eventually-consistent sync

### Four-Name Resolution Order
1. Petname (your private name) - if set
2. Verified name (from their UpdateDatabase) - with ✓ indicator
3. Proposed name (from vouches) - with "unverified" indicator
4. Unknown (short hex of DatabaseId)

### Owned vs Subscribed Databases
- Owned: has `signing_key`, can create events
- Subscribed: no `signing_key`, read-only, has `last_synced_sequence`
- Both have `event_key` for decryption

## Testing Strategy

### Unit Tests

```rust
#[test]
fn test_create_and_sign_event() {
    let (signing_key, verifying_key) = generate_keypair();
    let db_id = DatabaseId(verifying_key);

    let event = VouchEvent {
        database_id: db_id,
        sequence: 1,
        timestamp: now_ms(),
        payload: DatabaseEvent::Recommendation(RecommendationContent::Simple {
            subject: "Thai Place".into(),
            body: "Great pad thai!".into(),
        }),
    };

    let signed = sign_event(&event, &signing_key);
    assert!(verify_event(&signed).is_ok());
}

#[test]
fn test_create_recommendation() {
    let db = init_memory_database().unwrap();
    let my_db = db.create_database("My Recs".into()).unwrap();

    db.create_recommendation(my_db.id, "Joe's Pizza".into(), "Best in town!".into()).unwrap();

    let recs = db.list_recommendations().unwrap();
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].subject, "Joe's Pizza");
}

#[test]
fn test_disavow_hides_recommendation() {
    let db = init_memory_database().unwrap();
    let my_db = db.create_database("My Recs".into()).unwrap();

    let event = db.create_recommendation(my_db.id, "Bad Place".into(), "Avoid!".into()).unwrap();
    assert_eq!(db.list_recommendations().unwrap().len(), 1);

    db.disavow(my_db.id, my_db.id, event.event.sequence).unwrap();
    assert_eq!(db.list_recommendations().unwrap().len(), 0);
}

#[test]
fn test_four_name_resolution() {
    let db = init_memory_database().unwrap();
    // ... setup database with various name sources

    // Petname takes precedence
    db.set_petname(other_db_id, Some("Mom".into())).unwrap();
    assert_eq!(db.display_name(other_db_id).unwrap(), "Mom");
}
```

### Integration Tests

```rust
#[test]
fn test_search_finds_by_subject() {
    let db = init_memory_database().unwrap();
    let my_db = db.create_database("Recs".into()).unwrap();

    db.create_recommendation(my_db.id, "Amazing Sushi".into(), "Fresh fish".into()).unwrap();
    db.create_recommendation(my_db.id, "Great Pizza".into(), "Wood fired".into()).unwrap();

    let results = db.search("sushi").unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].subject.contains("Sushi"));
}

#[test]
fn test_vouch_preserves_signature() {
    let db = init_memory_database().unwrap();
    let alice_db = db.create_database("Alice".into()).unwrap();
    let bob_db = db.create_database("Bob".into()).unwrap();

    // Alice creates a rec
    let alice_rec = db.create_recommendation(
        alice_db.id, "Thai Place".into(), "Great!".into()
    ).unwrap();

    // Bob vouches for it
    let vouch = db.vouch(bob_db.id, &alice_rec, Some("Alice's Recs".into())).unwrap();

    // Verify the vouch chain
    assert!(verify_vouch_chain(&vouch).is_ok());
}
```

## Success Criteria

Phase 5 is complete when:

1. Events are persisted to SQLite and survive app restart
2. Recommendations can be created and disavowed
3. UI displays recommendations from the database (not mock data)
4. Events are cryptographically signed and verified
5. Search works across subject and body
6. Subject autocomplete works
7. Four-name resolution displays correct names
8. All existing UI functionality still works

## Future Phases (Not in Scope)

These build on Phase 5:
- **Phase 6**: Database management UI
- **Phase 7**: Invitation system (generate/accept invites)
- **Phase 8**: Vouch flow (vouch for others' recs)
- **Phase 9**: Sync via WebSocket relay
- **Phase 10**: Key backup with BIP39 mnemonics
