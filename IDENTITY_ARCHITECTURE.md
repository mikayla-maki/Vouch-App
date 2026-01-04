# Vouch Identity & Contact Architecture

This document describes how Vouch handles identity, key management, contact discovery, and naming. Read [ARCHITECTURE.md](./ARCHITECTURE.md) for the core data model and [SIGNAL_ARCHITECTURE.md](./SIGNAL_ARCHITECTURE.md) for privacy and network architecture.

## Core Principle: Database = Identity

**There is no separate "user" concept at the network level.**

Each database IS an identity:
- One database = one keypair = one public identity
- Alice's "Food Recs" and Alice's "Sports Takes" are completely separate identities on the network
- They're only connected in Alice's local app because she manages both keypairs
- Others cannot tell these databases belong to the same person (unless explicitly linked out-of-band)

This is simpler than having a user/database hierarchy and provides natural pseudonymity.

```rust
/// A database is an identity
struct Database {
    id: DatabaseId,              // Hash of public key
    public_key: PublicKey,       // The one key for this identity
    private_key: Option<SigningKey>,  // Present if we own this database

    // Metadata from UpdateDatabase events
    name: String,
    description: Option<String>,
    picture: Option<ContentHash>,

    is_local: bool,              // Do we own this database?
}

/// Database ID is the hash of the public key
struct DatabaseId(Hash);

impl DatabaseId {
    fn from_public_key(key: &PublicKey) -> Self {
        DatabaseId(blake3::hash(key.as_bytes()))
    }
}
```

**Benefits:**
- Simpler cryptography (one keypair per database)
- Natural pseudonymity (databases are unlinkable by default)
- Clean separation (database metadata = identity metadata)
- Follows SSB model (proven design)
- Privacy by default (can't build social graph of "users")

## The Four-Name Model

Each database identity has four types of names:

### 1. Identifier (Globally Unique, Cryptographic)

```rust
/// Example: did:vouch:4f2a8b3c9e...
struct DatabaseId(Hash);  // Hash of public key (32 bytes)
```

**Properties:**
- Globally unique
- Cryptographically verifiable
- Permanent (never changes)
- Not human-readable
- Hash format enables future key rotation (v2)

**Display:** Only shown in technical contexts, usually truncated: `@4f2a8b...`

### 2. Self-Proposed Name (Authoritative, Verified)

The name the database announces for itself via UpdateDatabase events.

```rust
enum VouchEventPayload {
    UpdateDatabase {
        name: String,              // "Alice's Food Recommendations"
        description: Option<String>,
        picture: Option<ContentHash>,
    },
    // ...
}
```

**Properties:**
- Set by database owner
- Cryptographically signed (part of event log)
- Can be updated (latest UpdateDatabase wins)
- Verifiable by anyone who fetches the events
- This is THE authoritative name

**Caching:**
```sql
CREATE TABLE contacts (
    database_id BLOB PRIMARY KEY,

    -- Verified: fetched directly from source
    verified_name TEXT,
    verified_at INTEGER,

    -- ...
);
```

### 3. Proposed Names (Unverified, From Others)

Names claimed by others in revouches, before you've verified by fetching from the source.

```rust
Revouch {
    source_database: DatabaseId,
    original_signature: Signature,
    content: RecommendationContent,

    // What Bob claims the source is called
    source_name: Option<String>,

    // Bootstrap key for Carol to verify
    source_initial_key: Option<PublicKey>,
}
```

**Properties:**
- Unverified (trust the revoucher)
- Could be stale (database renamed since revouch)
- Could be malicious (revoucher lying)
- Useful for immediate display before verification

**Caching:**
```sql
CREATE TABLE proposed_names (
    database_id BLOB NOT NULL,
    name TEXT NOT NULL,
    proposed_by BLOB NOT NULL,    -- Who told us this
    seen_in_event INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,

    PRIMARY KEY (database_id, proposed_by)
);
```

**Multiple proposed names analysis:**
```rust
fn analyze_proposed_names(db: DatabaseId) -> NameConsensus {
    let names = get_proposed_names(db);
    let unique_names: HashSet<String> = names.iter().map(|n| n.name).collect();

    match unique_names.len() {
        0 => NameConsensus::Unknown,
        1 => NameConsensus::Consensus,      // All sources agree
        2..=3 => NameConsensus::Disputed,   // Could be rename or deception
        _ => NameConsensus::Suspicious,     // Definitely weird
    }
}
```

**UI treatment:**
- Consensus: `"Alice's Food Recs (3 sources agree)"`
- Disputed: `"Alice's Food Recs or PDX Food (sources disagree)"` ⚠️
- Suspicious: `"Multiple conflicting names"` 🚩 `[Show details]`

### 4. Pet Name (Local Only, Never Transmitted)

Your personal, private name for a database.

```sql
CREATE TABLE contacts (
    database_id BLOB PRIMARY KEY,
    petname TEXT NOT NULL,        -- "Mom's Recs", "Thai Food Guy"

    -- Other fields...
);
```

**Properties:**
- Completely private (never leaves your device)
- Can be anything you want
- Takes precedence in your personal UI
- Following the [petname system](https://files.spritely.institute/papers/petnames.html)

**This is your namespace.** You control what you call things.

## Name Resolution & Display

```rust
fn display_name(db: DatabaseId, context: DisplayContext) -> String {
    let contact = get_contact(db);

    match context {
        // Your personal view (feed, subscriptions list)
        DisplayContext::PersonalView => {
            contact.petname
        },

        // Showing attribution (who made this rec)
        DisplayContext::Attribution => {
            if let Some(verified) = contact.verified_name {
                format!("{} ✓", verified)  // Verified checkmark
            } else if let Some(proposed) = contact.most_common_proposed_name() {
                let proposer = get_contact(proposed.proposed_by);
                format!("{} (via {})", proposed.name, proposer.petname)
            } else {
                format!("Unknown database ({}...)", db.short_form())
            }
        },

        // Technical/debugging view
        DisplayContext::Technical => {
            format!("{} ({})", contact.petname, db.to_string())
        },
    }
}
```

**Example UI:**

Your feed view (PersonalView):
```
🍜 Thai Place Downtown
"Best pad thai I've ever had..."
from: Mom's Recs • 2 hours ago
```

Attribution view when not your petname:
```
🍜 Thai Place Downtown
"Best pad thai I've ever had..."
from: Alice's Food Recommendations ✓ • 2 hours ago
         ↑ verified self-proposed name
```

Unverified attribution:
```
🔧 John's Auto
"Honest mechanic, fair prices"
from: Bob's Garage Picks (via Thai Food Guy) • 1 day ago
                            ↑ unverified, told to us by Thai Food Guy
```

## Contact Discovery

Contacts are discovered through three mechanisms:

### 1. Direct Invitation (Out-of-Band)

```
vouch://invite?db=did:vouch:abc123&relay=https://relay.vouch.chat&key=<base64_pubkey>

Parameters:
- db: DatabaseId (permanent identifier)
- relay: Where to fetch events
- key: Initial public key (bootstrap trust anchor)
```

**Flow:**
1. Alice generates invite link
2. Bob receives it via Signal/email/QR code
3. Bob's app:
   - Creates database subscription
   - Creates contact entry with placeholder petname: `"Unnamed (abc12...)"`
   - Stores initial public key as bootstrap trust anchor
   - Fetches events from relay
   - Processes UpdateDatabase events, caches verified name
4. Bob sets meaningful petname: `"Alice's Food Recs"`

### 2. Transitive Discovery (Via Revouches)

```rust
Revouch {
    source_database: DatabaseId,      // Carol learns about this
    original_signature: Signature,
    content: RecommendationContent,

    // Optional: convenience for Carol
    source_name: Option<String>,      // What Alice calls herself
    source_initial_key: Option<PublicKey>,  // For verification
}
```

**Flow:**
1. Carol subscribes to Bob's database
2. Carol sees Bob revouch Alice's rec
3. Carol's app:
   - Extracts `source_database` (Alice's ID)
   - Creates contact entry if not present
   - Stores proposed name from `source_name`
   - Marks `discovered_via: "revouch_from:Bob"`
   - Shows UI: `"Bob revouched from Alice's Food Recs (not subscribed)"`
4. Carol CANNOT automatically subscribe (no in-app follow button)
5. If Carol wants to subscribe, she must get invite link from Bob or Alice (out-of-band)

**Background verification (optional):**
```rust
async fn refresh_unverified_contacts() {
    for contact in contacts.where(verified_name.is_none()) {
        // Try to fetch and verify from source
        if let Ok(events) = fetch_events(contact.database_id, 0, 10).await {
            if let Some(update) = events.find_latest_update_database() {
                contact.verified_name = Some(update.name);
                contact.verified_at = Some(now());
            }
        }
    }
}
```

### 3. Manual Entry (Edge Case)

User can manually add a database if they know the ID and relay:
- Paste `did:vouch:abc123` and relay URL
- App fetches events
- User sets petname

This is rare but supports power users.

## Contact Book Schema

```sql
-- Local-only, never synced
CREATE TABLE contacts (
    database_id BLOB PRIMARY KEY,

    -- Your personal name (always present)
    petname TEXT NOT NULL,

    -- Verified name (fetched from source)
    verified_name TEXT,
    verified_at INTEGER,

    -- Discovery metadata
    discovered_via TEXT,          -- "invitation", "revouch_from:<database_id>", "manual"
    created_at INTEGER NOT NULL,

    -- TOFU (Trust On First Use) state
    initial_public_key BLOB NOT NULL,  -- Bootstrap trust anchor
    key_verified BOOLEAN DEFAULT FALSE,  -- Did user manually verify?
    last_key_check_at INTEGER,

    -- Subscription state
    is_subscribed BOOLEAN DEFAULT FALSE,
    subscription_status TEXT,     -- "active", "paused", "blocked"
);

-- Proposed names from revouches
CREATE TABLE proposed_names (
    database_id BLOB NOT NULL,
    name TEXT NOT NULL,
    proposed_by BLOB NOT NULL,    -- DatabaseId of revoucher
    seen_in_event INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,

    PRIMARY KEY (database_id, proposed_by),
    FOREIGN KEY (database_id) REFERENCES contacts(database_id),
    FOREIGN KEY (proposed_by) REFERENCES contacts(database_id)
);

-- Key history (v2: for rotation tracking)
CREATE TABLE key_history (
    database_id BLOB NOT NULL,
    public_key BLOB NOT NULL,
    valid_from_sequence INTEGER NOT NULL,
    valid_until_sequence INTEGER,  -- NULL = current
    added_at INTEGER NOT NULL,

    PRIMARY KEY (database_id, public_key),
    FOREIGN KEY (database_id) REFERENCES contacts(database_id)
);
```

## Key Management

### V1: Single Key, No Rotation

Each database has exactly one keypair. The private key signs all events.

```rust
struct Database {
    id: DatabaseId,                   // Hash of public key
    public_key: PublicKey,            // The one and only key
    private_key: Option<SigningKey>,  // Only present for owned databases
}

struct VouchEvent {
    database_id: DatabaseId,
    sequence: u64,
    timestamp: Timestamp,
    payload: VouchEventPayload,
    signature: Signature,  // Signed by THE key
}
```

**Why single key?**
- Simplest crypto model: one signature = one key
- Cannot have "any of these keys can verify" (cryptographically impossible)
- Proven model (SSB and Signal both use permanent identity keys)

**Verification is straightforward:**
```rust
fn verify_event(event: VouchEvent, public_key: PublicKey) -> Result<()> {
    // Extract public key from signature
    let signing_key = extract_public_key(&event.signature)?;

    // Must match the known key
    if signing_key != public_key {
        return Err("Event signed by wrong key");
    }

    // Verify signature
    verify_signature(event.signature, event.payload, public_key)
}
```

### Key Backup (Critical for V1)

Since keys cannot be rotated in v1, backup is essential.

**Backup format: BIP39 Mnemonic**
```rust
impl SigningKey {
    pub fn to_mnemonic(&self) -> String {
        // Convert 32-byte key to 24-word mnemonic
        bip39::encode(self.secret_bytes())
    }

    pub fn from_mnemonic(words: &str) -> Result<Self> {
        let bytes = bip39::decode(words)?;
        SigningKey::from_bytes(bytes)
    }
}
```

**UX flow:**

Database creation:
```
[Create Database]
→ Generate keypair
→ Show backup screen:
  "Your database identity is tied to this key. Back it up!"

  [Show Recovery Phrase] button
  → Display 24 words
  → "Write these down and store safely"
  → [ ] I've written down my recovery phrase
  → [Continue]
```

Key restoration:
```
[Restore Database]
→ "Enter your 24-word recovery phrase"
→ Text input (auto-complete word suggestions)
→ Derive keypair from mnemonic
→ Fetch events from relay
→ Continue using database
```

**Storage:**
- iOS: Store in Keychain with `kSecAttrAccessibleWhenUnlocked`
- Android: Store in Android Keystore with `ENCRYPT` purpose
- Desktop: OS-provided secure storage (Windows Credential Manager, macOS Keychain, Linux Secret Service)

### Key Compromise (V1 Mitigation)

**If private key is compromised:**
1. User has lost control of that database identity
2. Must create new database with new key
3. Publish farewell message in compromised database: `"This database has been compromised. Follow me at did:vouch:new_id"`
4. Share new invite links via out-of-band channels
5. Subscribers manually migrate

**If private key is lost (but backed up):**
1. Install app on new device
2. Import mnemonic phrase
3. Continue signing events
4. No protocol changes needed

This is acceptable for v1 given:
- Key compromise is rare with proper backup practices
- Similar to losing PGP key or Bitcoin wallet (established UX)
- Can add rotation in v2

### Trust On First Use (TOFU)

When you first encounter a database, you learn its public key and trust it.

```rust
struct ContactTOFU {
    database_id: DatabaseId,
    initial_public_key: PublicKey,  // First key seen, never changes
    key_verified: bool,              // Did user manually verify?
}

fn process_first_event(db_id: DatabaseId, event: VouchEvent) -> Result<()> {
    // Extract public key from signature
    let public_key = extract_public_key(&event.signature)?;

    // Verify ID matches key
    if DatabaseId::from_public_key(&public_key) != db_id {
        return Err("Database ID doesn't match public key");
    }

    // Store as trusted key
    contacts.insert(Contact {
        database_id: db_id,
        initial_public_key: public_key,
        key_verified: false,  // Warn user to verify
        // ...
    });

    Ok(())
}
```

**Key verification (optional but recommended):**

Following Signal's "safety numbers" model:
```rust
fn safety_number(db_a: DatabaseId, key_a: PublicKey,
                 db_b: DatabaseId, key_b: PublicKey) -> String {
    // Deterministic, verifiable out-of-band
    let hash = blake3::hash(&[db_a.bytes(), key_a.bytes(),
                              db_b.bytes(), key_b.bytes()]);
    format_as_groups(hash)  // "12345 67890 24680 ..."
}
```

Users can compare safety numbers via other channels (phone call, in-person) to verify no MITM.

**In practice:** Very few users verify (Signal learned this). We'll provide the feature but not require it.

### V2: Key Rotation (Deferred)

```rust
enum VouchEventPayload {
    // V2 addition
    RotateKey {
        new_public_key: PublicKey,
        reason: Option<String>,  // "Compromised", "Device lost", "Routine"
    },
    // ...
}

struct KeyTimeline {
    database_id: DatabaseId,
    rotations: Vec<KeyRotation>,
}

struct KeyRotation {
    valid_from_sequence: u64,
    public_key: PublicKey,
}
```

**Rotation flow:**
```
Sequence 0-99:  Signed by old_key
Sequence 100:   RotateKey event (signed by old_key, announces new_key)
Sequence 101+:  Signed by new_key
```

**Verification with timeline:**
```rust
fn verify_event_v2(event: VouchEvent, timeline: &KeyTimeline) -> Result<()> {
    // Which key was valid at this sequence?
    let valid_key = timeline.key_at_sequence(event.sequence)?;

    // Verify with that specific key
    verify_signature(event.signature, event.payload, valid_key)?;

    // Update timeline if this is a rotation
    if let RotateKey { new_public_key, .. } = event.payload {
        timeline.add_rotation(event.sequence + 1, new_public_key);
    }

    Ok(())
}
```

**Not included in v1** - adds significant complexity, rarely needed.

## Bootstrap & Verification Chain

### The Bootstrap Problem

When Carol sees Bob revouch from Alice, how does Carol learn Alice's public key?

**Solution: Bootstrap key in invitation and revouch**

```rust
// Invitation includes initial key
struct DatabaseInvite {
    database_id: DatabaseId,
    relay_url: String,
    event_key: SymmetricKey,      // For E2EE (see SIGNAL_ARCHITECTURE.md)
    initial_public_key: PublicKey, // Bootstrap trust anchor
}

// Revouch optionally includes initial key
struct Revouch {
    source_database: DatabaseId,
    original_signature: Signature,
    content: RecommendationContent,

    // Optional: for transitive discovery
    source_name: Option<String>,
    source_initial_key: Option<PublicKey>,
}
```

### Verification Flow

**Bob subscribes to Alice (direct invitation):**
1. Bob receives `DatabaseInvite` with `initial_public_key`
2. Bob fetches events from relay
3. Bob verifies event 0 signature matches `initial_public_key`
4. Bob processes UpdateDatabase, caches Alice's self-proposed name
5. Bob verifies all subsequent events with `initial_public_key`

**Carol sees Bob's revouch (transitive discovery):**
1. Carol verifies Bob's signature on revouch event ✓
2. Carol extracts `source_initial_key` from revouch
3. Carol verifies `original_signature` matches `source_initial_key` ✓
4. Cryptographic chain complete: Bob vouches for Alice, Alice signed the content
5. Carol can optionally fetch Alice's events to verify name and get more context

**Without source_initial_key:**
1. Carol sees revouch with only `source_database` (DatabaseId)
2. Carol cannot verify `original_signature` (doesn't have Alice's key)
3. Carol trusts Bob's revouch based on trust in Bob
4. Carol's UI shows: `"Bob revouched from Unknown Database (did:vouch:abc...)"`
5. Background task can fetch Alice's events to verify

### Name Verification Priority

```rust
enum NameVerificationStatus {
    Verified {
        name: String,
        verified_at: Timestamp,
    },
    ProposedConsensus {
        name: String,
        proposers: Vec<DatabaseId>,  // Multiple sources agree
    },
    ProposedDisputed {
        names: Vec<(String, DatabaseId)>,  // Sources disagree
    },
    Unverified,
}

fn get_display_name(db: DatabaseId) -> (String, NameVerificationStatus) {
    let contact = get_contact(db);

    // 1. Always prefer petname for personal view
    let display = contact.petname.clone();

    // 2. Determine verification status for attribution
    let status = if let Some(verified) = contact.verified_name {
        NameVerificationStatus::Verified {
            name: verified,
            verified_at: contact.verified_at.unwrap(),
        }
    } else {
        let proposed = get_proposed_names(db);
        let unique_names: HashMap<String, Vec<DatabaseId>> =
            proposed.into_iter()
                .map(|p| (p.name, p.proposed_by))
                .fold(HashMap::new(), |mut acc, (name, by)| {
                    acc.entry(name).or_insert(vec![]).push(by);
                    acc
                });

        match unique_names.len() {
            0 => NameVerificationStatus::Unverified,
            1 => {
                let (name, proposers) = unique_names.into_iter().next().unwrap();
                NameVerificationStatus::ProposedConsensus { name, proposers }
            },
            _ => {
                let names = unique_names.into_iter()
                    .flat_map(|(name, proposers)| {
                        proposers.into_iter().map(move |p| (name.clone(), p))
                    })
                    .collect();
                NameVerificationStatus::ProposedDisputed { names }
            }
        }
    };

    (display, status)
}
```

## No In-App Social Features

**Vouch is NOT a social network.** All social coordination happens out-of-band.

**Things that should NOT be events:**
- Friend requests
- Direct messages
- Follows/unfollows (subscribe is local-only state)
- Profile likes/comments
- Status updates

**Your database publishes claims (recs, revouches, disavowals). That's it.**

**For coordination, use:**
- Signal/WhatsApp for messaging
- Email for sharing invite links
- QR codes for in-person sharing
- Existing social platforms for discovery

This keeps Vouch focused and avoids becoming yet another messaging app.

## Implementation Priorities

### V1 Must-Have

- ✅ DatabaseId = hash(public_key)
- ✅ Single keypair per database
- ✅ UpdateDatabase events for self-proposed metadata
- ✅ Local-only contact book (petnames + cached names)
- ✅ Proposed names from revouches
- ✅ TOFU key management
- ✅ Mnemonic phrase backup
- ✅ Bootstrap key in invitations and revouches
- ✅ Name verification status (verified/proposed/unverified)
- ✅ Out-of-band invitation only

### V2 Enhancements

- Key rotation events (RotateKey)
- Safety number verification UI
- Multi-device support (share key or multiple keys via rotation)
- Social recovery (Shamir's Secret Sharing like Dark Crystal)
- Key expiration
- Contact export/import

### V3+ Future

- Identity linking (prove multiple databases belong to same person)
- Post-quantum cryptography
- Hardware security module integration
- Advanced TOFU improvements (transparency logs)

## Comparison with Other Systems

| Feature | Vouch (v1) | SSB | Signal |
|---------|-----------|-----|--------|
| **Identity = Key?** | ID = hash(key) | ID = key | ID = key |
| **Key rotation** | No (v2) | No | No (identity key) |
| **Backup keys** | Mnemonic phrase | Manual backup | Per-device |
| **Key compromise** | Create new DB | Create new identity | Remove device |
| **Identity discovery** | Out-of-band + revouches | Out-of-band + follows | Phone number |
| **Petnames** | Built-in | User-managed | Built-in |
| **Multi-database** | Yes (separate identities) | No (one feed/user) | N/A |
| **TOFU verification** | Optional safety numbers | Optional | Optional safety numbers |

## Example Flows

### Creating a Database

```rust
// User: "Create new database"
let primary_key = SigningKey::generate();
let db_id = DatabaseId::from_public_key(&primary_key.public_key());

// Show backup
let mnemonic = primary_key.to_mnemonic();
show_backup_screen(mnemonic);  // "Write these 24 words down!"

// Create database
let db = Database {
    id: db_id.clone(),
    public_key: primary_key.public_key(),
    private_key: Some(primary_key),
    name: "My Food Recommendations".to_string(),
    description: Some("Portland restaurants I love".to_string()),
    is_local: true,
};

// Publish first event
let event_0 = VouchEvent {
    database_id: db_id,
    sequence: 0,
    timestamp: now(),
    payload: UpdateDatabase {
        name: db.name.clone(),
        description: db.description.clone(),
        picture: None,
    },
    signature: primary_key.sign(payload),
};

publish_to_relay(event_0);
```

### Subscribing via Invitation

```rust
// User scans QR code: vouch://invite?db=...&relay=...&key=...
let invite = parse_invite(qr_code_data)?;

// Create contact
let contact = Contact {
    database_id: invite.database_id,
    petname: format!("Unnamed ({}...)", invite.database_id.short()),
    verified_name: None,
    initial_public_key: invite.initial_public_key,
    discovered_via: "invitation".to_string(),
    is_subscribed: true,
};
contacts.insert(contact);

// Fetch events
let events = relay.fetch_events(invite.database_id, since: 0).await?;

// Verify and process
for event in events {
    verify_event(event, invite.initial_public_key)?;
    process_event(event)?;
}

// Update cached name from latest UpdateDatabase
if let Some(name) = find_latest_database_name(&events) {
    contact.verified_name = Some(name);
    contact.verified_at = Some(now());
}

// Prompt user to set petname
show_set_petname_dialog(contact);
```

### Seeing a Revouch (Transitive Discovery)

```rust
// Carol sees Bob's revouch
let revouch_event = fetch_from_bob_database(sequence);

match revouch_event.payload {
    Revouch {
        source_database,
        source_name,
        source_initial_key,
        content,
        original_signature,
    } => {
        // Do we know this database?
        if let Some(contact) = contacts.get(source_database) {
            // Yes, verify with known key
            verify_signature(original_signature, content, contact.initial_public_key)?;
        } else if let Some(key) = source_initial_key {
            // No, but revouch provided bootstrap key
            verify_signature(original_signature, content, key)?;

            // Create contact entry
            contacts.insert(Contact {
                database_id: source_database,
                petname: source_name.unwrap_or_else(|| format!("Unnamed ({}...)", source_database.short())),
                verified_name: None,
                initial_public_key: key,
                discovered_via: format!("revouch_from:{}", revouch_event.database_id),
                is_subscribed: false,
            });

            // Store proposed name
            if let Some(name) = source_name {
                proposed_names.insert(ProposedName {
                    database_id: source_database,
                    name,
                    proposed_by: revouch_event.database_id,
                    seen_in_event: revouch_event.sequence,
                    first_seen_at: now(),
                });
            }

            // Background: try to verify name
            spawn_verification_task(source_database);
        } else {
            // No bootstrap key provided, trust Bob
            show_warning("Cannot verify revouch signature - trusting revoucher");
        }

        // Display in UI
        let display = format!("{} revouched from {}",
            get_contact(bob_database).petname,
            get_display_name_with_status(source_database)
        );
    }
}
```

### Background Name Verification

```rust
async fn verify_database_name(db_id: DatabaseId) {
    // Try to fetch events from source
    match fetch_events_from_any_relay(db_id, 0, 10).await {
        Ok(events) => {
            let contact = get_contact(db_id);

            // Verify all events with initial key
            for event in &events {
                if let Err(e) = verify_event(event, contact.initial_public_key) {
                    log::warn!("Event verification failed: {}", e);
                    return;
                }
            }

            // Find latest UpdateDatabase
            if let Some(update) = events.iter().rev().find_map(|e| {
                match &e.payload {
                    UpdateDatabase { name, .. } => Some(name.clone()),
                    _ => None,
                }
            }) {
                // Update verified name
                contact.verified_name = Some(update);
                contact.verified_at = Some(now());

                // Compare with proposed names
                let proposed = get_proposed_names(db_id);
                for p in proposed {
                    if p.name != update {
                        log::warn!("Proposed name '{}' doesn't match verified name '{}'",
                            p.name, update);
                    }
                }
            }
        },
        Err(e) => {
            log::debug!("Could not verify name for {}: {}", db_id.short(), e);
        }
    }
}
```

## Security Considerations

### Identity Verification

**TOFU (Trust On First Use):**
- First key seen is trusted
- Like SSH host keys or Signal contacts
- Most users won't manually verify (that's OK)

**Optional verification:**
- Safety numbers (compare out-of-band)
- QR code scanning (in-person)
- Voice verification (read numbers over phone)

**Key change detection:**
- Not possible in v1 (single key, no rotation)
- V2 will add: "This database rotated its key" warnings

### Threat Model

**Protected against:**
- ✅ Relay cannot impersonate (signatures)
- ✅ MITM cannot inject events (signatures)
- ✅ Relay cannot link databases to "users" (databases ARE identities)
- ✅ Subscribers cannot forge recs (signatures)

**Not protected against:**
- ❌ Compromised key (no rotation in v1)
- ❌ Device compromise (malware can steal key from keychain)
- ❌ Social engineering (user shares key)
- ❌ MITM at first contact (TOFU weakness - mitigated with safety numbers)

### Privacy Properties

**What others can learn:**
- DatabaseId (permanent identifier)
- Self-proposed name (from UpdateDatabase events)
- What you publish (recs, revouches, disavowals)
- Roughly when you're active (event timestamps)

**What others CANNOT learn:**
- Your real name (unless you put it in self-proposed name)
- Your other databases (unless you link them)
- Who you're subscribed to (subscriptions are local-only)
- Your petnames for others (never transmitted)

**Pseudonymity:**
- Each database is a separate pseudonym
- Unlinkable by default (different keypairs)
- Can voluntarily link via out-of-band channels or special events (v2)

## Open Questions for Future Sessions

1. **Multi-device sync:** Share key across devices or use key rotation?
2. **Identity linking:** Should there be a LinkIdentity event type? Privacy implications?
3. **Key rotation triggers:** When should users be prompted to rotate?
4. **Social recovery:** Implement Shamir's Secret Sharing like Dark Crystal?
5. **Name squatting:** Can high-profile names be protected? (Probably not - that's OK)
6. **Contact export:** How to share your contact book with new device?

## References

- [Petnames Paper](https://files.spritely.institute/papers/petnames.html) - The four-name model foundation
- [SSB Identity](https://handbook.scuttlebutt.nz/concepts/identity) - Single key, no rotation
- [Signal X3DH](https://signal.org/docs/specifications/x3dh/) - Identity key management
- [Dark Crystal](https://darkcrystal.pw/scuttlebutt-application/) - Social key recovery for SSB
- [DKMS](https://github.com/hyperledger/aries-rfcs/blob/master/concepts/0051-dkms/dkms-v4.md) - Decentralized key management patterns
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Core data model
- [SIGNAL_ARCHITECTURE.md](./SIGNAL_ARCHITECTURE.md) - Privacy and encryption
