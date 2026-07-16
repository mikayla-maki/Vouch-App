//! The sync engine: a sans-io session protocol between [`Database`]s.
//!
//! Like everything in this crate, the cut is *logic under invariants, dumb
//! plumbing at the edge*. The protocol is plain data ([`Request`] /
//! [`Response`] — these types are the wire format); the engine is a pure
//! state machine ([`SyncSession`]) that produces requests and consumes
//! responses; the server half is one stateless function ([`respond`]). A
//! transport — HTTP client, iroh stream, function call — just moves
//! messages, and carries no protocol logic to get wrong. No async runtime,
//! no I/O, no clock: the app's executor, the relay's tokio, and a test's
//! `while let` loop all drive the same engine, which is why every network
//! fault is testable as a plain deterministic unit test.
//!
//! The engine's only state is [`PeerCursor`] rows — everything else lives
//! in the [`Database`], whose ingest already guarantees verification,
//! idempotence, and order-independence. That split makes the recovery
//! story for every crash, timeout, and disconnect identical: throw the
//! session away and start a new one. Cursors advance only after the data
//! they describe has ingested; peers hold no conversation state; replays
//! deduplicate to nothing.
//!
//! Catch-up rides arrival cursors (fast, advisory), and every session
//! settles with a per-log fingerprint exchange that catches the drift
//! cursors can't see; mismatch falls back to full set reconciliation by
//! claim-hash list. Sessions move claims only — media is non-syncing by
//! default. A claim's `BlobRef` becomes a want, and bytes move when the
//! wanting side asks (`GetBlob`), or arrive a beat early via the
//! `PutBlob` fast-track (the holder answering the ask it knows is
//! coming — the `Notify` idea, applied to bytes). Either way the pull is
//! the only thing correctness rests on.
//!
//! Real-time delivery is layered on top, not into the protocol: a
//! [`Notify`] frame (an unsolicited `Status` with the new events attached)
//! is fanned out over any live channel and applied by [`apply_notify`] —
//! zero round trips when the fingerprints agree, a doorbell for an
//! ordinary session when they don't. Correctness never depends on the push
//! channel; it only makes the pull path's answer arrive sooner.
//!
//! [`Database`]: crate::Database

pub mod error;
pub mod notify;
pub mod protocol;
pub mod respond;
pub mod session;
pub mod state;

pub use error::Error;
pub use notify::{NotifyReport, apply_notify, notify_for};
pub use protocol::{BATCH, InstanceId, MAX_SERVE_BATCH, Notify, Request, Response};
pub use respond::respond;
pub use session::{SyncReport, SyncSession};
pub use state::{MemorySyncState, PeerCursor, SyncState};

use crate::database::Database;

/// Run a session to completion over any exchange function — the whole
/// driver, with the I/O abstracted to one closure. A transport error
/// aborts cleanly (cursors are never ahead of ingested data); just drive a
/// fresh session later.
pub fn drive(
    db: &mut Database,
    state: &mut dyn SyncState,
    mut session: SyncSession,
    mut exchange: impl FnMut(Request) -> Result<Response, Error>,
) -> Result<SyncReport, Error> {
    while let Some(request) = session.next_request(db) {
        let response = exchange(request)?;
        session.feed(db, state, response)?;
    }
    Ok(session.finish())
}
