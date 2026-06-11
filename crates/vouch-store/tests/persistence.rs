//! The same Database, durably: SQLite claims + file blobs behind the
//! vouch-core storage traits. The invariants under test here are the SAME
//! invariants vouch-core's tests pin against memory backends — that's the
//! point of the cut: logic written once, backends swapped underneath.

use std::path::PathBuf;

use vouch_core::{Database, SignedEvent, Value, Writer};

fn fresh_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vouch-store-test-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn rec(subject: &str) -> Value {
    Value::map([
        ("type", Value::text("rec")),
        ("subject", Value::text(subject)),
    ])
}

fn pull(from: &Database, into: &mut Database, log: &vouch_core::LogId) {
    for e in from.claims().serve_since(log, 0) {
        into.ingest(e).unwrap();
    }
}

#[test]
fn a_database_survives_restart() {
    let dir = fresh_dir("restart");
    let seed = [1u8; 32];

    // Session one: mint a photo rec and a plain rec, then drop everything.
    let (log, photo, fingerprint) = {
        let mut db = vouch_store::open(&dir).unwrap();
        let log = db.add_writer(Writer::from_seed(seed));
        let photo = db
            .attach(b"sunset over the counter".to_vec(), "image/jpeg")
            .unwrap();
        db.claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Joe's Pizza")),
                ("photo", Value::BlobRef(photo.clone())),
            ]),
        )
        .unwrap();
        db.claim(&log, rec("Blue Bottle")).unwrap();
        (log, photo, db.claims().fingerprint(&log))
    };

    // Session two: reopen. Claims, indexes, redactions, and blobs are all
    // there; the writer needs only its key to continue.
    let mut db = vouch_store::open(&dir).unwrap();
    assert_eq!(db.claims().len(), 2);
    assert_eq!(db.claims().fingerprint(&log), fingerprint);
    assert!(db.missing_blobs().is_empty());
    assert_eq!(
        db.blobs().get(&photo.hash),
        Some(b"sunset over the counter".to_vec())
    );
    assert_eq!(db.claims().log(&log).len(), 2);

    // A writer is a pure pen: nothing to resume, just re-add the key.
    db.add_writer(Writer::from_seed(seed));
    db.claim(&log, rec("the park")).unwrap();
    assert_eq!(db.claims().log_len(&log), 3);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn redaction_reaches_the_disk_and_survives_reopen() {
    let dir = fresh_dir("redact");
    let seed = [2u8; 32];

    let (log, target) = {
        let mut db = vouch_store::open(&dir).unwrap();
        let log = db.add_writer(Writer::from_seed(seed));
        let regret = db.claim(&log, rec("place I regret")).unwrap();
        db.claim(
            &log,
            Value::map([
                ("type", Value::text("redact")),
                (
                    "redacts",
                    Value::ClaimRef(vouch_core::ClaimRef {
                        log_id: log,
                        hash: regret.id(),
                    }),
                ),
            ]),
        )
        .unwrap();
        (log, regret.id())
    };

    // Reopen: the tombstone is a tombstone on disk — body gone, redaction
    // authority recorded, signature still verifiable, cursor intact.
    let db = vouch_store::open(&dir).unwrap();
    assert!(!db.claims().contains(&target));
    let tomb = db.claims().get(&target).expect("tombstone persisted");
    assert!(tomb.body.is_none());
    assert!(tomb.signed.body_bytes.is_none());
    tomb.signed.verify().expect("tombstone still verifies");
    assert!(db.claims().redaction(&target).is_some());
    assert_eq!(db.claims().log_len(&log), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sqlite_and_memory_databases_converge() {
    // The cut's contract: the SAME logic drives both backends, so a
    // durable database and an in-memory one are sync peers with identical
    // semantics — state vectors compare equal across backends.
    let dir = fresh_dir("converge");

    let mut durable = vouch_store::open(&dir).unwrap();
    let log = durable.add_writer(Writer::from_seed([3; 32]));
    let photo = durable.attach(b"latte art".to_vec(), "image/jpeg").unwrap();
    durable
        .claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Blue Bottle")),
                ("photo", Value::BlobRef(photo.clone())),
            ]),
        )
        .unwrap();
    let vouched: SignedEvent = durable.claims().serve_since(&log, 0).remove(0);

    let mut memory = Database::new();
    let bob = Writer::from_seed([4; 32]);
    let bob_log = bob.id();
    memory.add_writer(bob);
    let bob_vouch = memory
        .claim(
            &bob_log,
            Value::map([
                ("type", Value::text("vouch")),
                ("original", Value::Embed(Box::new(vouched))),
            ]),
        )
        .unwrap();
    durable.ingest(bob_vouch).unwrap();
    pull(&durable, &mut memory, &log);

    assert_eq!(
        durable.claims().state_vector(),
        memory.claims().state_vector()
    );
    assert_eq!(
        durable.claims().fingerprint(&log),
        memory.claims().fingerprint(&log)
    );
    assert_eq!(
        durable.claims().fingerprint(&bob_log),
        memory.claims().fingerprint(&bob_log)
    );
    // The memory peer wants the photo; the durable one holds it on disk.
    assert_eq!(memory.missing_blobs(), vec![photo.clone()]);
    memory
        .ingest_blob(photo.hash, b"latte art".to_vec())
        .unwrap();
    assert!(memory.missing_blobs().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn corrupt_blob_files_degrade_to_missing_and_heal() {
    let dir = fresh_dir("corrupt");

    let mut db = vouch_store::open(&dir).unwrap();
    let log = db.add_writer(Writer::from_seed([5; 32]));
    let photo = db.attach(b"original bytes".to_vec(), "image/png").unwrap();
    db.claim(
        &log,
        Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("gallery")),
            ("photo", Value::BlobRef(photo.clone())),
        ]),
    )
    .unwrap();

    // Disk rot: someone scribbles over the blob file.
    let blob_path = dir.join("blobs").join(photo.hash.to_string());
    std::fs::write(&blob_path, b"bitrot").unwrap();

    // Corrupt bytes read as ABSENT, never as wrong bytes — so the want-list
    // re-lists the blob.
    assert_eq!(db.blobs().get(&photo.hash), None);
    assert_eq!(db.missing_blobs(), vec![photo.clone()]);

    // Healing takes a SINGLE fetch: verified bytes are written through even
    // though a (corrupt) file exists under that hash, no prior read-evict
    // pass required. Re-corrupt to prove the heal didn't depend on the
    // eviction above.
    std::fs::write(&blob_path, b"bitrot again").unwrap();
    let was_new = db
        .ingest_blob(photo.hash, b"original bytes".to_vec())
        .unwrap();
    assert!(
        !was_new,
        "blob hash was already present (corrupt), so not 'new'"
    );
    assert_eq!(
        db.blobs().get(&photo.hash),
        Some(b"original bytes".to_vec())
    );
    assert!(db.missing_blobs().is_empty());

    let _ = std::fs::remove_dir_all(&dir);
}

/// Wraps the real SQLite backend and fails the Nth `put_claim`, forwarding
/// transactions — so we can prove a failed multi-write ingest rolls back
/// to NOTHING, not to a partial write set.
struct FailNthPut {
    inner: vouch_store::SqliteClaimStorage,
    puts_until_failure: std::cell::Cell<i64>,
}

impl vouch_core::ClaimStorage for FailNthPut {
    fn get_claim(
        &self,
        id: &vouch_core::ClaimHash,
    ) -> Result<Option<vouch_core::StoredClaim>, vouch_core::Error> {
        self.inner.get_claim(id)
    }
    fn put_claim(&mut self, claim: vouch_core::StoredClaim) -> Result<(), vouch_core::Error> {
        let left = self.puts_until_failure.get();
        self.puts_until_failure.set(left - 1);
        if left == 0 {
            // Fail exactly the Nth put, then recover (the "restart").
            return Err(vouch_core::Error::Storage("injected fault".into()));
        }
        self.inner.put_claim(claim)
    }
    fn claim_count(&self) -> Result<usize, vouch_core::Error> {
        self.inner.claim_count()
    }
    fn scan_claims(
        &self,
        visit: &mut dyn FnMut(&vouch_core::StoredClaim),
    ) -> Result<(), vouch_core::Error> {
        self.inner.scan_claims(visit)
    }
    fn scan_log(
        &self,
        log: &vouch_core::LogId,
        visit: &mut dyn FnMut(&vouch_core::StoredClaim),
    ) -> Result<(), vouch_core::Error> {
        self.inner.scan_log(log, visit)
    }
    fn add_backlink(
        &mut self,
        target: vouch_core::ClaimHash,
        source: vouch_core::ClaimHash,
    ) -> Result<(), vouch_core::Error> {
        self.inner.add_backlink(target, source)
    }
    fn remove_backlink(
        &mut self,
        target: &vouch_core::ClaimHash,
        source: &vouch_core::ClaimHash,
    ) -> Result<(), vouch_core::Error> {
        self.inner.remove_backlink(target, source)
    }
    fn backlinks(
        &self,
        target: &vouch_core::ClaimHash,
    ) -> Result<Vec<vouch_core::ClaimHash>, vouch_core::Error> {
        self.inner.backlinks(target)
    }
    fn add_blob_referrer(
        &mut self,
        blob: vouch_core::BlobHash,
        source: vouch_core::ClaimHash,
    ) -> Result<(), vouch_core::Error> {
        self.inner.add_blob_referrer(blob, source)
    }
    fn remove_blob_referrer(
        &mut self,
        blob: &vouch_core::BlobHash,
        source: &vouch_core::ClaimHash,
    ) -> Result<(), vouch_core::Error> {
        self.inner.remove_blob_referrer(blob, source)
    }
    fn blob_referrers(
        &self,
        blob: &vouch_core::BlobHash,
    ) -> Result<Vec<vouch_core::ClaimHash>, vouch_core::Error> {
        self.inner.blob_referrers(blob)
    }
    fn redaction(
        &self,
        target: &vouch_core::ClaimHash,
    ) -> Result<Option<vouch_core::ClaimHash>, vouch_core::Error> {
        self.inner.redaction(target)
    }
    fn set_redaction(
        &mut self,
        target: vouch_core::ClaimHash,
        by: vouch_core::ClaimHash,
    ) -> Result<(), vouch_core::Error> {
        self.inner.set_redaction(target, by)
    }
    fn scan_redactions(
        &self,
        visit: &mut dyn FnMut(vouch_core::ClaimHash, vouch_core::ClaimHash),
    ) -> Result<(), vouch_core::Error> {
        self.inner.scan_redactions(visit)
    }
    fn scan_backlinks(
        &self,
        visit: &mut dyn FnMut(vouch_core::ClaimHash, vouch_core::ClaimHash),
    ) -> Result<(), vouch_core::Error> {
        self.inner.scan_backlinks(visit)
    }
    fn scan_blob_referrers(
        &self,
        visit: &mut dyn FnMut(vouch_core::BlobHash, vouch_core::ClaimHash),
    ) -> Result<(), vouch_core::Error> {
        self.inner.scan_blob_referrers(visit)
    }
    fn begin(&mut self) -> Result<(), vouch_core::Error> {
        self.inner.begin()
    }
    fn commit(&mut self) -> Result<(), vouch_core::Error> {
        self.inner.commit()
    }
    fn rollback(&mut self) -> Result<(), vouch_core::Error> {
        self.inner.rollback()
    }
}

#[test]
fn a_failed_ingest_rolls_back_to_nothing_on_sqlite() {
    // A vouch embedding a rec is one ingest with TWO put_claims (embedded
    // first, outer second). Fail the second: with real transactions the
    // first must vanish too — atomicity, not just healability.
    let mut alice = Writer::from_seed([6; 32]);
    let mut bob = Writer::from_seed([7; 32]);
    let alice_rec = alice.claim(rec("Joe's Pizza")).unwrap();
    let bob_vouch = bob
        .claim(Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(alice_rec))),
        ]))
        .unwrap();

    let storage = FailNthPut {
        inner: vouch_store::SqliteClaimStorage::open_in_memory().unwrap(),
        puts_until_failure: std::cell::Cell::new(1), // first put ok, second fails
    };
    let mut db = Database::with_stores(
        Box::new(storage),
        Box::new(vouch_core::MemoryBlobStorage::new()),
    );

    let err = db.ingest(bob_vouch.clone()).unwrap_err();
    assert!(matches!(err, vouch_core::Error::Storage(_)));
    // The embedded claim's put succeeded inside the transaction — and is
    // GONE after rollback. No partial state, not even healable debris.
    assert_eq!(db.claims().len(), 0);
    assert!(db.claims().verify_integrity().is_empty());

    // The store remains usable; redelivery completes cleanly.
    db.ingest(bob_vouch).unwrap();
    assert_eq!(db.claims().len(), 2);
    assert!(db.claims().verify_integrity().is_empty());
}
