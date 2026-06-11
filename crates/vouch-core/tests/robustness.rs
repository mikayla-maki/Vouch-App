//! Fault injection and invariant checking, in the spirit of Oxide's iddqd
//! work: don't argue the store survives a crash at any write — try every
//! write.
//!
//! The contract under test: ingest's write ordering makes `put_claim` the
//! commit point, and every earlier write is an idempotent upsert. So a
//! backend that dies at ANY write boundary, followed by at-least-once
//! redelivery, must converge to exactly the never-crashed state — with a
//! clean integrity check. If a refactor ever reorders the writes, the
//! exhaustive sweep below fails.

use std::cell::Cell;
use std::rc::Rc;

use vouch_core::storage::{ClaimStorage, MemoryClaimStorage};
use vouch_core::value::{BlobHash, ClaimHash};
use vouch_core::{
    ClaimRef, ClaimStore, Error, LogId, Provenance, SignedEvent, StoredClaim, Value, Writer,
};

/// Wraps the memory backend; every MUTATING call spends from a shared
/// budget and fails once it's gone (reads stay up: a crashed-and-restarted
/// process can still see its partial disk). Deliberately does NOT
/// implement transactions — this simulates the worst backend, where
/// partial writes persist.
struct FlakyStorage {
    inner: MemoryClaimStorage,
    budget: Rc<Cell<i64>>,
    writes: Rc<Cell<u64>>,
    /// When false, transactions are EXPLICIT no-ops — the conscious
    /// "partial state can persist across a crash" choice the trait now
    /// forces a backend to write out. When true, forwarded to the memory
    /// backend's undo log.
    transactional: bool,
    /// If `Some(n)`, the n-th write fails ONCE (a transient blip), then the
    /// backend recovers — unlike `budget` which fails forever after.
    fail_once_at: Cell<Option<u64>>,
}

impl FlakyStorage {
    fn spend(&mut self) -> Result<(), Error> {
        self.writes.set(self.writes.get() + 1);
        if self.fail_once_at.get() == Some(self.writes.get()) {
            self.fail_once_at.set(None);
            return Err(Error::Storage("transient fault".into()));
        }
        let left = self.budget.get();
        if left <= 0 {
            return Err(Error::Storage("injected fault".into()));
        }
        self.budget.set(left - 1);
        Ok(())
    }
}

impl ClaimStorage for FlakyStorage {
    fn get_claim(&self, id: &ClaimHash) -> Result<Option<StoredClaim>, Error> {
        self.inner.get_claim(id)
    }
    fn put_claim(&mut self, claim: StoredClaim) -> Result<(), Error> {
        self.spend()?;
        self.inner.put_claim(claim)
    }
    fn claim_count(&self) -> Result<usize, Error> {
        self.inner.claim_count()
    }
    fn scan_claims(&self, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        self.inner.scan_claims(visit)
    }
    fn scan_log(&self, log: &LogId, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        self.inner.scan_log(log, visit)
    }
    fn add_backlink(&mut self, target: ClaimHash, source: ClaimHash) -> Result<(), Error> {
        self.spend()?;
        self.inner.add_backlink(target, source)
    }
    fn remove_backlink(&mut self, target: &ClaimHash, source: &ClaimHash) -> Result<(), Error> {
        self.spend()?;
        self.inner.remove_backlink(target, source)
    }
    fn backlinks(&self, target: &ClaimHash) -> Result<Vec<ClaimHash>, Error> {
        self.inner.backlinks(target)
    }
    fn add_blob_referrer(&mut self, blob: BlobHash, source: ClaimHash) -> Result<(), Error> {
        self.spend()?;
        self.inner.add_blob_referrer(blob, source)
    }
    fn remove_blob_referrer(&mut self, blob: &BlobHash, source: &ClaimHash) -> Result<(), Error> {
        self.spend()?;
        self.inner.remove_blob_referrer(blob, source)
    }
    fn blob_referrers(&self, blob: &BlobHash) -> Result<Vec<ClaimHash>, Error> {
        self.inner.blob_referrers(blob)
    }
    fn redaction(&self, target: &ClaimHash) -> Result<Option<ClaimHash>, Error> {
        self.inner.redaction(target)
    }
    fn set_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<(), Error> {
        self.spend()?;
        self.inner.set_redaction(target, by)
    }
    fn scan_redactions(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_redactions(visit)
    }
    fn scan_backlinks(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_backlinks(visit)
    }
    fn scan_blob_referrers(&self, visit: &mut dyn FnMut(BlobHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_blob_referrers(visit)
    }
    fn begin(&mut self) -> Result<(), Error> {
        if self.transactional {
            self.inner.begin()
        } else {
            Ok(()) // deliberately non-atomic: simulates the worst backend
        }
    }
    fn commit(&mut self) -> Result<(), Error> {
        if self.transactional {
            self.inner.commit()
        } else {
            Ok(())
        }
    }
    fn rollback(&mut self) -> Result<(), Error> {
        if self.transactional {
            self.inner.rollback()
        } else {
            Ok(())
        }
    }
}

/// A write-heavy scenario touching every invariant path: embeds (recursive
/// multi-claim ingest), cross-log refs (backlinks), media (blob
/// referrers), redaction (entry + body drop + index removal), and a
/// tombstone fill-in.
fn scenario() -> Vec<SignedEvent> {
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let mut carol = Writer::from_seed([3; 32]);

    let blob = vouch_core::BlobRef {
        hash: BlobHash([7; 32]),
        size: 9,
        mime: "image/png".into(),
    };
    let rec1 = alice
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("Joe's Pizza")),
            ("photo", Value::BlobRef(blob)),
        ]))
        .unwrap();
    // rec2 (the redaction target below) carries BOTH an outgoing ref and an
    // outgoing blob, so redacting it exercises the edge-removal path — the
    // crash window between dropping the body and clearing the index.
    let rec2 = alice
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("place I regret")),
            (
                "about",
                Value::ClaimRef(ClaimRef {
                    log_id: alice.id(),
                    hash: rec1.id(),
                }),
            ),
            (
                "photo",
                Value::BlobRef(vouch_core::BlobRef {
                    hash: BlobHash([8; 32]),
                    size: 3,
                    mime: "image/png".into(),
                }),
            ),
        ]))
        .unwrap();
    let vouch = bob
        .claim(Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(rec1.clone()))),
        ]))
        .unwrap();
    let disavowal = carol
        .claim(Value::map([
            ("type", Value::text("disavowal")),
            (
                "disavows",
                Value::ClaimRef(ClaimRef {
                    log_id: alice.id(),
                    hash: rec2.id(),
                }),
            ),
        ]))
        .unwrap();
    let redact = alice
        .claim(Value::map([
            ("type", Value::text("redact")),
            (
                "redacts",
                Value::ClaimRef(ClaimRef {
                    log_id: alice.id(),
                    hash: rec2.id(),
                }),
            ),
        ]))
        .unwrap();
    vec![
        rec1.without_body(), // tombstone first: exercises fill-in
        rec2,
        vouch,
        rec1, // body arrives later
        disavowal,
        redact,
    ]
}

fn flaky_store(budget: i64, transactional: bool) -> (ClaimStore, Rc<Cell<i64>>, Rc<Cell<u64>>) {
    let budget = Rc::new(Cell::new(budget));
    let writes = Rc::new(Cell::new(0u64));
    let storage = FlakyStorage {
        inner: MemoryClaimStorage::new(),
        budget: budget.clone(),
        writes: writes.clone(),
        transactional,
        fail_once_at: Cell::new(None),
    };
    (ClaimStore::with_storage(Box::new(storage)), budget, writes)
}

#[test]
fn crash_at_every_write_point_heals_under_redelivery() {
    let events = scenario();

    // The control: no faults, ever.
    let mut control = ClaimStore::new();
    for e in &events {
        control.ingest(e.clone()).unwrap();
    }
    let control_state = control.state_vector();
    assert!(control.verify_integrity().is_empty());

    // Count the writes a clean run performs.
    let (mut counter, _, writes) = flaky_store(i64::MAX, false);
    for e in &events {
        counter.ingest(e.clone()).unwrap();
    }
    let total = writes.get();
    assert!(total > 10, "scenario should be write-heavy, got {total}");

    // Crash at write N, for every N; redeliver; demand exact convergence.
    for n in 0..total {
        let (mut store, budget, _) = flaky_store(n as i64, false);
        for e in &events {
            let _ = store.ingest(e.clone()); // faults expected
        }
        budget.set(i64::MAX); // "restart": storage works again
        for e in &events {
            store
                .ingest(e.clone())
                .unwrap_or_else(|err| panic!("redelivery failed at crash point {n}: {err}"));
        }
        assert_eq!(
            store.state_vector(),
            control_state,
            "state diverged after crash at write {n}"
        );
        let problems = store.verify_integrity();
        assert!(
            problems.is_empty(),
            "integrity violations after crash at write {n}: {problems:?}"
        );
    }
}

#[test]
fn transactional_backends_leave_zero_debris() {
    // The stronger property when the backend honors transactions (memory
    // undo log here, SQLite's journal in vouch-store): a failed ingest
    // doesn't merely heal under redelivery — it never happened. At every
    // crash point, the store passes fsck BEFORE any redelivery, then
    // redelivery completes the picture.
    let events = scenario();

    let mut control = ClaimStore::new();
    for e in &events {
        control.ingest(e.clone()).unwrap();
    }
    let control_state = control.state_vector();

    let (mut counter, _, writes) = flaky_store(i64::MAX, true);
    for e in &events {
        counter.ingest(e.clone()).unwrap();
    }
    let total = writes.get();

    for n in 0..total {
        let (mut store, budget, _) = flaky_store(n as i64, true);
        for e in &events {
            let _ = store.ingest(e.clone()); // faults expected
        }
        // Zero debris: integrity holds even before redelivery.
        let problems = store.verify_integrity();
        assert!(
            problems.is_empty(),
            "debris after crash at write {n}: {problems:?}"
        );
        budget.set(i64::MAX);
        for e in &events {
            store.ingest(e.clone()).unwrap();
        }
        assert_eq!(
            store.state_vector(),
            control_state,
            "state diverged after crash at write {n}"
        );
    }
}

/// Panics mid-ingest must not let a caller observe half-applied state: the
/// store poisons itself, Mutex-style.
struct PanickingStorage {
    inner: MemoryClaimStorage,
}

impl ClaimStorage for PanickingStorage {
    fn get_claim(&self, id: &ClaimHash) -> Result<Option<StoredClaim>, Error> {
        self.inner.get_claim(id)
    }
    fn put_claim(&mut self, _claim: StoredClaim) -> Result<(), Error> {
        panic!("simulated backend panic");
    }
    fn claim_count(&self) -> Result<usize, Error> {
        self.inner.claim_count()
    }
    fn scan_claims(&self, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        self.inner.scan_claims(visit)
    }
    fn scan_log(&self, log: &LogId, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        self.inner.scan_log(log, visit)
    }
    fn add_backlink(&mut self, target: ClaimHash, source: ClaimHash) -> Result<(), Error> {
        self.inner.add_backlink(target, source)
    }
    fn remove_backlink(&mut self, target: &ClaimHash, source: &ClaimHash) -> Result<(), Error> {
        self.inner.remove_backlink(target, source)
    }
    fn backlinks(&self, target: &ClaimHash) -> Result<Vec<ClaimHash>, Error> {
        self.inner.backlinks(target)
    }
    fn add_blob_referrer(&mut self, blob: BlobHash, source: ClaimHash) -> Result<(), Error> {
        self.inner.add_blob_referrer(blob, source)
    }
    fn remove_blob_referrer(&mut self, blob: &BlobHash, source: &ClaimHash) -> Result<(), Error> {
        self.inner.remove_blob_referrer(blob, source)
    }
    fn blob_referrers(&self, blob: &BlobHash) -> Result<Vec<ClaimHash>, Error> {
        self.inner.blob_referrers(blob)
    }
    fn redaction(&self, target: &ClaimHash) -> Result<Option<ClaimHash>, Error> {
        self.inner.redaction(target)
    }
    fn set_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<(), Error> {
        self.inner.set_redaction(target, by)
    }
    fn scan_redactions(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_redactions(visit)
    }
    fn scan_backlinks(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_backlinks(visit)
    }
    fn scan_blob_referrers(&self, visit: &mut dyn FnMut(BlobHash, ClaimHash)) -> Result<(), Error> {
        self.inner.scan_blob_referrers(visit)
    }
    fn begin(&mut self) -> Result<(), Error> {
        self.inner.begin()
    }
    fn commit(&mut self) -> Result<(), Error> {
        self.inner.commit()
    }
    fn rollback(&mut self) -> Result<(), Error> {
        self.inner.rollback()
    }
}

#[test]
fn a_panic_mid_ingest_poisons_the_store() {
    let mut alice = Writer::from_seed([1; 32]);
    let event = alice
        .claim(Value::map([("type", Value::text("rec"))]))
        .unwrap();

    let mut store = ClaimStore::with_storage(Box::new(PanickingStorage {
        inner: MemoryClaimStorage::new(),
    }));

    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = store.ingest(event);
    }));
    assert!(unwound.is_err(), "backend panic should unwind");

    // Any further use fails loudly instead of serving partial state.
    let observed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| store.len()));
    let payload = observed.expect_err("poisoned store must refuse queries");
    let message = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .expect("panic carries a message");
    assert!(message.contains("poisoned"), "got: {message}");
}

#[test]
fn fsck_catches_a_lying_or_corrupted_backend() {
    let mut alice = Writer::from_seed([1; 32]);
    let event = alice
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("Joe's Pizza")),
        ]))
        .unwrap();
    let claim = event.verify().unwrap();

    // A backend with rotted rows: the claim's body bytes were tampered
    // (rows are self-authenticating, so this is detectable), plus a
    // phantom backlink pointing from a claim that doesn't exist.
    let mut storage = MemoryClaimStorage::new();
    let mut tampered = event.clone();
    tampered.body_bytes.as_mut().unwrap()[0] ^= 0xff;
    storage
        .put_claim(StoredClaim {
            signed: tampered,
            header: claim.header,
            body: claim.body,
            refs: vec![],
            blobs: vec![],
            provenance: Provenance::Direct,
            arrival: 0,
            received_at: 0,
        })
        .unwrap();
    storage
        .add_backlink(ClaimHash([9; 32]), ClaimHash([8; 32]))
        .unwrap();

    let store = ClaimStore::with_storage(Box::new(storage));
    let problems = store.verify_integrity();
    assert!(
        problems.iter().any(|p| p.contains("fails verification")),
        "tampered row not flagged: {problems:?}"
    );
    assert!(
        problems.iter().any(|p| p.contains("phantom backlink")),
        "phantom backlink not flagged: {problems:?}"
    );

    // And a healthy store reports nothing.
    let mut healthy = ClaimStore::new();
    healthy.ingest(event).unwrap();
    assert!(healthy.verify_integrity().is_empty());
}

#[test]
fn a_transient_fault_ingesting_an_embed_aborts_not_swallows() {
    // CRITICAL regression: a transient backend fault (one failed write that
    // would succeed on retry) DURING an embedded claim's ingest must abort
    // the whole transaction — not be silently miscounted as a "rejected
    // embed", commit a container without its embed, and return Ok (so a
    // sync layer advances its cursor and never redelivers).
    let mut bob = Writer::from_seed([2; 32]);
    let mut alice = Writer::from_seed([1; 32]);
    let inner = bob
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("the original")),
        ]))
        .unwrap();
    let container = alice
        .claim(Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(inner.clone()))),
        ]))
        .unwrap();

    // Embeds are ingested first, so the embed's put_claim is the FIRST
    // write. Fail it once, transactionally.
    let budget = Rc::new(Cell::new(i64::MAX));
    let writes = Rc::new(Cell::new(0u64));
    let storage = FlakyStorage {
        inner: MemoryClaimStorage::new(),
        budget,
        writes,
        transactional: true,
        fail_once_at: Cell::new(Some(1)),
    };
    let mut store = ClaimStore::with_storage(Box::new(storage));

    let err = store.ingest(container.clone()).unwrap_err();
    assert!(
        matches!(err, Error::Storage(_)),
        "transient embed fault must surface as a retriable error, got {err:?}"
    );
    // Nothing committed — not the container, not a phantom index row.
    assert!(store.is_empty());
    assert!(store.verify_integrity().is_empty());

    // Retry (the "redelivery"): now it lands cleanly, embed and all.
    store.ingest(container.clone()).unwrap();
    assert!(store.contains(&inner.id()));
    assert!(store.contains(&container.id()));
    assert!(store.verify_integrity().is_empty());
}
