# Vouch Signal-Inspired Architecture

This document describes Vouch's privacy and network architecture, inspired by Signal's threat model and design principles. Read [ARCHITECTURE.md](./ARCHITECTURE.md) first for the core data model and event sourcing design.

## Design Principles

1. **Privacy by default**: E2EE is not optional—it's the foundation
2. **Metadata protection**: Relay cannot build social graph or usage patterns
3. **Invite-only**: No public discovery, all access is explicitly granted
4. **Local-first**: Network is for sync only, your data lives on your device
5. **Simple relay**: Store-and-forward only, minimal trust required
6. **High-risk user protection**: Support reviewers who face retaliation risks

## Threat Model

### V1 Target: Curious Relay Operator + Passive Network Adversary

**Attackers we protect against:**
- Relay operator trying to read content
- Relay operator trying to map social graph
- Passive network observer (ISP, corporate firewall)
- Compromised relay attempting traffic analysis

**Attackers we don't protect against (yet):**
- Nation-state adversaries with timing correlation across global infrastructure
- Quantum computers (post-quantum crypto deferred)
- Device compromise (local data is readable)
- Social engineering / phishing

**Attack vectors prevented:**
- ✅ Content reading (E2EE)
- ✅ Content tampering (signatures)
- ✅ Social graph mapping (anonymous credentials)
- ✅ Publisher identification (sealed sender)
- ✅ Subscriber identification (anonymous subscriptions)
- ✅ Timing correlation (dummy traffic, batching)

**Attack vectors deferred:**
- Statistical attacks on dummy traffic patterns (v2+)
- Cross-relay correlation (no federation in v1)
- Device compromise recovery (v2+)

### Scale Assumptions

Following Signal's model:
- Users subscribe to 10-50 databases (not thousands)
- Databases have 10-100 subscribers (not millions)
- Events per database: 10-100/month (not thousands/day)
- High-value users (journalists, activists) may have higher risk profiles
- Most users have moderate privacy needs

These assumptions simplify:
- Signature verification (5K sigs/month is trivial)
- SQLite scalability (50K total events is small)
- Relay costs (low bandwidth)
- Spam/abuse (invite-only prevents unsolicited content)

## Discovery & Bootstrap

### Invite-Only Model

No global database directory. No in-app discovery. All access is explicitly granted via out-of-band invitation.

```rust
/// Database invitation (shared out-of-band)
struct DatabaseInvite {
    /// Which database this grants access to
    database_id: DatabaseId,

    /// Symmetric key for decrypting events
    event_key: SymmetricKey,

    /// Where to fetch events
    relay_url: String,

    /// Optional: Who's inviting you (for display)
    inviter_name: Option<String>,

    /// Optional: Single-use token for relay authentication
    access_token: Option<AccessToken>,

    /// Optional: Expiration
    expires_at: Option<Timestamp>,
}

/// Serialized as URL or QR code
/// vouch://invite?db=<base64>&key=<base64>&relay=<url>&...
```

### Invitation Flows

**In-person (QR code):**
1. Alice opens "Share Database" in app
2. App generates QR code containing `DatabaseInvite`
3. Bob scans with camera
4. Bob's app requests relay access
5. Relay notifies Alice of pending request
6. Alice approves → Bob receives events

**Out-of-band link (Signal, email, etc):**
1. Alice generates invite link: `vouch://invite?...`
2. Alice sends via Signal/WhatsApp/email
3. Bob clicks link → Opens Vouch app
4. Same flow as QR code

**Public databases (special case):**
- Organizations (Consumer Reports, journalism outlets) can publish invite links publicly
- Links include pre-authorized access token
- No approval needed (but still E2EE)
- "Public" means "anyone with link" not "globally discoverable"

### Cold Start Solution

Real-world social graph IS the bootstrap:
- Share your first database via Signal/WhatsApp with trusted friends
- They share theirs back
- Organic growth through existing trust relationships
- No artificial "suggested databases" or algorithmic discovery

## Encryption Architecture

### Per-Database Symmetric Keys

Each database has a single symmetric key shared among all authorized subscribers. This is simpler than Signal's double ratchet because:
- No 1:1 conversations (only broadcast to group)
- No forward secrecy requirement (events are immutable)
- Events never change once published

```rust
struct DatabaseKeys {
    database_id: DatabaseId,

    /// Symmetric key for encrypting all events in this database
    /// Shared via invitation, rotates only on compromise
    event_key: SymmetricKey,

    /// Database owner's signing keypair (never shared)
    signing_key: SigningKey,
}

/// Events are encrypted then signed
struct VouchEvent {
    database_id: DatabaseId,
    sequence: u64,

    /// Encrypted with database event_key
    /// Contains: author, timestamp, payload
    encrypted_payload: Vec<u8>,

    /// Anonymous credential proving authorization
    /// (not a signature that reveals identity)
    authorization_proof: AnonymousCredential,
}

/// After decryption (local only)
struct DecryptedEvent {
    author: UserIdentifier,
    timestamp: Timestamp,
    payload: VouchEventPayload,
    signature: Signature,  // Over plaintext payload
}
```

### Encryption Flow

**Publishing:**
```
Alice's plaintext event
  → Sign with Alice's private key
  → Serialize (author, timestamp, payload, signature)
  → Encrypt with database event_key
  → Add anonymous credential
  → Publish to relay
```

**Subscribing:**
```
Bob fetches encrypted event from relay
  → Decrypt with database event_key (from invite)
  → Deserialize (author, timestamp, payload, signature)
  → Verify signature with author's public key
  → Store in local event log
  → Update materialized view
```

### Key Compromise Handling

**If database event_key leaks:**
- All past events are readable (no forward secrecy in v1)
- Database owner publishes `KeyRotation` event
- New key distributed to authorized subscribers
- Old events remain encrypted with old key
- Future events use new key

**Key rotation deferred to v2** - v1 assumes key compromise is rare.

### Trust-On-First-Use (TOFU)

When Bob receives Alice's identifier via invite:
1. Bob's app stores `(Alice's identifier, Alice's public key)`
2. All future events from Alice must match this key
3. If key changes → App warns Bob (potential MITM or device rotation)
4. Bob must verify out-of-band (Signal safety numbers model)

## Sealed Sender Architecture

### Goal: Hide Who Published What

Without sealed sender, relay sees:
```
Alice (identifier: 0x123...) published to database_X at 2:30pm
Bob (identifier: 0x456...) subscribed to database_X at 2:31pm
```

Relay learns: Alice and Bob are connected.

With sealed sender, relay sees:
```
Anonymous event published to database_X at 2:30pm
Anonymous subscription request for database_X at 2:31pm
```

Relay learns: Database_X exists. Nothing else.

### Anonymous Credentials

Instead of signatures that reveal identity, use **anonymous credentials** that prove authorization without revealing who.

```rust
/// Proof that you're authorized to publish, without revealing identity
struct AnonymousCredential {
    /// Database this credential is for
    database_id: DatabaseId,

    /// Zero-knowledge proof: "I'm in the authorized set"
    /// (or blinded signature from database owner)
    proof: ZKProof,
}
```

**Setup (when database is created):**
1. Alice creates database, generates event_key and owner_key
2. Alice generates her own anonymous credential (self-signed)

**Granting access (when Bob subscribes):**
1. Alice issues anonymous credential for Bob
2. Credential proves "holder can publish to database_X"
3. Credential doesn't reveal which authorized user this is
4. Alice sends credential to Bob via encrypted channel

**Publishing:**
1. Bob creates event, encrypts with event_key
2. Bob generates proof using his credential
3. Relay verifies proof (valid credential for database_X) without learning Bob's identity
4. Relay stores event

### Implementation Approaches

**Option A: Blind Signatures (simpler, recommended for v1)**

Database owner signs credentials blindly:
```rust
// Alice creates blinded signature for Bob
let credential = database_owner_key.sign_blind(database_id);

// Bob unblinds it (Alice never saw Bob's identifier)
let anonymous_credential = credential.unblind(bob_blinding_factor);

// Bob proves authorization to relay
relay.verify_credential(anonymous_credential, database_id); // ✓
// Relay can't tell if this is Alice, Bob, or Carol
```

**Trade-off:** If credential leaks, anyone can publish. Mitigation: One-time credentials that relay tracks.

**Option B: Zero-Knowledge Proofs (more secure, deferred to v2+)**

Bob proves "I know a private key whose public key is in the authorized set":
```rust
// Alice publishes authorized set (hashed or merkle root)
let authorized_set_commitment = merkle_root([alice_pk, bob_pk, carol_pk]);

// Bob generates ZK proof
let proof = zkproof::prove(
    "I know private_key such that hash(public_key(private_key)) is in commitment",
    bob_private_key,
    authorized_set_commitment
);

// Relay verifies without learning which member Bob is
relay.verify_zk_proof(proof, authorized_set_commitment); // ✓
```

**Trade-off:** Much more complex, computationally expensive.

### Anonymous Subscriptions

Subscribing also reveals identity. Hide this with **Private Information Retrieval (PIR)**.

**Challenge:** Bob wants events from database_X without revealing he's interested in database_X.

**Solution options:**

**Option A: Multi-server PIR (deferred)**
- Multiple non-colluding relays
- Bob sends query fragments to each
- Reconstructs response without any single relay knowing query
- Requires federation (v2+)

**Option B: Dummy traffic (v1 approach)**
```rust
// Bob's client periodically fetches from random databases
// Real fetches are indistinguishable from dummy fetches
struct SubscriptionManager {
    // Databases Bob actually cares about
    real_subscriptions: Vec<DatabaseId>,

    // Dummy databases for traffic padding
    dummy_subscriptions: Vec<DatabaseId>,

    // Fetch both at random intervals
    async fn sync(&mut self) {
        let all_dbs = mix(real_subscriptions, dummy_subscriptions);
        for db in all_dbs.shuffle() {
            self.fetch_events(db).await;
            sleep(random_interval()).await;
        }
    }
}
```

**Trade-off:** Network overhead (fetching databases Bob doesn't care about). With small scale (50 subscriptions), this is acceptable.

**Option C: Event batching (v1 approach)**
```rust
// Relay batches events across all databases
// Bob downloads entire batch, filters locally
struct EventBatch {
    /// Events from ALL active databases
    events: Vec<VouchEvent>,

    /// Bob decrypts only those from his subscribed databases
    /// Relay doesn't know which ones Bob cares about
}
```

**Trade-off:** Bob downloads more data than needed. With small scale (100 events/day across all databases), this is acceptable.

**Recommendation for v1:** Combine dummy traffic + event batching for subscriber anonymity.

### Timing Attacks & Countermeasures

**Attack:** Alice publishes at 2:30pm, Bob fetches at 2:31pm → Likely connected.

**Defenses:**

1. **Event batching**: Relay accumulates events, releases in batches every N minutes
2. **Random delays**: Client adds random delay before fetching
3. **Dummy events**: Publishers occasionally send dummy events (indistinguishable from real)
4. **Persistent connections**: WebSocket stays open, events pushed asynchronously

```rust
struct RelayBatchingConfig {
    /// Relay releases event batches every 5 minutes
    batch_interval: Duration,

    /// Each batch contains events from last 5 minutes across all databases
    /// Client can't tell when event was actually published
    batch_window: Duration,
}
```

## Relay Architecture

### Relay Responsibilities (Minimal)

```rust
/// Relay is store-and-forward + access control only
/// Cannot read content, cannot identify users
struct VouchRelayServer {
    /// Storage: database_id -> Vec<EncryptedEvent>
    event_store: EventStore,

    /// Access control: database_id -> Vec<AnonymousCredential>
    /// (stored as commitments/merkle roots, not raw identifiers)
    access_control: ACLStore,

    /// Active WebSocket connections
    connections: WebSocketPool,

    /// Batching queue for timing attack mitigation
    batch_queue: BatchQueue,
}

#[async_trait]
trait VouchRelay {
    /// Publish encrypted events with anonymous credential
    async fn publish(
        &self,
        database_id: DatabaseId,
        events: Vec<VouchEvent>,
        credential: AnonymousCredential,
    ) -> Result<()>;

    /// Subscribe to event stream (anonymous)
    /// Returns batched events from multiple databases
    async fn subscribe(
        &self,
        subscription_set: BlindedSubscriptionSet,
    ) -> Result<EventStream<EventBatch>>;

    /// Update access control (database owner only)
    async fn update_acl(
        &self,
        database_id: DatabaseId,
        new_acl_commitment: MerkleRoot,
        proof: OwnershipProof,
    ) -> Result<()>;
}
```

### What Relay CAN'T Do

- ❌ Read event content (encrypted)
- ❌ Identify event authors (anonymous credentials)
- ❌ Identify subscribers (anonymous subscriptions)
- ❌ Correlate publishers and subscribers (batching + timing obfuscation)
- ❌ Tamper with events (signatures)
- ❌ Censor specific users (doesn't know who's who)

### What Relay CAN Do

- ✅ Know which databases exist
- ✅ Know rough activity levels per database
- ✅ Enforce rate limits per database (not per user)
- ✅ Delete old events (retention policy)
- ✅ Block entire databases (rare, for abuse)

### Relay Trust Model

**Honest-but-curious relay:** Relay follows protocol but tries to learn everything it can.

**Protections:**
- Content is encrypted (relay learns nothing about recs)
- Anonymous credentials (relay can't identify publishers)
- Anonymous subscriptions (relay can't identify subscribers)
- Batching + timing obfuscation (relay can't correlate)

**Remaining attack surface:**
- Relay can deny service (refuse to store/forward events)
- Relay can censor databases (block entire database_id)
- With enough data, statistical attacks might reveal patterns

**Mitigation:** Users can switch relays (invite link includes relay_url). If relay censors Alice's database, Alice publishes new invite links with different relay.

## Storage & Sync

### Local Storage (SQLite)

Same as ARCHITECTURE.md but events are stored encrypted:

```sql
-- All events stored encrypted as received from relay
CREATE TABLE events (
    id INTEGER PRIMARY KEY,
    database_id BLOB NOT NULL,
    sequence INTEGER NOT NULL,
    encrypted_payload BLOB NOT NULL,  -- Encrypted
    authorization_proof BLOB NOT NULL, -- Anonymous credential
    received_at INTEGER NOT NULL,
    UNIQUE(database_id, sequence)
);

-- Decrypted events projected into materialized view
-- (same tables as ARCHITECTURE.md)
CREATE TABLE recs (...);
CREATE TABLE rec_sources (...);
CREATE TABLE reactions (...);

-- Store decryption keys for subscribed databases
CREATE TABLE database_keys (
    database_id BLOB PRIMARY KEY,
    event_key BLOB NOT NULL,  -- Symmetric key from invitation
    received_at INTEGER NOT NULL
);
```

### Sync Protocol

**Establishing subscription:**
```
1. Bob receives DatabaseInvite (out-of-band)
   - Contains: database_id, event_key, relay_url

2. Bob connects to relay (anonymous WebSocket)
   - No authentication at connection time

3. Bob subscribes to event stream
   - Includes multiple databases (real + dummy)
   - Relay can't tell which Bob cares about

4. Relay streams event batches
   - Bob decrypts events from subscribed databases
   - Ignores events from dummy databases
```

**Publishing events:**
```
1. Alice creates event (rec, revouch, etc)

2. Alice signs event with her private key

3. Alice encrypts (author, timestamp, payload, signature) with database event_key

4. Alice generates anonymous credential

5. Alice publishes to relay
   - Relay verifies credential (valid for database_id)
   - Relay stores encrypted event
   - Relay queues for next batch

6. Relay broadcasts batch to active subscribers
```

**Handling offline:**
```
1. Bob goes offline for 1 week

2. Bob reconnects

3. Bob requests events since last_sequence
   - For each subscribed database
   - Mixed with dummy requests

4. Relay sends batches containing missed events

5. Bob decrypts and processes
```

## Access Control

### ACL Management

Database owners control who can subscribe:

```rust
/// Access control list for a database
struct DatabaseACL {
    database_id: DatabaseId,

    /// Commitment to authorized set (merkle root or hash)
    /// Relay stores this, not raw identifiers
    authorized_commitment: MerkleRoot,

    /// Local only: actual authorized users
    /// (never sent to relay)
    authorized_users: Vec<UserIdentifier>,
}
```

**Granting access:**
1. Alice approves Bob's subscription request
2. Alice adds Bob to local `authorized_users` list
3. Alice generates anonymous credential for Bob
4. Alice sends credential to Bob (encrypted, out-of-band or via relay)
5. Alice updates `authorized_commitment` on relay (Bob's identity not revealed)

**Revoking access:**
1. Alice removes Bob from local `authorized_users` list
2. Alice generates new credentials for remaining users
3. Alice updates `authorized_commitment` on relay
4. Alice rotates database event_key (optional, if full revocation needed)
5. Old credentials stop working (relay rejects Bob's publishes)

**Relay enforcement:**
```rust
// Relay verifies credential without learning identity
fn verify_credential(
    &self,
    database_id: DatabaseId,
    credential: AnonymousCredential,
) -> Result<()> {
    let acl = self.access_control.get(database_id)?;

    // Verify credential is valid for this ACL commitment
    // (cryptographic proof, no identity revealed)
    credential.verify_against_commitment(acl.authorized_commitment)?;

    Ok(())
}
```

## Event Types

Events are the same as ARCHITECTURE.md but encrypted:

```rust
enum VouchEventPayload {
    /// New recommendation
    Rec(RecommendationContent),

    /// Revouch from another database
    /// NOTE: Identity attribution is TBD (see "Revouch Privacy Problem" below)
    Revouch {
        original_signature: Signature,
        content: RecommendationContent,
        // TODO: How to handle source_author without leaking identity?
    },

    /// Signal-style delete request
    DeleteRequest {
        content_hash: ContentHash,
        reason: Option<String>,
    },

    /// Update database metadata
    UpdateDatabase {
        name: String,
        description: Option<String>,
    },

    /// Key rotation (v2+)
    RotateKey {
        new_key_encrypted_for_each_user: HashMap<UserIdentifier, EncryptedKey>,
    },
}
```

### Delete Request Semantics

Following Signal's model:
1. Alice creates rec, publishes to her database
2. Alice regrets it, publishes `DeleteRequest` event
3. Bob's client receives `DeleteRequest`
4. Bob's UI automatically hides the rec (soft delete from materialized view)
5. Event log still contains the rec (local-first = can't force delete)
6. Social norm: Honor delete requests or risk losing access to databases

**Enforcement:** If Bob ignores delete requests, Alice can revoke his access.

## Revouch Privacy Problem (TBD)

**Current status:** Revouch includes `source_author` identifier, which leaks identity.

**Options under consideration:**
1. Strip attribution (revouch becomes "verified content integrity" not "Alice said this")
2. Optional attribution (Bob chooses privacy vs verification)
3. Contact book events (introduce identities so subscribers can verify)

**Decision deferred:** Needs dedicated discussion in separate session. For v1, accept that revouch may leak identities, focus on core privacy guarantees (content + metadata protection).

## Multi-Device Sync (Deferred to v2)

Signal uses linked devices with per-device keys. Similar approach for Vouch:

```rust
struct DeviceGroup {
    user_id: UserIdentifier,
    devices: Vec<Device>,

    // Devices sync via special encrypted "device sync" database
    sync_database_id: DatabaseId,
}

struct Device {
    device_id: DeviceId,
    device_key: PublicKey,
    added_at: Timestamp,
    device_name: String,  // "Alice's iPhone"
}
```

All devices in group can publish to user's databases. Relay treats each device as separate authorized credential.

**Not included in v1.**

## Implementation Priorities

### V1 Must-Have (Sealed Sender Lite)

- ✅ E2EE with per-database symmetric keys
- ✅ Anonymous credentials for publishing (blind signatures)
- ✅ Event batching for timing obfuscation
- ✅ Dummy traffic for subscriber anonymity
- ✅ Invite-only subscriptions (QR code + links)
- ✅ SQLite event log + materialized view
- ✅ Basic relay with ACL enforcement
- ✅ Rec, Revouch, DeleteRequest events
- ✅ Trust-on-first-use (TOFU) identity model

### V2 Enhancements

- Key rotation events
- Zero-knowledge proofs (instead of blind signatures)
- Private information retrieval (multi-server PIR)
- Multi-device sync
- Statistical attack mitigation (improved dummy traffic)

### V3+ Future

- Post-quantum cryptography
- Hardware enclaves (SGX-style sealed sender)
- Tor integration
- Device compromise recovery

## Security Considerations

### Cryptographic Primitives

**V1 uses:**
- **Symmetric encryption**: ChaCha20-Poly1305 or AES-GCM for event payloads
- **Signatures**: Ed25519 for event signing
- **Key derivation**: HKDF for deriving keys
- **Anonymous credentials**: RSA blind signatures or BLS signatures
- **Secure random**: OS-provided CSPRNG for key generation

**V2+ may add:**
- Zero-knowledge proofs (zk-SNARKs or Bulletproofs)
- Post-quantum signatures (CRYSTALS-Dilithium)

### Key Management

**User responsibilities:**
- Securely store private signing key (lose key = lose identity)
- Back up key to separate device/paper wallet
- Protect database event_keys (shared secret)

**App responsibilities:**
- Generate strong keys (256-bit minimum)
- Store keys in OS keychain (iOS Keychain, Android Keystore)
- Never transmit private keys over network
- Warn on key changes (TOFU violations)

### Known Limitations

**What this architecture does NOT protect:**
- Device compromise (malware can read local database)
- Coerced disclosure (can't deny you have the data)
- Traffic analysis by nation-state (correlate encrypted traffic patterns)
- Social engineering (user tricked into inviting attacker)
- Screenshots/screen recording
- Quantum computers (pre-quantum crypto)

**When to use Vouch vs alternatives:**
- ✅ Protecting against curious relay operators
- ✅ Protecting against corporate surveillance
- ✅ Protecting against passive network monitoring
- ✅ Moderate-risk users (journalists, activists, privacy advocates)
- ❌ Against nation-state adversaries (use Tor + Tails)
- ❌ Against device seizure (use full-disk encryption separately)

## Comparison with Signal

| Feature | Signal | Vouch |
|---------|--------|-------|
| **Encryption** | Double ratchet (forward secrecy) | Symmetric per-database (no forward secrecy) |
| **Identity** | Phone number + username | Public key only |
| **Discovery** | Phone contacts + username search | Invite-only (QR/links) |
| **Groups** | Invite-based, admin-controlled | Databases with ACLs |
| **Sealed sender** | SGX enclaves | Blind signatures + batching |
| **Message types** | Text, media, reactions | Recs, revouches, deletes |
| **Federation** | Single Signal foundation | Single relay (v1), federated (v2+) |
| **Multi-device** | Linked devices | Deferred to v2 |
| **Metadata** | Phone numbers visible to relay | No identifiers visible to relay |

## Operational Considerations

### Relay Hosting

**V1: Single relay (Vouch foundation)**
- Centralized but E2EE
- Users trust relay for availability, not privacy
- Open source relay code (auditable)

**V2: User-hostable relays**
- Docker one-liner deployment
- Users choose relay in invite links
- No relay-to-relay communication needed

**V3: Federated relays**
- Cross-relay subscriptions
- DHT-based database discovery (still invite-only)

### Costs & Scaling

With 10,000 users, 50 subscriptions each, 100 events/month:
- Storage: 10K users × 50 dbs × 100 events × 5KB = 250GB/month
- Bandwidth: Similar (events fetched once per subscriber)
- Compute: Minimal (just forwarding encrypted blobs)

**Cost estimate:** $100-500/month for 10K users on AWS/GCP.

Much cheaper than Signal (no voice/video infrastructure).

### Abuse & Spam

**Invite-only prevents most spam:**
- Can't send unsolicited recs (no global discovery)
- Databases with bad actors get un-subscribed
- ACL revocation blocks abusive users

**Relay-level defenses:**
- Rate limiting per database (not per user, preserves anonymity)
- Size limits on events (prevent DOS)
- Retention policies (auto-delete old events)

**No content moderation needed:** Relay can't read content. Users self-moderate by unsubscribing.

## References

- [Signal Protocol](https://signal.org/docs/) - Sealed sender, double ratchet
- [Signal's Sealed Sender](https://signal.org/blog/sealed-sender/) - Metadata privacy
- [Private Information Retrieval](https://en.wikipedia.org/wiki/Private_information_retrieval)
- [Blind Signatures](https://en.wikipedia.org/wiki/Blind_signature)
- [Zero-Knowledge Proofs](https://en.wikipedia.org/wiki/Zero-knowledge_proof)
- [ARCHITECTURE.md](./ARCHITECTURE.md) - Core data model and event sourcing

## Open Questions for Next Sessions

1. **Revouch attribution:** How to verify identity chains without leaking identities? (Dedicated session needed)
2. **Contact propagation:** Should contact introductions be events? How do they flow through network? (Dedicated session needed)
3. **Statistical attack resistance:** How much dummy traffic is enough? Formal analysis needed.
4. **Key rotation triggers:** When should database owners rotate keys?
5. **Credential revocation:** How quickly can access be revoked? Real-time or eventual?
