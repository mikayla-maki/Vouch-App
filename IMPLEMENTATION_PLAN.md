# Vouch Desktop UI Implementation Plan

## Overview

Vouch is a local-first, privacy-preserving database of recommendations. The desktop UI features a **master-detail layout** where a feed of recommendations appears alongside a detail panel.

See [VOUCH_ARCHITECTURE.md](./VOUCH_ARCHITECTURE.md) for the full system design.

## Current State

**Phases 1-3 are complete.** The UI shell is fully functional with mock data:
- Master-detail layout with collapsible sidebar
- Feed panel with search and filtering
- Detail panel with vouch chain visualization
- Theme switching (light/dark)

**Remaining work:** Data layer, CRUD operations, contact management, and sync.

## Proposed Layout

```
┌─────────────────────────────────────────────────────────────────┐
│  Vouch                                            [👤] [⚙️]     │
├────────────┬─────────────────┬──────────────────────────────────┤
│ Filters    │ [🔍 Search...]  │                                  │
│            │ ────────────────│                                  │
│ • All      │                 │     [Photo/Avatar]               │
│ • Mine     │ ┌─────────────┐ │                                  │
│ • Friends  │ │ 🍜 Thai Pl  │ │     "Thai Place Downtown"        │
│            │ │ "Amazing..."│ │     ──────────────────────       │
│ ────────── │ │ via Mom •2h │ │                                  │
│            │ └─────────────┘ │     📝 Original recommendation:  │
│ Databases  │                 │     "Best pad thai I've ever..." │
│ • Mom      │ ┌─────────────┐ │                                  │
│ • Alice    │ │ John's Auto │ │     👤 Source: Mom's Food Recs   │
│ • Bob      │ │ "Avoid..."  │ │     🔄 Vouched by: You, Alice    │
│            │ │ via Alice   │ │                                  │
│            │ └─────────────┘ │     💬 [Add a note]              │
│            │                 │     ──────────────────────       │
│            │      ...        │     Related recs (3)             │
│            │                 │                                  │
├────────────┼─────────────────┼──────────────────────────────────┤
│            │ [+ New Rec]     │   [Vouch]  [Disavow]  [Edit]     │
└────────────┴─────────────────┴──────────────────────────────────┘
```

## Component Hierarchy

```
VouchApp (root)
├── Sidebar (collapsible)
│   ├── FilterList (all/mine/subscriptions)
│   └── DatabaseList
│       └── DatabaseRow[] (subscribed databases with petnames)
│
├── FeedPanel
│   ├── SearchBar
│   ├── FeedHeader (sort options)
│   └── FeedList
│       └── RecordCard[] (virtualized)
│           ├── RecordThumbnail
│           ├── RecordSummary (subject, excerpt)
│           └── RecordMeta (source database, timestamp)
│
├── DetailPanel (shown when rec selected)
│   ├── DetailHeader
│   │   └── SubjectPhoto / Placeholder
│   ├── SubjectInfo (name)
│   ├── RecommendationContent
│   ├── VouchChain (original source, who vouched)
│   ├── RelatedRecs (other recs for same subject)
│   └── ActionBar (vouch, disavow, edit)
│
└── Modals
    ├── NewRecommendationModal
    ├── InviteModal (generate/accept invites)
    └── SettingsModal
```

## Implementation Phases

### Phase 1: Core Data Types & Stub Views ✅ COMPLETE
- Data types in `data.rs`
- App shell in `app.rs`
- Stub UI components

### Phase 2: Feed View ✅ COMPLETE
- Search bar with real-time filtering
- RecordCard with full design
- Feed sorting and virtualized scrolling

### Phase 3: Detail View ✅ COMPLETE
- Subject header with placeholder
- Full recommendation text display
- Vouch chain visualization
- Related recs section

### Phase 4: Create/Edit Flow (IN PROGRESS)
**Goal**: CRUD for recommendations in your own database

- [x] New Recommendation modal UI
- [ ] Wire modal save to data layer
- [ ] Edit existing recommendation
- [ ] Disavow flow with confirmation

### Phase 5: Data Layer Integration
**Goal**: Connect to real persistence with event sourcing

See [PHASE_5_DATA_LAYER.md](./PHASE_5_DATA_LAYER.md) for detailed plan.

- [ ] SQLite database setup (event log + materialized views)
- [ ] Event store implementation (append-only log)
- [ ] Projector (events → materialized views)
- [ ] Query layer for feed/detail views
- [ ] Reactivity - UI updates when data changes
- [ ] Key generation and storage (Ed25519 keypairs)

### Phase 6: Database & Subscription Management
**Goal**: Manage your databases and subscriptions

- [ ] Create new database (generates keypair)
- [ ] Database settings (name, description via UpdateDatabase events)
- [ ] View subscribed databases with petnames
- [ ] Edit petnames for subscribed databases
- [ ] Unsubscribe from database
- [ ] Four-name display resolution (petname → verified → proposed → unknown)

### Phase 7: Invitation System
**Goal**: Share and accept database invitations

- [ ] Generate invite link/QR for your database
- [ ] Accept invite (stores database_id, event_key, relay_url)
- [ ] Import from `vouch://invite?...` URL scheme
- [ ] Invitation UI (show what you're subscribing to)

### Phase 8: Vouch Flow
**Goal**: Vouch for others' recommendations

- [ ] Vouch action on recommendations from subscriptions
- [ ] Include original signature in vouch event
- [ ] Verify vouch chain signatures on display
- [ ] Show vouch provenance in detail view

### Phase 9: Sync
**Goal**: Real-time sync via relay

- [ ] WebSocket relay connection
- [ ] Authenticate as database owner (signature challenge)
- [ ] Publish encrypted events to relay
- [ ] Fetch and decrypt events from subscriptions
- [ ] Handle reconnection and offline queue
- [ ] Sync state tracking (last_synced_sequence per subscription)

### Phase 10: Key Management
**Goal**: Secure key storage and backup

- [ ] BIP39 mnemonic generation for new databases
- [ ] Show recovery phrase on database creation
- [ ] Restore database from mnemonic
- [ ] Platform keychain integration (macOS Keychain)

## File Structure

```
src/
├── main.rs              # App initialization
├── app.rs               # VouchApp root entity
├── data.rs              # Re-exports from store, mock data for dev
├── theme.rs             # Theme switching
├── assets.rs            # Asset loader
├── crypto/
│   ├── mod.rs           # Crypto exports
│   ├── keys.rs          # Ed25519 keypair management
│   ├── signing.rs       # Event signing/verification
│   └── encryption.rs    # ChaCha20-Poly1305 for events
├── store/
│   ├── mod.rs           # Public API exports
│   ├── database.rs      # SQLite connection and migrations
│   ├── event.rs         # VouchEvent, SignedVouchEvent types
│   ├── event_store.rs   # Append and read events
│   ├── projector.rs     # Project events to materialized views
│   ├── recommendation_store.rs  # Query recommendations
│   ├── database_registry.rs     # Query known databases
│   └── error.rs         # Store error types
├── sync/
│   ├── mod.rs           # Sync exports
│   ├── relay.rs         # WebSocket relay client
│   ├── protocol.rs      # Relay protocol types
│   └── invitation.rs    # Invite generation/parsing
├── ui/
│   ├── mod.rs           # UI exports
│   ├── feed_panel.rs    # Feed list view
│   ├── detail_panel.rs  # Rec detail view
│   ├── record_card.rs   # Card component
│   ├── search_bar.rs    # Search input
│   ├── sidebar.rs       # Collapsible sidebar
│   └── modals/
│       ├── mod.rs
│       ├── new_recommendation.rs
│       ├── invite.rs
│       └── settings.rs
└── components/
    ├── avatar.rs        # Database/subject avatars
    └── timestamp.rs     # Relative time display
```

## Key Architecture Concepts

### Database = Identity
Each database is identified by its public key (`DatabaseId`). Your "Food Recs" and "Book Recs" are separate identities. Others can't tell they belong to the same person.

### Events Are Signed
Every event is signed by the database owner. Vouches include the original author's signature, creating a verifiable chain.

### Four-Name Model
1. **DatabaseId** - The public key (not human-readable)
2. **Self-proposed name** - Set by owner via UpdateDatabase
3. **Proposed names** - Names others claim in vouches
4. **Petname** - Your private local name (takes precedence)

### Vouch vs Subscribe
- **Subscribe**: "I want to see this database" (their data, synced locally)
- **Vouch**: "I endorse this AND host it" (your database, you control durability)

## Dependencies

```toml
[dependencies]
gpui = "0.2"
gpui-component = "0.5"

# Data layer (Phase 5)
rusqlite = { version = "0.32", features = ["bundled"] }
uuid = { version = "1.8", features = ["v4", "serde"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"

# Crypto (Phase 5)
ed25519-dalek = "2.1"
chacha20poly1305 = "0.10"
rand = "0.8"

# Key backup (Phase 10)
bip39 = "2.0"

# Sync (Phase 9)
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
tokio-tungstenite = "0.21"
```

## Design Decisions

### Desktop Adaptations

| Mobile Concept | Desktop Adaptation |
|----------------|-------------------|
| Two separate screens | Side-by-side panels |
| Tap to navigate | Click to select, detail shows in panel |
| Pull to refresh | Background refresh + manual button |
| Bottom nav | Top toolbar + collapsible sidebar |
| Full-screen modals | Floating modals |

### Color Scheme

Soft, approachable palette:
- **Primary**: Soft pink (`#F8BBD9` / `#EC407A`)
- **Secondary**: Pastel purple (`#CE93D8` / `#AB47BC`)
- **Accent**: Pastel blue (`#90CAF9` / `#42A5F5`)
- **Background**: Warm white (`#FFF8FA`)
- **Surface**: Light lavender (`#F3E5F5`)
- **Text**: Soft charcoal (`#424242`)
