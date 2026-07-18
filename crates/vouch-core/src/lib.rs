//! # vouch-core
//!
//! The Vouch engine core: single-writer signed claim logs with
//! content-addressed identity, the dynamic value model, canonical CBOR
//! encoding, and an in-memory claim store with a generic link index.
//!
//! This crate is deliberately I/O-free. Everything here operates on bytes
//! and in-memory structures, which makes multi-client interactions testable
//! as plain unit tests: create several [`Writer`]s, exchange
//! [`SignedEvent`]s between [`ClaimStore`]s, and assert convergence.
//!
//! The crate (together with its conformance test vectors) doubles as the
//! cross-language wire-format spec: see `VOUCH_ARCHITECTURE.md` at the
//! repository root.
//!
//! ## The shape
//!
//! A claim is a signed *header* that pins a detachable *body* by hash; its
//! identity is the hash of the header `[version, log_id, body_hash]`.
//! Nothing about ordering is signed — sync coordinates are pipe-local
//! arrival counts each store keeps about itself, and drift between pipes is
//! caught by per-log set fingerprints. There are no slots and therefore no
//! forks; redaction drops a body and leaves the signed header as a
//! tombstone. The graph that *means* something (recs, entities, vouches,
//! disavowals) lives entirely inside bodies as [`ClaimRef`] values; the
//! header is plumbing.
//!
//! ## Layering
//!
//! - [`value`] — the dynamic body model: CBOR values plus the well-known
//!   tagged types [`ClaimRef`], `Embed`, and [`BlobRef`].
//! - [`cbor`] — deterministic encoding (RFC 8949 §4.2) and a strict decoder
//!   that rejects non-canonical input.
//! - [`claim`] — headers, bodies, canonical bytes, signing and verification.
//! - [`store`] — an in-memory, order-insensitive claim store with backlink
//!   indexing, cross-path dedup, redaction, and body fill-in.
//! - [`blob`] — content-addressed media storage: verify-on-arrival,
//!   want-list driven, GC'd when redaction orphans the bytes.
//! - [`writer`] — the pen: just a signing key, no data and no position.
//! - [`database`] — the composition: N merged logs + media + your writers
//!   behind one door in and a query surface out.
//! - [`draft`] — a claim under construction: body plus attachments, minted
//!   atomically.
//! - [`sync`] — the sans-io sync engine: the wire protocol as data, the
//!   session state machine, the stateless responder, push frames, cursor
//!   state.
//! - [`peer`] — the composition with a name on the network: one actor task
//!   per database, channel pipes, follows in, broadcasts out. Still no
//!   I/O — sockets live in transport tasks; this crate ends at typed
//!   messages.

pub mod blob;
pub mod cbor;
pub mod claim;
pub mod database;
pub mod draft;
pub mod error;
pub mod fold;
pub mod keys;
pub mod peer;
pub mod rec;
pub mod storage;
pub mod store;
pub mod sync;
pub mod value;
pub mod writer;

pub use blob::{BlobStorage, BlobStore, MemoryBlobStorage};
pub use claim::{
    Claim, EventHeader, MAX_BODY_SIZE, SIGNING_DOMAIN, SignedEvent, WIRE_VERSION, signing_input,
};
pub use database::Database;
pub use draft::Draft;
pub use fold::{Comment, Component, FieldContribution, FieldState};
pub use rec::Recommendation;
pub use ed25519_dalek::Signature;
pub use error::Error;
pub use keys::LogId;
pub use peer::{
    Peer, PeerActor, PeerEvent, PipeConfig, PipeEnd, PipeId, PipeMsg, ServePolicy, pipe,
};
pub use storage::{ClaimStorage, MemoryClaimStorage};
pub use store::{
    ClaimStore, IngestReport, StateVector, StoredClaim, fingerprint_claim, fingerprint_redaction,
    redact_target,
};
pub use value::{
    BlobHash, BlobRef, ClaimHash, ClaimRef, Edges, Fields, MAX_EMBED_DEPTH, Path, PathSeg, Value,
};
pub use writer::Writer;
