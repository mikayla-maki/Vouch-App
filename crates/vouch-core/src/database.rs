//! The composition: a Vouch database.
//!
//! A [`Database`] is what you get by merging your N subscribed logs — the
//! database you'd have if everyone had written into it together. It
//! composes the three primitives, which otherwise never touch:
//!
//! - [`Writer`] signs (the pen),
//! - [`ClaimStore`] converges (the notebook),
//! - [`BlobStore`] caches (the shoebox of photos),
//!
//! behind one door in (`ingest`/`ingest_blob` — from any pipe: network,
//! file, another `Database`) and a read-only query surface out. Minting —
//! attach bytes, pin them in a body, sign, ingest — lives here and only
//! here, so blob-before-claim sequencing and "your own claims are ordinary
//! claims" are structural, not conventions.
//!
//! This is still pure state: no I/O, no transports, no fetch policy. The
//! sync engine is a `Database` plus pipes and scheduling; a relay is a
//! `Database` with no writers. Two `Database`s exchanging `serve_since`
//! streams are a complete sync session, testable without I/O.

use std::collections::HashMap;

use crate::blob::BlobStore;
use crate::claim::SignedEvent;
use crate::error::Error;
use crate::keys::LogId;
use crate::store::{ClaimStore, IngestReport};
use crate::value::{BlobHash, BlobRef, Value};
use crate::writer::Writer;

/// N merged logs, their media, and the writers for the logs you own.
#[derive(Default)]
pub struct Database {
    claims: ClaimStore,
    blobs: BlobStore,
    writers: HashMap<LogId, Writer>,
}

impl Database {
    pub fn new() -> Database {
        Database::default()
    }

    // ── Owned logs ──────────────────────────────────────────────────────

    /// Adopt a writer: claims can now be minted into its log. Key custody
    /// (keychain, mnemonic restore) is the caller's business; this just
    /// holds the pen. Returns the log's id.
    pub fn add_writer(&mut self, writer: Writer) -> LogId {
        let id = writer.id();
        self.writers.insert(id, writer);
        id
    }

    /// Create a fresh identity from OS randomness and adopt it.
    pub fn create_log(&mut self) -> Result<LogId, Error> {
        Ok(self.add_writer(Writer::generate()?))
    }

    /// The logs this database can write to.
    pub fn owned_logs(&self) -> impl Iterator<Item = &LogId> {
        self.writers.keys()
    }

    // ── Minting (the composition) ───────────────────────────────────────

    /// Store media bytes locally and return the [`BlobRef`] to pin in a
    /// claim body. Attach-then-claim ordering is enforced by the API shape:
    /// you can't pin a ref you haven't been handed.
    pub fn attach(&mut self, bytes: Vec<u8>, mime: impl Into<String>) -> BlobRef {
        let size = bytes.len() as u64;
        let hash = self.blobs.put(bytes);
        BlobRef {
            hash,
            size,
            mime: mime.into(),
        }
    }

    /// Mint a claim into an owned log: sign it, ingest it like any other
    /// event, and return the artifact (for a publish queue). After this
    /// returns, the claim is part of local state — your own claims are
    /// ordinary claims.
    pub fn claim(
        &mut self,
        log: &LogId,
        timestamp_ms: i64,
        body: Value,
    ) -> Result<SignedEvent, Error> {
        let writer = self.writers.get_mut(log).ok_or(Error::NotOurLog(*log))?;
        let event = writer.claim(timestamp_ms, body)?;
        self.claims.ingest(event.clone())?;
        Ok(event)
    }

    // ── The door in ─────────────────────────────────────────────────────

    /// Ingest one signed event from any pipe. Order-insensitive,
    /// idempotent; see [`ClaimStore::ingest`].
    pub fn ingest(&mut self, event: SignedEvent) -> Result<IngestReport, Error> {
        self.claims.ingest(event)
    }

    /// Ingest blob bytes from any pipe, verified against the pinning hash.
    /// Returns whether the bytes were new; see [`BlobStore::insert_verified`].
    pub fn ingest_blob(&mut self, pinned: BlobHash, bytes: Vec<u8>) -> Result<bool, Error> {
        self.blobs.insert_verified(pinned, bytes)
    }

    // ── The query surface ───────────────────────────────────────────────

    /// The merged claim graph: timelines, logs, backlinks, fingerprints,
    /// `serve_since` — the UI's read surface and the serve side of sync.
    pub fn claims(&self) -> &ClaimStore {
        &self.claims
    }

    /// The media cache.
    pub fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    /// The fetch want-list: blobs that live bodies pin and we don't hold.
    pub fn missing_blobs(&self) -> Vec<BlobRef> {
        self.claims.missing_blobs(&self.blobs)
    }

    // ── Maintenance ─────────────────────────────────────────────────────

    /// Drop blobs no live body references (cooperative deletion for media).
    pub fn gc_blobs(&mut self) -> Vec<BlobHash> {
        self.blobs.gc(&self.claims)
    }
}
