//! An order-insensitive claim store with generic link indexing.
//!
//! Convergence is the store's contract: any two stores holding the same set
//! of received artifacts are in the same state, regardless of ingest order.
//! Identity is content (the header hash), so there are no slots, no forks,
//! and no conflicts — the store is a pile of independently verified claims,
//! plus two monotone side effects:
//!
//! - **Redaction.** A `redact` claim (own log only) asks conformant stores
//!   to forget a body. The header and signature stay — a *signed tombstone*
//!   — so the claim's existence remains verifiable and serveable while its
//!   content is gone. Monotone: no un-redact, in any arrival order.
//!
//! - **Body fill-in.** A header-only event (served tombstone, stripped by a
//!   lossy peer) stores as a bodiless claim; if the body later arrives from
//!   any pipe, it verifies against the header's body hash and attaches.
//!   Only a signed redact claim makes bodilessness permanent — so a peer
//!   stripping bodies is a recoverable nuisance, not a censor.
//!
//! Embedded claims are *content, not rows*: a quote is part of the speech
//! that carries it, verified in place and read by recursion
//! ([`StoredClaim::embeds`]), never extracted into the store. Its edges —
//! the embed itself, and every ref and blob inside it — index under the
//! *quoting* claim, so redacting the quote removes the quote, interior and
//! all, with nothing left behind. The store's rows are exactly the
//! top-level events its logs delivered.
//!
//! `ClaimStore` is the *logic*; rows live behind the
//! [`ClaimStorage`](crate::storage::ClaimStorage) trait (memory for tests,
//! SQLite in the app), so the invariants above are written exactly once no
//! matter the backend.

use std::collections::{BTreeMap, HashMap};

use crate::blob::BlobStore;
use crate::claim::{Claim, EventHeader, SignedEvent};
use crate::error::Error;
use crate::keys::LogId;
use crate::storage::{ClaimStorage, MemoryClaimStorage};
use crate::value::{BlobHash, BlobRef, ClaimHash, ClaimRef, Edges, Path, Value};

/// Read failures are corruption or misconfiguration, not recoverable
/// conditions a query caller can act on — so reads panic rather than
/// infecting every query signature with `Result`. Mutations (`ingest`)
/// propagate storage errors properly.
const READ: &str = "claim storage read failed";

/// A claim at rest: original artifact, decoded views, extracted link
/// metadata. `body: None` means tombstone (if redacted) or body-not-yet-seen
/// (check [`ClaimStore::redaction`] to tell which).
#[derive(Debug, Clone)]
pub struct StoredClaim {
    pub signed: SignedEvent,
    pub header: EventHeader,
    pub body: Option<Value>,
    /// Every outgoing claim edge, collected *through* embeds: each
    /// `ClaimRef` in the body, one edge per verified embed (a quote is the
    /// strongest form of reference), and every ref inside those embeds —
    /// all attributed to this claim. See [`Value::collect_edges`].
    pub refs: Vec<(Path, ClaimRef)>,
    /// Every outgoing blob edge, collected the same way: a quote that shows
    /// a photo pins that photo, under this claim.
    pub blobs: Vec<(Path, BlobRef)>,
    /// Position in THIS store's per-log arrival order: "the sequence
    /// number is always the count" — how many claims of this log the store
    /// held when this one landed. Local metadata: excluded from state
    /// vectors and fingerprints, meaningless to any other store. This is
    /// what sync cursors index — a cursor is just "how many of this log's
    /// claims I've received from this pipe."
    pub arrival: u64,
    /// When THIS store first ingested the claim, by the caller's clock
    /// (Unix ms; 0 when no clock was supplied). Local metadata: the
    /// author's claimed time is the body's `at`; this is ours.
    pub received_at: i64,
}

impl StoredClaim {
    /// The verified claims quoted in this body, shallow, with the path
    /// where each sits. Embeds are content, not rows — this is how they
    /// are read. Recurse by calling [`Value::embedded_claims`] on each
    /// returned claim's body; identity is `claim.header.id()`. Embeds that
    /// fail verification are omitted (unrenderable garbage the container's
    /// author signed). Empty for tombstones.
    pub fn embeds(&self) -> Vec<(Path, Claim)> {
        match &self.body {
            Some(b) => b.embedded_claims(),
            None => Vec::new(),
        }
    }
}

/// What one `ingest` call did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// The claim's id, if the event was newly added.
    pub newly_stored: Option<ClaimHash>,
    /// Events that were already fully present.
    pub duplicates: usize,
    /// Embeds whose interiors were not indexed: verification failed, or
    /// nesting exceeded [`MAX_EMBED_DEPTH`](crate::value::MAX_EMBED_DEPTH).
    /// The containing claim is stored regardless (its author signed the
    /// garbage; that is recorded, not endorsed) — such embeds just
    /// contribute no edges and won't render.
    pub skipped_embeds: usize,
    /// Bodies suppressed because the claim is redacted.
    pub redacted_skips: usize,
    /// Bodies attached to claims previously known only by header.
    pub bodies_attached: usize,
    /// Redactions that took effect during this ingest (a new redaction
    /// authority recorded, or a held body actually dropped). Zero on
    /// idempotent replays.
    pub redactions_applied: usize,
}

/// One claim's contribution to a [`StateVector`]: header bytes and body
/// bytes if held.
pub type ClaimSnapshot = (Vec<u8>, Option<Vec<u8>>);

/// A canonical snapshot of the store's *convergent* state: exactly the
/// replicated substance — headers, bodies, redactions — and nothing local.
///
/// Local metadata (arrival order, receive times, which valid signature we
/// hold — an author can produce many; any one of them is equal proof of
/// authorship) is deliberately excluded: sync exchanges claims by id and
/// would never propagate it, so including it would let two fully-synced
/// stores compare unequal forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateVector {
    pub claims: BTreeMap<ClaimHash, ClaimSnapshot>,
    /// target → redacting claim (the authority making bodilessness monotone)
    pub redactions: BTreeMap<ClaimHash, ClaimHash>,
}

/// The claim store: convergence logic over a [`ClaimStorage`] backend.
pub struct ClaimStore {
    storage: Box<dyn ClaimStorage>,
    /// Panic poisoning, like `Mutex`: set while an ingest is in flight and
    /// cleared on orderly exit. If a panic unwinds mid-ingest (and someone
    /// catches it), the store may hold a partial write set — every later
    /// call fails loudly instead of serving half-applied state.
    poisoned: bool,
}

impl Default for ClaimStore {
    fn default() -> ClaimStore {
        ClaimStore::new()
    }
}

impl ClaimStore {
    /// An in-memory store (tests, simulations, relays).
    pub fn new() -> ClaimStore {
        ClaimStore::with_storage(Box::new(MemoryClaimStorage::new()))
    }

    /// A store over an injected backend (the app injects SQLite here).
    pub fn with_storage(storage: Box<dyn ClaimStorage>) -> ClaimStore {
        ClaimStore {
            storage,
            poisoned: false,
        }
    }

    fn guard(&self) {
        assert!(
            !self.poisoned,
            "claim store poisoned: an ingest did not complete or roll back \
             cleanly (a panic unwound mid-ingest, or commit and rollback both \
             failed); state may be partial"
        );
    }

    /// Verify and store one signed event, applying any redaction its body
    /// carries. Order-insensitive and idempotent.
    ///
    /// Embedded claims are verified in place and indexed as edges of this
    /// event — never stored as rows of their own. Embed problems and
    /// redaction skips are counted in the report, never fatal. Verification
    /// errors occur before any mutation; storage errors surface as
    /// [`Error::Storage`].
    ///
    /// The whole call is one transaction when the backend supports them: on
    /// SQLite a crash mid-ingest persists nothing. Without transactions,
    /// write ordering makes `put_claim` the commit point — a partial ingest
    /// leaves only idempotent index rows that redelivery of the same event
    /// completes.
    pub fn ingest(&mut self, event: SignedEvent) -> Result<IngestReport, Error> {
        self.ingest_at(event, 0)
    }

    /// [`ingest`](Self::ingest) with the caller's clock: `received_at`
    /// (Unix ms) is recorded on newly stored claims as local metadata.
    /// vouch-core never reads a clock itself — time is injected.
    pub fn ingest_at(
        &mut self,
        event: SignedEvent,
        received_at: i64,
    ) -> Result<IngestReport, Error> {
        self.guard();
        self.storage.begin()?;
        self.poisoned = true;
        let mut report = IngestReport::default();
        let outcome = self.ingest_inner(event, received_at, &mut report);
        match outcome {
            Ok(()) => match self.storage.commit() {
                Ok(()) => {
                    self.poisoned = false;
                    Ok(report)
                }
                // Commit failed (e.g. SQLITE_BUSY): the transaction may still
                // be open. Roll back; un-poison only if that restored a clean
                // state, so a transient commit failure leaves a usable store.
                Err(e) => {
                    if self.storage.rollback().is_ok() {
                        self.poisoned = false;
                    }
                    Err(e)
                }
            },
            Err(e) => {
                // Only un-poison if the rollback restored a clean state.
                if self.storage.rollback().is_ok() {
                    self.poisoned = false;
                }
                Err(e)
            }
        }
    }

    fn ingest_inner(
        &mut self,
        event: SignedEvent,
        received_at: i64,
        report: &mut IngestReport,
    ) -> Result<(), Error> {
        let claim = event.verify()?;
        let id = event.id();

        // Is THIS claim an engine-recognized redaction of its own log?
        let own_redaction = redact_target(claim.header.log_id, claim.body.as_ref());

        // A redaction takes effect whenever a verified redact body is SEEN —
        // even if this event's own body won't be stored. (A claim can never
        // redact itself: its body would have to contain its own hash, which
        // is a hash cycle.)
        if let Some(target) = own_redaction
            && self.apply_redaction(target, id)?
        {
            report.redactions_applied += 1;
        }

        // A redact claim's body is pure machinery (a hash pointer, no user
        // content) and the only carrier of the redaction it encodes, so it
        // is never suppressed — losing it would un-redact the original on
        // any store that learned the redact only as a tombstone.
        let suppressed = own_redaction.is_none() && self.storage.redaction(&id)?.is_some();
        let body = if suppressed { None } else { claim.body };
        if suppressed && event.body_bytes.is_some() {
            report.redacted_skips += 1;
        }

        let existing = self.storage.get_claim(&id)?;
        if let Some(c) = &existing {
            // The first valid signature we saw stays. An author can mint
            // many valid signatures for one header; all are equal proof of
            // authorship, so which one we hold is local metadata, not state
            // (and is excluded from StateVector accordingly).
            if c.body.is_some() || body.is_none() {
                report.duplicates += 1;
                return Ok(());
            }
            // Fall through: we hold a header-only claim and now have a
            // verified body for it.
        }

        // The claim's outgoing edges, collected THROUGH its embeds: a quote
        // is content, not a row, so everything it references — including
        // the quoted claim itself — indexes under this claim, and dies with
        // it on redaction.
        let edges = match &body {
            Some(b) => b.collect_edges(),
            None => Edges::default(),
        };
        report.skipped_embeds += edges.skipped;
        let Edges { refs, blobs, .. } = edges;

        for (_path, target) in &refs {
            self.storage.add_backlink(target.hash, id)?;
        }
        for (_path, b) in &blobs {
            self.storage.add_blob_referrer(b.hash, id)?;
        }

        match existing {
            Some(mut c) => {
                c.signed.body_bytes = event.body_bytes;
                c.body = body;
                c.refs = refs;
                c.blobs = blobs;
                self.storage.put_claim(c)?;
                report.bodies_attached += 1;
            }
            None => {
                // Dense per-log arrival: the count at insert time. Rows are
                // never deleted, so this is monotone and unique — and it
                // rolls back with the transaction.
                let mut arrival = 0u64;
                self.storage
                    .scan_log(&claim.header.log_id, &mut |_| arrival += 1)?;
                self.storage.put_claim(StoredClaim {
                    signed: SignedEvent {
                        header_bytes: event.header_bytes,
                        signature: event.signature,
                        body_bytes: if suppressed { None } else { event.body_bytes },
                    },
                    header: claim.header,
                    body,
                    refs,
                    blobs,
                    arrival,
                    received_at,
                })?;
                report.newly_stored = Some(id);
            }
        }
        Ok(())
    }

    /// Apply a redaction: record the monotone authority, drop the target's
    /// body if we hold it. Returns whether anything took effect (false on
    /// idempotent replays). Ties resolve to the smallest redactor so the
    /// outcome is arrival-order independent.
    fn apply_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<bool, Error> {
        let mut effective = false;
        match self.storage.redaction(&target)? {
            None => {
                self.storage.set_redaction(target, by)?;
                effective = true;
            }
            Some(prev) if by < prev => self.storage.set_redaction(target, by)?,
            Some(_) => {}
        }
        if let Some(mut c) = self.storage.get_claim(&target)?
            && c.body.is_some()
            // A redact claim's body is never dropped: it carries no user
            // content (just a hash pointer) and is the sole carrier of the
            // redaction it encodes. Dropping it would erase that fact from
            // the wire, un-redacting the original on any peer that restores
            // from a backup of tombstones.
            && redact_target(c.header.log_id, c.body.as_ref()).is_none()
        {
            effective = true;
            // Remove the outgoing edges BEFORE dropping the body. A crash in
            // between then leaves the body present, so redelivery re-enters
            // this branch and idempotently re-removes whatever edges remain.
            // (Dropping the body first would flip the `body.is_some()` guard
            // and strand the edges forever — a tombstone leaking what the
            // redacted body linked to.) A blob nobody references anymore
            // becomes GC-able.
            let refs = std::mem::take(&mut c.refs);
            let blobs = std::mem::take(&mut c.blobs);
            for (_path, r) in &refs {
                self.storage.remove_backlink(&r.hash, &target)?;
            }
            for (_path, b) in &blobs {
                self.storage.remove_blob_referrer(&b.hash, &target)?;
            }
            c.body = None;
            c.signed.body_bytes = None;
            self.storage.put_claim(c)?;
        }
        Ok(effective)
    }

    /// Number of known claims (with or without bodies).
    pub fn len(&self) -> usize {
        self.guard();
        self.storage.claim_count().expect(READ)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// True if we hold this claim's *content* (header and body).
    pub fn contains(&self, id: &ClaimHash) -> bool {
        self.guard();
        self.storage
            .get_claim(id)
            .expect(READ)
            .is_some_and(|c| c.body.is_some())
    }

    /// The claim, content or tombstone. `None` only if entirely unknown.
    pub fn get(&self, id: &ClaimHash) -> Option<StoredClaim> {
        self.guard();
        self.storage.get_claim(id).expect(READ)
    }

    /// If the claim was redacted, the claim that did it.
    pub fn redaction(&self, id: &ClaimHash) -> Option<ClaimHash> {
        self.guard();
        self.storage.redaction(id).expect(READ)
    }

    /// Every claim that links *to* `target`, ascending. Works for targets
    /// we haven't seen yet (dangling edges are real edges).
    pub fn backlinks(&self, target: &ClaimHash) -> Vec<ClaimHash> {
        self.guard();
        self.storage.backlinks(target).expect(READ)
    }

    /// Every claim whose live body references this blob. Empty means the
    /// blob is unreferenced (and a [`BlobStore`] may garbage-collect it).
    pub fn blob_referrers(&self, blob: &BlobHash) -> Vec<ClaimHash> {
        self.guard();
        self.storage.blob_referrers(blob).expect(READ)
    }

    /// The fetch want-list: every blob referenced by a live body that
    /// `blobs` doesn't hold, deduplicated, in canonical order. What a sync
    /// engine feeds its fetch queue; a want never expires, so a blob can
    /// heal from any pipe whenever it shows up.
    pub fn missing_blobs(&self, blobs: &BlobStore) -> Vec<BlobRef> {
        self.guard();
        let mut wanted: BTreeMap<BlobHash, BlobRef> = BTreeMap::new();
        self.storage
            .scan_claims(&mut |c| {
                for (_path, b) in &c.blobs {
                    if !blobs.contains(&b.hash) {
                        wanted.entry(b.hash).or_insert_with(|| b.clone());
                    }
                }
            })
            .expect(READ);
        wanted.into_values().collect()
    }

    /// All content-bearing claims from one log, in display order
    /// `(at, id)` — the body's claimed time when present, deterministic
    /// across stores, cosmetic by design.
    pub fn log(&self, log_id: &LogId) -> Vec<StoredClaim> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_log(log_id, &mut |c| {
                if c.body.is_some() {
                    out.push(c.clone());
                }
            })
            .expect(READ);
        out.sort_by_key(|c| (body_at(&c.body), c.signed.id()));
        out
    }

    /// Everything we hold for one log past a cursor — content claims *and*
    /// signed tombstones — in THIS store's arrival order. The cursor is a
    /// count: "I have `have` of your claims for this log; send the rest."
    /// It advances by the number of events returned, and is only
    /// meaningful against this store (arrival order is local). Tombstones
    /// ride along as headers without bodies, so a backfiller never
    /// downloads redacted content, and the markers are signed by
    /// construction.
    pub fn serve_since(&self, log_id: &LogId, have: u64) -> Vec<SignedEvent> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_log(log_id, &mut |c| {
                if c.arrival >= have {
                    out.push((c.arrival, c.signed.clone()));
                }
            })
            .expect(READ);
        out.sort_by_key(|(arrival, _)| *arrival);
        out.into_iter().map(|(_, e)| e).collect()
    }

    /// Every artifact we hold — content claims and signed tombstones, all
    /// logs — in canonical `(log, id)` order (convergent: two stores with
    /// the same state dump identical streams). This is the store
    /// serialized: replaying these frames through `ingest` reconstructs
    /// the convergent state exactly. Backup and the file transport are
    /// this one dump.
    pub fn events(&self) -> Vec<SignedEvent> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_claims(&mut |c| {
                out.push((c.header.log_id, c.signed.id(), c.signed.clone()));
            })
            .expect(READ);
        out.sort_by_key(|(log, id, _)| (*log, *id));
        out.into_iter().map(|(_, _, e)| e).collect()
    }

    /// How many claims we hold for one log (content and tombstones — rows
    /// are never deleted, so this never regresses). This is the number a
    /// cursor counts toward, and half of the sync handshake
    /// `(count, fingerprint)`.
    pub fn log_len(&self, log_id: &LogId) -> u64 {
        self.guard();
        let mut n = 0u64;
        self.storage.scan_log(log_id, &mut |_| n += 1).expect(READ);
        n
    }

    /// An order-independent digest of everything we hold for one log: which
    /// claims, whether we hold their bodies ("have" means "have the body"),
    /// and which redactions apply. Two honest stores agree on a log's
    /// fingerprint exactly when they agree on that log's convergent state.
    ///
    /// This is the sync layer's drift detector. Arrival cursors are the
    /// fast path, but they can't see *silent* divergence — a pipe that
    /// power-cycled and reused arrival positions (e.g. a relay restored
    /// from a stale backup) leaves both sides "caught up" at the same count
    /// while holding different sets. So a catch-up ends with a fingerprint
    /// exchange: match means done, mismatch means fall back to full set
    /// reconciliation. Like the cursor itself this is advisory — an XOR of
    /// per-claim digests detects drift between cooperating clients, it is
    /// not a defense against liars.
    ///
    /// Built by XOR-folding [`fingerprint_claim`] and
    /// [`fingerprint_redaction`] digests — and XOR makes it *homomorphic*:
    /// knowing a peer's fingerprint and the one claim they just ingested,
    /// you can compute their new fingerprint without asking (vouch-sync's
    /// push path lives on this).
    pub fn fingerprint(&self, log_id: &LogId) -> [u8; 32] {
        self.guard();
        let mut acc = [0u8; 32];
        self.storage
            .scan_log(log_id, &mut |c| {
                xor_into(
                    &mut acc,
                    fingerprint_claim(&c.signed.id(), c.body.is_some()),
                );
            })
            .expect(READ);
        let mut redactions = Vec::new();
        self.storage
            .scan_redactions(&mut |target, by| redactions.push((target, by)))
            .expect(READ);
        for (target, by) in redactions {
            let redactor_log = self
                .storage
                .get_claim(&by)
                .expect(READ)
                .map(|c| c.header.log_id);
            if redactor_log == Some(*log_id) {
                xor_into(&mut acc, fingerprint_redaction(&target, &by));
            }
        }
        acc
    }

    /// Everything we hold for one log as `(id, has_body)` pairs, ascending
    /// by id — the hash-list half of full set reconciliation. When
    /// fingerprints still disagree after a catch-up, two stores exchange
    /// these lists and diff them: ids the peer lacks are offered, ids we
    /// lack are fetched, and a body the peer holds for a claim we hold
    /// bodiless (and unredacted) is fetched too — "have" means "have the
    /// body", so this is also where stripped bodies heal. Redactions need
    /// no entry of their own: every redaction is carried by an ordinary
    /// redact claim, so syncing the claim diff converges the redaction
    /// sets as well.
    pub fn log_hashes(&self, log_id: &LogId) -> Vec<(ClaimHash, bool)> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_log(log_id, &mut |c| out.push((c.signed.id(), c.body.is_some())))
            .expect(READ);
        out.sort_by_key(|(id, _)| *id);
        out
    }

    /// The merged timeline across all logs (content-bearing claims only),
    /// sorted by `(at, log_id, id)` — the bodies' claimed times when
    /// present — deterministic across clients, cosmetic by design.
    pub fn timeline(&self) -> Vec<StoredClaim> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_claims(&mut |c| {
                if c.body.is_some() {
                    out.push(c.clone());
                }
            })
            .expect(READ);
        out.sort_by_key(|c| (body_at(&c.body), c.header.log_id, c.signed.id()));
        out
    }

    /// A canonical snapshot of the store's state.
    pub fn state_vector(&self) -> StateVector {
        self.guard();
        let mut claims = BTreeMap::new();
        self.storage
            .scan_claims(&mut |c| {
                claims.insert(
                    c.signed.id(),
                    (c.signed.header_bytes.clone(), c.signed.body_bytes.clone()),
                );
            })
            .expect(READ);
        let mut redactions = BTreeMap::new();
        self.storage
            .scan_redactions(&mut |target, by| {
                redactions.insert(target, by);
            })
            .expect(READ);
        StateVector { claims, redactions }
    }

    /// Convenience: content-bearing claims whose body has
    /// `"type": <type_name>` at the top level. Vocabulary-level queries live
    /// in higher layers; this exists so tests and prototypes can speak the
    /// starter vocabulary.
    pub fn by_type(&self, type_name: &str) -> Vec<StoredClaim> {
        self.guard();
        let mut out = Vec::new();
        self.storage
            .scan_claims(&mut |c| {
                let is_match = matches!(
                    &c.body,
                    Some(Value::Map(m)) if matches!(m.get("type"), Some(Value::Text(t)) if t == type_name)
                );
                if is_match {
                    out.push(c.clone());
                }
            })
            .expect(READ);
        out
    }

    /// Discard every claim received before `cutoff` (Unix ms) — a bounded
    /// *retention* policy, not the redaction mechanism. This is not, and
    /// can never be, cursor-driven: a cursor only knows about peers this
    /// store has already talked to, never about who might follow a log a
    /// year from now expecting its full history. There is no claim count
    /// or cursor position that proves "safe to delete" in that world.
    ///
    /// What makes this safe is the opposite move: stop promising
    /// permanence. A relay that holds a bounded window and says so plainly
    /// (this call) is a sound, honest service; a relay that quietly hopes
    /// cursors will someday justify a delete is not. Permanence is a
    /// property of *peers* — an always-on device that never calls this —
    /// not of a relay.
    pub fn purge_older_than(&mut self, cutoff: i64) -> Result<Vec<ClaimHash>, Error> {
        self.guard();
        self.storage.begin()?;
        self.poisoned = true;
        match self.storage.purge_older_than(cutoff) {
            Ok(purged) => match self.storage.commit() {
                Ok(()) => {
                    self.poisoned = false;
                    Ok(purged)
                }
                Err(e) => {
                    if self.storage.rollback().is_ok() {
                        self.poisoned = false;
                    }
                    Err(e)
                }
            },
            Err(e) => {
                if self.storage.rollback().is_ok() {
                    self.poisoned = false;
                }
                Err(e)
            }
        }
    }
}

impl ClaimStore {
    /// The fsck: cross-check the whole store against its own invariants.
    /// Returns human-readable violations; empty means healthy.
    ///
    /// Every stored artifact re-verifies (signature, body hash — rows are
    /// self-authenticating, so a backend cannot lie about *content*, only
    /// lose it); redacted claims must be bodiless; the index edges must
    /// agree with the bodies in both directions. Run it after chaos tests,
    /// or as a paranoia pass at app startup.
    ///
    /// One caveat by design: a store that crashed mid-ingest *without*
    /// transactions may transiently hold phantom index rows until the
    /// event is redelivered — fsck is a check for quiescent stores, and
    /// "violations now" plus "redelivery" must end in "no violations".
    pub fn verify_integrity(&self) -> Vec<String> {
        self.guard();
        let mut problems = Vec::new();
        let mut claims: Vec<StoredClaim> = Vec::new();
        self.storage
            .scan_claims(&mut |c| claims.push(c.clone()))
            .expect(READ);

        // Pass 1: every claim row is self-consistent and forward-indexed.
        for c in &claims {
            let id = c.signed.id();
            if let Err(e) = c.signed.verify() {
                problems.push(format!("claim {id:?} fails verification: {e}"));
                continue;
            }
            if c.body.is_some() != c.signed.body_bytes.is_some() {
                problems.push(format!(
                    "claim {id:?}: decoded body and body bytes disagree"
                ));
            }
            if c.body.is_some() && self.storage.redaction(&id).expect(READ).is_some() {
                problems.push(format!("claim {id:?} is redacted but still has a body"));
            }
            let Edges { refs, blobs, .. } = match &c.body {
                Some(b) => b.collect_edges(),
                None => Edges::default(),
            };
            if refs != c.refs {
                problems.push(format!("claim {id:?}: stored refs disagree with body"));
            }
            if blobs != c.blobs {
                problems.push(format!("claim {id:?}: stored blob refs disagree with body"));
            }
            for (_path, r) in &c.refs {
                if !self.storage.backlinks(&r.hash).expect(READ).contains(&id) {
                    problems.push(format!(
                        "claim {id:?}: ref to {:?} missing from backlink index",
                        r.hash
                    ));
                }
            }
            for (_path, b) in &c.blobs {
                if !self
                    .storage
                    .blob_referrers(&b.hash)
                    .expect(READ)
                    .contains(&id)
                {
                    problems.push(format!(
                        "claim {id:?}: blob {:?} missing from referrer index",
                        b.hash
                    ));
                }
            }
        }

        // Arrival positions must be unique per log, or cursors would
        // skip or double-serve.
        let mut seen_arrivals = std::collections::HashSet::new();
        for c in &claims {
            if !seen_arrivals.insert((c.header.log_id, c.arrival)) {
                problems.push(format!(
                    "duplicate arrival {} in log {:?}",
                    c.arrival, c.header.log_id
                ));
            }
        }

        // Pass 2: every index row points back to a live edge. (Dangling
        // TARGETS are fine — edges to claims we haven't seen are real
        // edges; phantom SOURCES are not.)
        let by_id: HashMap<ClaimHash, &StoredClaim> =
            claims.iter().map(|c| (c.signed.id(), c)).collect();
        let mut backlink_rows = Vec::new();
        self.storage
            .scan_backlinks(&mut |t, s| backlink_rows.push((t, s)))
            .expect(READ);
        for (target, source) in backlink_rows {
            match by_id.get(&source) {
                Some(c) if c.refs.iter().any(|(_, r)| r.hash == target) => {}
                _ => problems.push(format!("phantom backlink {target:?} <- {source:?}")),
            }
        }
        let mut referrer_rows = Vec::new();
        self.storage
            .scan_blob_referrers(&mut |b, s| referrer_rows.push((b, s)))
            .expect(READ);
        for (blob, source) in referrer_rows {
            match by_id.get(&source) {
                Some(c) if c.blobs.iter().any(|(_, b)| b.hash == blob) => {}
                _ => problems.push(format!("phantom blob referrer {blob:?} <- {source:?}")),
            }
        }

        // Pass 3: every redaction entry is backed by a real redact claim
        // that targets it. A redact claim's body is never dropped, so the
        // redactor is always present with its body — a dangling or
        // fabricated entry would censor a claim with no authority behind it.
        let mut redaction_rows = Vec::new();
        self.storage
            .scan_redactions(&mut |t, by| redaction_rows.push((t, by)))
            .expect(READ);
        for (target, by) in redaction_rows {
            let backed = by_id
                .get(&by)
                .is_some_and(|c| redact_target(c.header.log_id, c.body.as_ref()) == Some(target));
            if !backed {
                problems.push(format!(
                    "redaction {target:?} <- {by:?} not backed by a valid redact claim"
                ));
            }
        }
        problems
    }
}

/// Engine-recognized display time: an optional top-level `at` body key
/// (Unix milliseconds), read leniently like `type` and `redacts`. The
/// author's claimed time — transitively signed via the body hash, and
/// redacted along with the rest of the body.
fn body_at(body: &Option<Value>) -> Option<i64> {
    let Some(Value::Map(m)) = body else {
        return None;
    };
    match m.get("at") {
        Some(Value::Int(t)) => Some(*t),
        _ => None,
    }
}

/// Engine-recognized redaction: `{type: "redact", redacts: ClaimRef}` where
/// the target is in the author's *own* log. Anyone else's "redact" is mere
/// speech, stored like any claim but with no engine effect.
/// XOR one digest into a fingerprint accumulator.
fn xor_into(acc: &mut [u8; 32], digest: [u8; 32]) {
    for (a, b) in acc.iter_mut().zip(&digest) {
        *a ^= b;
    }
}

/// One claim's contribution to a log [`fingerprint`](ClaimStore::fingerprint):
/// BLAKE3 over `("claim", id, has-body bit)`. Public because the fingerprint
/// is an XOR fold of these, so sync layers can update a remembered peer
/// fingerprint claim-by-claim instead of re-asking for it.
pub fn fingerprint_claim(id: &ClaimHash, has_body: bool) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"claim");
    h.update(&id.0);
    h.update(&[has_body as u8]);
    *h.finalize().as_bytes()
}

/// One redaction's contribution to its redactor-log
/// [`fingerprint`](ClaimStore::fingerprint): BLAKE3 over
/// `("redaction", target, redacting claim)`. See [`fingerprint_claim`].
pub fn fingerprint_redaction(target: &ClaimHash, by: &ClaimHash) -> [u8; 32] {
    let mut h = blake3::Hasher::new();
    h.update(b"redaction");
    h.update(&target.0);
    h.update(&by.0);
    *h.finalize().as_bytes()
}

/// What this body redacts, if it is a well-formed redact claim by `author`:
/// `{ type: "redact", redacts: ClaimRef }` pointing into the author's own
/// log (redaction is own-log-only; anything else is an ordinary claim that
/// happens to mention a redaction). This is the engine's recognizer —
/// public so sync layers judging a redact claim's effect apply exactly the
/// rule ingest applies.
pub fn redact_target(author: LogId, body: Option<&Value>) -> Option<ClaimHash> {
    let Some(Value::Map(m)) = body else {
        return None;
    };
    if !matches!(m.get("type"), Some(Value::Text(t)) if t == "redact") {
        return None;
    }
    let Some(Value::ClaimRef(target)) = m.get("redacts") else {
        return None;
    };
    (target.log_id == author).then_some(target.hash)
}
