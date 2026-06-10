//! An in-memory, order-insensitive claim store with generic link indexing.
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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::blob::BlobStore;
use crate::claim::{EventHeader, SignedEvent};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::{BlobHash, BlobRef, ClaimHash, ClaimRef, Path, Value};

/// How deep embedded claims may nest inside one another.
///
/// Each level of a re-vouch chain nests the previous event, so this bounds
/// the longest endorsement chain we'll verify in one artifact. ~33 bits
/// suffice to individually identify every human, so even a maximally viral
/// chain of re-vouches stays well under 64; the cap is pure headroom over
/// reality while still bounding adversarial verification work (one
/// signature check per level).
const MAX_EMBED_DEPTH: usize = 64;

/// How a claim got here. Order-insensitive: seeing a claim directly ever
/// means `Direct`, no matter what arrived first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Provenance {
    /// Received as a top-level event (from the author's log).
    Direct,
    /// Only ever seen embedded inside someone else's claim.
    Embedded,
}

/// A claim at rest: original artifact, decoded views, extracted link
/// metadata. `body: None` means tombstone (if redacted) or body-not-yet-seen
/// (check [`ClaimStore::redaction`] to tell which).
#[derive(Debug, Clone)]
pub struct StoredClaim {
    pub signed: SignedEvent,
    pub header: EventHeader,
    pub body: Option<Value>,
    /// Every `ClaimRef` in the body, with the path where it was found.
    pub refs: Vec<(Path, ClaimRef)>,
    /// Every `BlobRef` in the body, with the path where it was found.
    pub blobs: Vec<(Path, BlobRef)>,
    pub provenance: Provenance,
}

/// What one `ingest` call did.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct IngestReport {
    /// Claims newly added (the event itself and/or claims embedded in it).
    pub newly_stored: Vec<ClaimHash>,
    /// Events that were already fully present.
    pub duplicates: usize,
    /// Embedded events that failed verification and were skipped. The
    /// embedding claim is stored regardless (its author signed it; the
    /// garbage inside is their problem, recorded not replicated).
    pub rejected_embeds: usize,
    /// Bodies suppressed because the claim is redacted.
    pub redacted_skips: usize,
    /// Bodies attached to claims previously known only by header.
    pub bodies_attached: usize,
}

/// One claim's contribution to a [`StateVector`]: header bytes and body
/// bytes if held.
pub type ClaimSnapshot = (Vec<u8>, Option<Vec<u8>>);

/// A canonical snapshot of the store's *convergent* state: exactly the
/// replicated substance — headers, bodies, redactions — and nothing local.
///
/// Two kinds of metadata are deliberately excluded, because sync exchanges
/// claims by id and would never propagate them: which valid signature we
/// hold (an author can produce many; any one of them is equal proof of
/// authorship), and provenance (how *we* came to know a claim). Including
/// either would let two fully-synced stores compare unequal forever.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateVector {
    pub claims: BTreeMap<ClaimHash, ClaimSnapshot>,
    /// target → redacting claim (the authority making bodilessness monotone)
    pub redactions: BTreeMap<ClaimHash, ClaimHash>,
}

/// The in-memory claim store.
#[derive(Default)]
pub struct ClaimStore {
    claims: HashMap<ClaimHash, StoredClaim>,
    backlinks: HashMap<ClaimHash, BTreeSet<ClaimHash>>,
    /// blob → claims whose live bodies reference it. Drives the fetch
    /// want-list and blob GC; entries die with the referencing bodies.
    blob_referrers: HashMap<BlobHash, BTreeSet<ClaimHash>>,
    /// target → redacting claim id. Monotone; ties resolve to the smallest
    /// redactor so the outcome is arrival-order independent.
    redactions: HashMap<ClaimHash, ClaimHash>,
}

impl ClaimStore {
    pub fn new() -> ClaimStore {
        ClaimStore::default()
    }

    /// Verify and store one signed event, recursively ingesting anything it
    /// embeds and applying any redaction its body carries. Order-insensitive
    /// and idempotent.
    ///
    /// Every error this returns occurs before the store is mutated — there
    /// is nothing to roll back, by construction. Embed problems and
    /// redaction skips are counted in the report, never fatal.
    pub fn ingest(&mut self, event: SignedEvent) -> Result<IngestReport, Error> {
        let mut report = IngestReport::default();
        self.ingest_inner(event, Provenance::Direct, 0, &mut report)?;
        Ok(report)
    }

    fn ingest_inner(
        &mut self,
        event: SignedEvent,
        provenance: Provenance,
        depth: usize,
        report: &mut IngestReport,
    ) -> Result<(), Error> {
        if depth > MAX_EMBED_DEPTH {
            return Err(Error::EmbedTooDeep);
        }
        let claim = event.verify()?;
        let id = event.id();

        // A redaction takes effect whenever a verified redact body is SEEN —
        // even if this event's own body won't be stored. (A claim can never
        // redact itself: its body would have to contain its own hash, which
        // is a hash cycle.)
        if let Some(target) = redact_target(claim.header.log_id, claim.body.as_ref()) {
            self.apply_redaction(target, id);
        }

        let suppressed = self.redactions.contains_key(&id);
        let body = if suppressed { None } else { claim.body };
        if suppressed && event.body_bytes.is_some() {
            report.redacted_skips += 1;
        }

        let known = self.claims.contains_key(&id);
        if known {
            let existing = self.claims.get_mut(&id).expect("checked above");
            if provenance < existing.provenance {
                existing.provenance = provenance;
            }
            // The first valid signature we saw stays. An author can mint
            // many valid signatures for one header; all are equal proof of
            // authorship, so which one we hold is local metadata, not state
            // (and is excluded from StateVector accordingly).
            if existing.body.is_some() || body.is_none() {
                report.duplicates += 1;
                return Ok(());
            }
            // Fall through: we hold a header-only claim and now have a
            // verified body for it.
        }

        let (refs, embeds, blobs) = match &body {
            Some(b) => b.collect_refs(),
            None => Default::default(),
        };

        // Ingest embedded claims first (any order would converge; this just
        // means a vouch's original is queryable by the time the vouch is).
        // An embed cannot redact its own container (hash cycle), so `body`
        // staying attached below is order-independent.
        for (_path, embedded) in embeds {
            if self
                .ingest_inner(embedded, Provenance::Embedded, depth + 1, report)
                .is_err()
            {
                report.rejected_embeds += 1;
            }
        }

        for (_path, target) in &refs {
            self.backlinks.entry(target.hash).or_default().insert(id);
        }
        for (_path, b) in &blobs {
            self.blob_referrers.entry(b.hash).or_default().insert(id);
        }

        if known {
            let existing = self.claims.get_mut(&id).expect("checked above");
            existing.signed.body_bytes = event.body_bytes;
            existing.body = body;
            existing.refs = refs;
            existing.blobs = blobs;
            report.bodies_attached += 1;
        } else {
            self.claims.insert(
                id,
                StoredClaim {
                    signed: SignedEvent {
                        header_bytes: event.header_bytes,
                        signature: event.signature,
                        body_bytes: if suppressed { None } else { event.body_bytes },
                    },
                    header: claim.header,
                    body,
                    refs,
                    blobs,
                    provenance,
                },
            );
            report.newly_stored.push(id);
        }
        Ok(())
    }

    /// Apply a redaction: record the monotone authority, drop the target's
    /// body if we hold it.
    fn apply_redaction(&mut self, target: ClaimHash, by: ClaimHash) {
        let entry = self.redactions.entry(target).or_insert(by);
        if by < *entry {
            *entry = by;
        }
        if let Some(c) = self.claims.get_mut(&target)
            && c.body.is_some()
        {
            c.body = None;
            c.signed.body_bytes = None;
            // The redacted claim's outgoing links and blob wants die with
            // its body (a blob nobody references anymore becomes GC-able).
            let refs = std::mem::take(&mut c.refs);
            let blobs = std::mem::take(&mut c.blobs);
            for (_path, r) in refs {
                if let Some(sources) = self.backlinks.get_mut(&r.hash) {
                    sources.remove(&target);
                }
            }
            for (_path, b) in blobs {
                if let Some(sources) = self.blob_referrers.get_mut(&b.hash) {
                    sources.remove(&target);
                    if sources.is_empty() {
                        self.blob_referrers.remove(&b.hash);
                    }
                }
            }
        }
    }

    /// Number of known claims (with or without bodies).
    pub fn len(&self) -> usize {
        self.claims.len()
    }

    pub fn is_empty(&self) -> bool {
        self.claims.is_empty()
    }

    /// True if we hold this claim's *content* (header and body).
    pub fn contains(&self, id: &ClaimHash) -> bool {
        matches!(self.claims.get(id), Some(c) if c.body.is_some())
    }

    /// The claim, content or tombstone. `None` only if entirely unknown.
    pub fn get(&self, id: &ClaimHash) -> Option<&StoredClaim> {
        self.claims.get(id)
    }

    /// If the claim was redacted, the claim that did it.
    pub fn redaction(&self, id: &ClaimHash) -> Option<ClaimHash> {
        self.redactions.get(id).copied()
    }

    /// Every claim that links *to* `target`, in canonical order. Works for
    /// targets we haven't seen yet (dangling edges are real edges).
    pub fn backlinks(&self, target: &ClaimHash) -> impl Iterator<Item = &ClaimHash> {
        self.backlinks.get(target).into_iter().flatten()
    }

    /// Every claim whose live body references this blob. Empty means the
    /// blob is unreferenced (and a [`BlobStore`] may garbage-collect it).
    pub fn blob_referrers(&self, blob: &BlobHash) -> impl Iterator<Item = &ClaimHash> {
        self.blob_referrers.get(blob).into_iter().flatten()
    }

    /// The fetch want-list: every blob referenced by a live body that
    /// `blobs` doesn't hold, deduplicated, in canonical order. What a sync
    /// engine feeds its fetch queue; a want never expires, so a blob can
    /// heal from any pipe whenever it shows up.
    pub fn missing_blobs(&self, blobs: &BlobStore) -> Vec<BlobRef> {
        let mut wanted: BTreeMap<BlobHash, BlobRef> = BTreeMap::new();
        for c in self.claims.values() {
            for (_path, b) in &c.blobs {
                if !blobs.contains(&b.hash) {
                    wanted.entry(b.hash).or_insert_with(|| b.clone());
                }
            }
        }
        wanted.into_values().collect()
    }

    /// All content-bearing claims from one log, ordered by the
    /// advisory `(sequence, timestamp, id)`.
    pub fn log(&self, log_id: &LogId) -> Vec<&StoredClaim> {
        let mut out: Vec<&StoredClaim> = self
            .claims
            .values()
            .filter(|c| c.header.log_id == *log_id && c.body.is_some())
            .collect();
        out.sort_by_key(|c| (c.header.sequence, c.header.timestamp, c.signed.id()));
        out
    }

    /// Everything we hold for one log — content claims *and* signed
    /// tombstones — ordered by advisory sequence. This is what a peer
    /// serves for a backfill: tombstones ride along as headers without
    /// bodies, so a backfiller never downloads redacted content, and the
    /// markers are signed by construction.
    pub fn serve_since(&self, log_id: &LogId, since: u64) -> Vec<&SignedEvent> {
        let mut out: Vec<&StoredClaim> = self
            .claims
            .values()
            .filter(|c| c.header.log_id == *log_id && c.header.sequence > since)
            .collect();
        out.sort_by_key(|c| (c.header.sequence, c.signed.id()));
        out.into_iter().map(|c| &c.signed).collect()
    }

    /// The highest advisory sequence seen per log (a sync hint between
    /// cooperating clients, nothing more). Tombstones count, so redaction
    /// never regresses it.
    pub fn max_sequence(&self, log_id: &LogId) -> Option<u64> {
        self.claims
            .values()
            .filter(|c| c.header.log_id == *log_id)
            .map(|c| c.header.sequence)
            .max()
    }

    /// An order-independent digest of everything we hold for one log: which
    /// claims, whether we hold their bodies ("have" means "have the body"),
    /// and which redactions apply. Two honest stores agree on a log's
    /// fingerprint exactly when they agree on that log's convergent state.
    ///
    /// This is the sync layer's drift detector. Sequence cursors are the
    /// fast path, but they can't see *silent* divergence — a writer that
    /// power-cycled and reused numbers leaves both sides "caught up" at the
    /// same cursor while holding different sets. So a catch-up ends with a
    /// fingerprint exchange: match means done, mismatch means fall back to
    /// full set reconciliation. Like the sequence itself this is advisory —
    /// an XOR of per-claim digests detects drift between cooperating
    /// clients, it is not a defense against liars.
    pub fn fingerprint(&self, log_id: &LogId) -> [u8; 32] {
        let mut acc = [0u8; 32];
        let mut mix = |hash: blake3::Hash| {
            for (a, b) in acc.iter_mut().zip(hash.as_bytes()) {
                *a ^= b;
            }
        };
        for (id, c) in &self.claims {
            if c.header.log_id != *log_id {
                continue;
            }
            let mut h = blake3::Hasher::new();
            h.update(b"claim");
            h.update(&id.0);
            h.update(&[c.body.is_some() as u8]);
            mix(h.finalize());
        }
        for (target, by) in &self.redactions {
            let redactor_log = self.claims.get(by).map(|c| c.header.log_id);
            if redactor_log == Some(*log_id) {
                let mut h = blake3::Hasher::new();
                h.update(b"redaction");
                h.update(&target.0);
                h.update(&by.0);
                mix(h.finalize());
            }
        }
        acc
    }

    /// The merged timeline across all logs (content-bearing claims only),
    /// sorted by `(timestamp, log_id, sequence, id)` — deterministic
    /// across clients, cosmetic by design.
    pub fn timeline(&self) -> Vec<&StoredClaim> {
        let mut out: Vec<&StoredClaim> =
            self.claims.values().filter(|c| c.body.is_some()).collect();
        out.sort_by_key(|c| {
            (
                c.header.timestamp,
                c.header.log_id,
                c.header.sequence,
                c.signed.id(),
            )
        });
        out
    }

    /// A canonical snapshot of the store's state.
    pub fn state_vector(&self) -> StateVector {
        StateVector {
            claims: self
                .claims
                .iter()
                .map(|(id, c)| {
                    (
                        *id,
                        (c.signed.header_bytes.clone(), c.signed.body_bytes.clone()),
                    )
                })
                .collect(),
            redactions: self.redactions.iter().map(|(k, v)| (*k, *v)).collect(),
        }
    }

    /// Convenience: content-bearing claims whose body has
    /// `"type": <type_name>` at the top level. Vocabulary-level queries live
    /// in higher layers; this exists so tests and prototypes can speak the
    /// starter vocabulary.
    pub fn by_type<'a>(&'a self, type_name: &'a str) -> impl Iterator<Item = &'a StoredClaim> {
        self.claims.values().filter(move |c| {
            matches!(
                &c.body,
                Some(Value::Map(m)) if matches!(m.get("type"), Some(Value::Text(t)) if t == type_name)
            )
        })
    }
}

/// Engine-recognized redaction: `{type: "redact", redacts: ClaimRef}` where
/// the target is in the author's *own* log. Anyone else's "redact" is mere
/// speech, stored like any claim but with no engine effect.
fn redact_target(author: LogId, body: Option<&Value>) -> Option<ClaimHash> {
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
