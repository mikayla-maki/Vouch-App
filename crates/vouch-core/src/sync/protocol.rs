//! The sync protocol as plain data.
//!
//! Sans-io: these messages *are* the wire protocol. The engine
//! ([`SyncSession`](crate::SyncSession)) produces [`Request`]s and consumes
//! [`Response`]s; the responder ([`respond`](crate::respond)) is the server
//! half. A transport is anything that moves a `Request` to a peer and
//! brings a `Response` back — an HTTP client, an iroh stream, a function
//! call into another [`Database`](vouch_core::Database) in the same
//! process. Transports carry no protocol logic, exactly as storage
//! backends carry no convergence logic.
//!
//! Authentication is deliberately not modeled here. A relay that restricts
//! who may `Publish` or `PutBlob` (signature-challenge auth for log owners)
//! enforces that *around* the protocol, at the transport layer; between
//! mutually trusting peers every message is open. Nothing in the engine
//! depends on the answer — published garbage fails verification at ingest,
//! which is the real gate.

use crate::{BlobHash, ClaimHash, LogId, SignedEvent};

/// One incarnation of a peer's arrival order. Cursors count positions in
/// the peer's per-log arrival sequence, which is only meaningful while
/// that sequence keeps growing in place — so a peer mints a fresh instance
/// whenever its arrival order could have rewound (a relay boots, a
/// database file is created or restored). A client seeing an unfamiliar
/// instance resets its cursors to zero and re-pulls; ingest dedup makes
/// the re-download harmless. This is the cheap *prevention* for stale
/// cursors; the fingerprint exchange remains the *detection* for peers
/// that get it wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub struct InstanceId(pub [u8; 16]);

/// How many events a session asks for per `Since` or sends per `Publish` /
/// `Claims` message — the paging unit, so no single message grows with log
/// size. (Bodies are capped at 64 KiB each, so this also bounds message
/// bytes.)
pub const BATCH: u64 = 256;

/// The responder's own ceiling on a `Since` reply, whatever the request
/// asked for.
pub const MAX_SERVE_BATCH: u64 = 1024;

/// What one side asks of the other. Every request is answerable from a
/// `Database` alone — peers hold no per-conversation state, which is what
/// lets a session die at any message boundary and a fresh one finish the
/// job.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub enum Request {
    /// "Where do you stand on this log?" → count, fingerprint, instance.
    /// Opens every per-log exchange and settles it afterward.
    Status { log: LogId },
    /// Incremental pull: "I have `have` of this log from you — send up to
    /// `max` more." `have` is a cursor against the *responder's* arrival
    /// order.
    Since { log: LogId, have: u64, max: u64 },
    /// Full set reconciliation, step one: every claim id you hold for this
    /// log, with a has-body bit ("have" means "have the body").
    Hashes { log: LogId },
    /// Reconciliation fetch: specific claims by id. Returns whatever the
    /// responder holds — content or signed tombstone; bodies fill in on
    /// ingest.
    Claims { ids: Vec<ClaimHash> },
    /// Push events to the peer. Idempotent: every event is verified and
    /// deduplicated by the receiver's ingest, so redelivery and
    /// multi-device overlap cost nothing.
    Publish { events: Vec<SignedEvent> },
    /// Blob bytes by hash — THE blob transfer. Media moves only when the
    /// wanting side asks: sessions never carry bytes, fingerprints never
    /// see them, and "non-syncing" is the default posture (fetch when the
    /// UI wants to render, eagerly only where a pipe opts in). The want
    /// derives from claims already held, so there is nothing to negotiate
    /// and no ordering to get wrong.
    GetBlob { hash: BlobHash },
    /// One blob's bytes, unsolicited: the answer to the `GetBlob` the
    /// sender knows is coming. A fast-track exactly like [`Notify`] is for
    /// claims — conceptually still pull-based, the holder just answers
    /// early (a p2p mint is two frames: the claim, then its bytes).
    /// Advisory and droppable: the receiver accepts iff its own want-list
    /// asks (so the bytes can never arrive "before" their claim — an
    /// unwanted supply is declined and the pull path remains the truth).
    /// The receiver hashes the bytes itself; the content address is the
    /// only name a blob has.
    PutBlob { bytes: Vec<u8> },
}

/// The answers. A response carries no question context — the session knows
/// what it asked.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub enum Response {
    /// Answer to [`Request::Status`].
    Status {
        /// How many claims the responder holds for the log (content and
        /// tombstones; never regresses within an instance).
        count: u64,
        /// The responder's set fingerprint for the log (see
        /// [`ClaimStore::fingerprint`](vouch_core::ClaimStore::fingerprint)).
        fingerprint: [u8; 32],
        /// The incarnation its arrival order — and thus any cursor — is
        /// valid against.
        instance: InstanceId,
    },
    /// Answer to [`Request::Since`] and [`Request::Claims`].
    Events { events: Vec<SignedEvent> },
    /// Answer to [`Request::Hashes`].
    Hashes { entries: Vec<(ClaimHash, bool)> },
    /// Answer to [`Request::Publish`] and [`Request::PutBlob`].
    Ack {
        /// Events (or blobs) that were new to the responder. Advisory —
        /// cursor advancement never depends on it.
        stored: u64,
        /// Events that failed verification and were dropped. Non-zero
        /// between honest peers means something is corrupting artifacts in
        /// transit.
        rejected: u64,
    },
    /// Answer to [`Request::GetBlob`]; `None` if the responder doesn't
    /// hold it (a want never expires — any other pipe may heal it later).
    Blob { bytes: Option<Vec<u8>> },
}

/// The push frame: server-initiated, so it is neither a [`Request`] nor a
/// [`Response`] — a transport with a live channel (WebSocket, SSE, an iroh
/// stream) fans these out to subscribers when new claims land.
///
/// It is an unsolicited `Status` with the events attached: the receiver
/// ingests the events (safe blind — verification lives at ingest) and then
/// runs the same settle decision a session would, against the carried
/// `(count, fingerprint, instance)`. Best case the claim is applied and
/// the log fully settled with **zero round trips**; worst case the frame
/// degrades into a doorbell — "something changed, run a session" — with a
/// free claim attached. See [`apply_notify`](crate::apply_notify).
///
/// With `events` empty it is a pure heartbeat: a cheap anti-entropy ping
/// that lets an idle subscriber confirm it is still settled (or discover
/// it isn't) without sending anything.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub struct Notify {
    pub log: LogId,
    /// The newly landed events, in the sender's arrival order. Bodies
    /// included; blobs are NOT — a pushed claim pinning media lands as
    /// content plus a want (push channels don't carry megabytes).
    pub events: Vec<SignedEvent>,
    /// The sender's claim count for the log, after ingesting `events`.
    pub count: u64,
    /// The sender's fingerprint for the log, after ingesting `events`.
    pub fingerprint: [u8; 32],
    /// The incarnation those coordinates are valid against.
    pub instance: InstanceId,
}
