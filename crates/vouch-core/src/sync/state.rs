//! The engine's only state: cursors.
//!
//! Everything else a sync session touches lives in the [`Database`]
//! (claims, blobs, arrival orders) or in the messages. What survives
//! between sessions is one small row per `(peer, log)`, and losing it is
//! never wrong — a missing cursor just means a full re-pull that ingest
//! dedup flattens into a no-op.
//!
//! [`Database`]: vouch_core::Database

use std::collections::HashMap;

use crate::LogId;

use super::error::Error;
use super::protocol::InstanceId;

/// Where we stand with one peer on one log. All four fields are *about
/// the pipe*, not about the data: two databases that have never met still
/// converge through a third, cursors or no cursors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PeerCursor {
    /// The peer incarnation the counts are valid against. `None` means
    /// we've never completed a `Status` with this peer. Any mismatch on
    /// contact resets the whole row — counts against a dead arrival order
    /// are noise.
    pub instance: Option<InstanceId>,
    /// How many of this log's claims we have *received from this pipe*: a
    /// position in the peer's arrival order. Advances only after the
    /// received batch ingested cleanly — our half of the monotonicity
    /// bargain.
    pub pull: u64,
    /// How many of *our* arrival positions for this log we have pushed to
    /// this peer. A position in our own arrival order; the peer never
    /// sees it.
    pub push: u64,
    /// The peer's fingerprint for this log the last time we finished a
    /// full reconciliation with it. A fingerprint mismatch equal to this
    /// value is the *known benign* difference — claims we hold that this
    /// peer won't take (e.g. a relay that only the log's owner may publish
    /// to) — and doesn't trigger another reconciliation. Cleared whenever
    /// the peer's set changes.
    pub settled: Option<[u8; 32]>,
}

/// Cursor persistence. Memory in tests, a SQLite table in the app
/// (vouch-store). Dumb rows, like every backend in this codebase: the
/// session decides what the numbers mean.
pub trait SyncState: Send {
    fn cursor(&self, peer: &str, log: &LogId) -> Result<PeerCursor, Error>;
    fn set_cursor(&mut self, peer: &str, log: &LogId, cursor: PeerCursor) -> Result<(), Error>;
}

/// In-memory cursor rows (tests, simulations).
#[derive(Default)]
pub struct MemorySyncState {
    rows: HashMap<(String, LogId), PeerCursor>,
}

impl MemorySyncState {
    pub fn new() -> MemorySyncState {
        MemorySyncState::default()
    }
}

impl SyncState for MemorySyncState {
    fn cursor(&self, peer: &str, log: &LogId) -> Result<PeerCursor, Error> {
        Ok(self
            .rows
            .get(&(peer.to_string(), *log))
            .copied()
            .unwrap_or_default())
    }

    fn set_cursor(&mut self, peer: &str, log: &LogId, cursor: PeerCursor) -> Result<(), Error> {
        self.rows.insert((peer.to_string(), *log), cursor);
        Ok(())
    }
}
