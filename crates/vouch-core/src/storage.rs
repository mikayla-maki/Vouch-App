//! The claim storage seam: dumb rows and indexes under the invariants.
//!
//! [`ClaimStorage`] is deliberately *below* [`ClaimStore`](crate::ClaimStore):
//! backends store and retrieve rows; they never interpret them. All
//! convergence logic — monotone redaction, seen-is-applied, body fill-in,
//! what counts as state — lives in `ClaimStore`, written and tested exactly
//! once, driving whichever backend it's given.
//!
//! Two real consumers define this trait's shape:
//!
//! - [`MemoryClaimStorage`] — tests and simulations: vouch-core stays
//!   I/O-free, sync sessions run as plain unit tests.
//! - SQLite (in vouch-store) — the app, where mobile is the primary
//!   target: durable, transactional, lazy, and redaction's body-drop is a
//!   plain column update so cooperative deletion reaches the disk.
//!
//! Contract notes for implementors:
//! - `put_claim` is an upsert keyed by the claim's id (`signed.id()`).
//! - List-returning methods return ascending order.
//! - Scans visit in unspecified order; callers sort.

use std::collections::{BTreeSet, HashMap};

use crate::error::Error;
use crate::keys::LogId;
use crate::store::StoredClaim;
use crate::value::{BlobHash, ClaimHash};

/// Storage primitive for claims and their indexes. See the module docs for
/// the contract; see [`ClaimStore`](crate::ClaimStore) for the logic that
/// drives it.
pub trait ClaimStorage: Send {
    // ── claims ──────────────────────────────────────────────────────────
    fn get_claim(&self, id: &ClaimHash) -> Result<Option<StoredClaim>, Error>;
    /// Upsert by `claim.signed.id()`.
    fn put_claim(&mut self, claim: StoredClaim) -> Result<(), Error>;
    fn claim_count(&self) -> Result<usize, Error>;
    /// Visit every claim, unspecified order.
    fn scan_claims(&self, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error>;
    /// Visit every claim of one log, unspecified order (backends index this).
    fn scan_log(&self, log: &LogId, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error>;

    // ── backlink index ──────────────────────────────────────────────────
    fn add_backlink(&mut self, target: ClaimHash, source: ClaimHash) -> Result<(), Error>;
    fn remove_backlink(&mut self, target: &ClaimHash, source: &ClaimHash) -> Result<(), Error>;
    /// Sources linking to `target`, ascending.
    fn backlinks(&self, target: &ClaimHash) -> Result<Vec<ClaimHash>, Error>;

    // ── blob referrer index ─────────────────────────────────────────────
    fn add_blob_referrer(&mut self, blob: BlobHash, source: ClaimHash) -> Result<(), Error>;
    fn remove_blob_referrer(&mut self, blob: &BlobHash, source: &ClaimHash) -> Result<(), Error>;
    /// Claims whose live bodies reference `blob`, ascending.
    fn blob_referrers(&self, blob: &BlobHash) -> Result<Vec<ClaimHash>, Error>;

    // ── redactions ──────────────────────────────────────────────────────
    fn redaction(&self, target: &ClaimHash) -> Result<Option<ClaimHash>, Error>;
    /// Upsert: record `by` as the redactor of `target`.
    fn set_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<(), Error>;
    fn scan_redactions(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error>;

    // ── retention ────────────────────────────────────────────────────────
    /// Discard every claim whose `received_at` is strictly before `cutoff`,
    /// along with its own outgoing backlink/blob-referrer edges and any
    /// redaction row naming it as the target. Returns what was purged.
    ///
    /// This is a hard delete (header and signature too, unlike redaction's
    /// tombstone), and it does not scrub edges where a *surviving* claim
    /// still points at the purged one — a dangling reference to content
    /// you no longer hold, same as any other peer that never received it.
    fn purge_older_than(&mut self, cutoff: i64) -> Result<Vec<ClaimHash>, Error>;

    // ── integrity scans (for fsck; see ClaimStore::verify_integrity) ────
    /// Visit every backlink row as `(target, source)`.
    fn scan_backlinks(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error>;
    /// Visit every blob-referrer row as `(blob, source)`.
    fn scan_blob_referrers(&self, visit: &mut dyn FnMut(BlobHash, ClaimHash)) -> Result<(), Error>;

    // ── transactions (required: atomicity is the backend's job) ─────────
    //
    // The engine brackets every ingest with begin … commit/rollback;
    // backends implement these natively (SQLite's journal, the memory
    // backend's undo log). They are REQUIRED, not defaulted: a backend
    // that genuinely cannot offer atomicity must write the no-ops itself —
    // a visible decision, never a silent omission. The engine still
    // converges over such a backend (write ordering makes `put_claim` the
    // commit point, and redelivery completes any partial ingest), but
    // "partial state can persist across a crash" should be a sentence
    // someone consciously wrote.
    fn begin(&mut self) -> Result<(), Error>;
    fn commit(&mut self) -> Result<(), Error>;
    fn rollback(&mut self) -> Result<(), Error>;
}

/// The in-memory backend: tests, simulations, ephemeral stores.
///
/// Fully transactional, by the book: mutations inside an open transaction
/// push undo closures; commit discards them, rollback replays them in
/// reverse. So even the test backend gives `ingest` real atomicity — the
/// fault-injection suite has to *deliberately strip* transactions to
/// exercise the no-transaction story.
#[derive(Default)]
pub struct MemoryClaimStorage {
    state: State,
    /// `Some` while a transaction is open: the undo log.
    undo: Option<Vec<Undo>>,
}

#[derive(Default)]
struct State {
    claims: HashMap<ClaimHash, StoredClaim>,
    backlinks: HashMap<ClaimHash, BTreeSet<ClaimHash>>,
    blob_referrers: HashMap<BlobHash, BTreeSet<ClaimHash>>,
    redactions: HashMap<ClaimHash, ClaimHash>,
}

type Undo = Box<dyn FnOnce(&mut State) + Send>;

fn add_edge<K: std::hash::Hash + Eq + Copy>(
    map: &mut HashMap<K, BTreeSet<ClaimHash>>,
    key: K,
    value: ClaimHash,
) -> bool {
    map.entry(key).or_default().insert(value)
}

fn remove_edge<K: std::hash::Hash + Eq + Copy>(
    map: &mut HashMap<K, BTreeSet<ClaimHash>>,
    key: &K,
    value: &ClaimHash,
) -> bool {
    let Some(set) = map.get_mut(key) else {
        return false;
    };
    let removed = set.remove(value);
    if set.is_empty() {
        map.remove(key);
    }
    removed
}

impl MemoryClaimStorage {
    pub fn new() -> MemoryClaimStorage {
        MemoryClaimStorage::default()
    }

    fn record(&mut self, undo: Undo) {
        if let Some(log) = &mut self.undo {
            log.push(undo);
        }
    }
}

impl ClaimStorage for MemoryClaimStorage {
    fn get_claim(&self, id: &ClaimHash) -> Result<Option<StoredClaim>, Error> {
        Ok(self.state.claims.get(id).cloned())
    }

    fn put_claim(&mut self, claim: StoredClaim) -> Result<(), Error> {
        let id = claim.signed.id();
        if self.undo.is_some() {
            let prior = self.state.claims.get(&id).cloned();
            self.record(Box::new(move |s| {
                match prior {
                    Some(p) => s.claims.insert(id, p),
                    None => s.claims.remove(&id),
                };
            }));
        }
        self.state.claims.insert(id, claim);
        Ok(())
    }

    fn claim_count(&self) -> Result<usize, Error> {
        Ok(self.state.claims.len())
    }

    fn scan_claims(&self, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        for c in self.state.claims.values() {
            visit(c);
        }
        Ok(())
    }

    fn scan_log(&self, log: &LogId, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        for c in self
            .state
            .claims
            .values()
            .filter(|c| c.header.log_id == *log)
        {
            visit(c);
        }
        Ok(())
    }

    fn add_backlink(&mut self, target: ClaimHash, source: ClaimHash) -> Result<(), Error> {
        if add_edge(&mut self.state.backlinks, target, source) {
            self.record(Box::new(move |s| {
                remove_edge(&mut s.backlinks, &target, &source);
            }));
        }
        Ok(())
    }

    fn remove_backlink(&mut self, target: &ClaimHash, source: &ClaimHash) -> Result<(), Error> {
        let (target, source) = (*target, *source);
        if remove_edge(&mut self.state.backlinks, &target, &source) {
            self.record(Box::new(move |s| {
                add_edge(&mut s.backlinks, target, source);
            }));
        }
        Ok(())
    }

    fn backlinks(&self, target: &ClaimHash) -> Result<Vec<ClaimHash>, Error> {
        Ok(self
            .state
            .backlinks
            .get(target)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default())
    }

    fn add_blob_referrer(&mut self, blob: BlobHash, source: ClaimHash) -> Result<(), Error> {
        if add_edge(&mut self.state.blob_referrers, blob, source) {
            self.record(Box::new(move |s| {
                remove_edge(&mut s.blob_referrers, &blob, &source);
            }));
        }
        Ok(())
    }

    fn remove_blob_referrer(&mut self, blob: &BlobHash, source: &ClaimHash) -> Result<(), Error> {
        let (blob, source) = (*blob, *source);
        if remove_edge(&mut self.state.blob_referrers, &blob, &source) {
            self.record(Box::new(move |s| {
                add_edge(&mut s.blob_referrers, blob, source);
            }));
        }
        Ok(())
    }

    fn blob_referrers(&self, blob: &BlobHash) -> Result<Vec<ClaimHash>, Error> {
        Ok(self
            .state
            .blob_referrers
            .get(blob)
            .map(|s| s.iter().copied().collect())
            .unwrap_or_default())
    }

    fn redaction(&self, target: &ClaimHash) -> Result<Option<ClaimHash>, Error> {
        Ok(self.state.redactions.get(target).copied())
    }

    fn set_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<(), Error> {
        if self.undo.is_some() {
            let prior = self.state.redactions.get(&target).copied();
            self.record(Box::new(move |s| {
                match prior {
                    Some(p) => s.redactions.insert(target, p),
                    None => s.redactions.remove(&target),
                };
            }));
        }
        self.state.redactions.insert(target, by);
        Ok(())
    }

    fn scan_redactions(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        for (t, by) in &self.state.redactions {
            visit(*t, *by);
        }
        Ok(())
    }

    fn scan_backlinks(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        for (target, sources) in &self.state.backlinks {
            for source in sources {
                visit(*target, *source);
            }
        }
        Ok(())
    }

    fn scan_blob_referrers(&self, visit: &mut dyn FnMut(BlobHash, ClaimHash)) -> Result<(), Error> {
        for (blob, sources) in &self.state.blob_referrers {
            for source in sources {
                visit(*blob, *source);
            }
        }
        Ok(())
    }

    fn purge_older_than(&mut self, cutoff: i64) -> Result<Vec<ClaimHash>, Error> {
        let purge_ids: Vec<ClaimHash> = self
            .state
            .claims
            .values()
            .filter(|c| c.received_at < cutoff)
            .map(|c| c.signed.id())
            .collect();

        for &id in &purge_ids {
            if let Some(prior) = self.state.claims.remove(&id)
                && self.undo.is_some()
            {
                self.record(Box::new(move |s| {
                    s.claims.insert(id, prior);
                }));
            }

            let targets: Vec<ClaimHash> = self
                .state
                .backlinks
                .iter()
                .filter(|(_, sources)| sources.contains(&id))
                .map(|(target, _)| *target)
                .collect();
            for target in targets {
                if remove_edge(&mut self.state.backlinks, &target, &id) && self.undo.is_some() {
                    self.record(Box::new(move |s| {
                        add_edge(&mut s.backlinks, target, id);
                    }));
                }
            }

            let blobs: Vec<BlobHash> = self
                .state
                .blob_referrers
                .iter()
                .filter(|(_, sources)| sources.contains(&id))
                .map(|(blob, _)| *blob)
                .collect();
            for blob in blobs {
                if remove_edge(&mut self.state.blob_referrers, &blob, &id) && self.undo.is_some() {
                    self.record(Box::new(move |s| {
                        add_edge(&mut s.blob_referrers, blob, id);
                    }));
                }
            }

            if let Some(prior_by) = self.state.redactions.remove(&id)
                && self.undo.is_some()
            {
                self.record(Box::new(move |s| {
                    s.redactions.insert(id, prior_by);
                }));
            }
        }

        Ok(purge_ids)
    }

    fn begin(&mut self) -> Result<(), Error> {
        if self.undo.is_some() {
            return Err(Error::Storage("transaction already open".into()));
        }
        self.undo = Some(Vec::new());
        Ok(())
    }

    fn commit(&mut self) -> Result<(), Error> {
        self.undo
            .take()
            .map(drop)
            .ok_or_else(|| Error::Storage("commit without an open transaction".into()))
    }

    fn rollback(&mut self) -> Result<(), Error> {
        let log = self
            .undo
            .take()
            .ok_or_else(|| Error::Storage("rollback without an open transaction".into()))?;
        for undo in log.into_iter().rev() {
            undo(&mut self.state);
        }
        Ok(())
    }
}
