//! Full sync sessions between in-process databases — the complete protocol
//! with the network removed. The exchange closure IS the transport; a
//! hostile pipe is a closure that lies.

use vouch_core::sync::{
    Error, InstanceId, MemorySyncState, Request, Response, SyncReport, SyncSession, drive, respond,
};
use vouch_core::{ClaimRef, Database, LogId, SignedEvent, Value, Writer};

/// A peer: a database plus the incarnation of its arrival order.
struct Peer {
    db: Database,
    instance: InstanceId,
}

impl Peer {
    fn new(tag: u8) -> Peer {
        Peer {
            db: Database::new(),
            instance: InstanceId([tag; 16]),
        }
    }
}

/// One full session against an honest in-process peer.
fn sync(
    client: &mut Database,
    state: &mut MemorySyncState,
    name: &str,
    peer: &mut Peer,
    pull: &[LogId],
    push: &[LogId],
) -> SyncReport {
    let session = SyncSession::new(name, 1000, pull.to_vec(), push.to_vec());
    drive(client, state, session, |req| {
        respond(&mut peer.db, peer.instance, 1000, req).map_err(Error::from)
    })
    .unwrap()
}

fn rec(db: &mut Database, log: &LogId, at: i64, text: &str) -> SignedEvent {
    db.claim(
        log,
        Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(at)),
            ("body", Value::text(text)),
        ]),
    )
    .unwrap()
}

fn redact(db: &mut Database, log: &LogId, at: i64, target: &SignedEvent) -> SignedEvent {
    db.claim(
        log,
        Value::map([
            ("type", Value::text("redact")),
            ("at", Value::Int(at)),
            (
                "redacts",
                Value::ClaimRef(ClaimRef {
                    log_id: target.header().unwrap().log_id,
                    hash: target.id(),
                }),
            ),
        ]),
    )
    .unwrap()
}

fn converged(a: &Database, b: &Database) {
    assert_eq!(a.claims().state_vector(), b.claims().state_vector());
    assert!(a.claims().verify_integrity().is_empty());
    assert!(b.claims().verify_integrity().is_empty());
}

#[test]
fn publish_then_cold_catch_up_converges_and_idles_in_one_message() {
    // The author pushes to a relay; a stranger pulls from it.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([1; 32]));
    for i in 0..5 {
        rec(&mut author, &log, 100 + i, &format!("rec {i}"));
    }
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    let report = sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );
    assert_eq!(report.pushed, 5);
    converged(&author, &relay.db);

    let mut reader = Database::new();
    let mut reader_state = MemorySyncState::new();
    let report = sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );
    assert_eq!(report.pulled, 5);
    assert_eq!(report.reconciled, 0);
    converged(&reader, &author);

    // Caught up and settled: an idle re-sync is exactly one Status
    // message — the settle rides the same answer.
    let mut messages = 0;
    let session = SyncSession::new("relay", 2000, vec![log], vec![]);
    let report = drive(&mut reader, &mut reader_state, session, |req| {
        messages += 1;
        respond(&mut relay.db, relay.instance, 2000, req).map_err(Error::from)
    })
    .unwrap();
    assert_eq!(messages, 1);
    assert_eq!(report, SyncReport::default());
}

#[test]
fn incremental_push_sends_only_what_is_new() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([2; 32]));
    rec(&mut author, &log, 1, "first");
    rec(&mut author, &log, 2, "second");

    let mut relay = Peer::new(1);
    let mut state = MemorySyncState::new();
    assert_eq!(
        sync(&mut author, &mut state, "relay", &mut relay, &[], &[log]).pushed,
        2
    );

    rec(&mut author, &log, 3, "third");
    let report = sync(&mut author, &mut state, "relay", &mut relay, &[], &[log]);
    assert_eq!(report.pushed, 1);
    assert_eq!(relay.db.claims().log_len(&log), 3);
    converged(&author, &relay.db);
}

#[test]
fn two_devices_one_log_converge_through_a_relay() {
    // Same mnemonic restored on two devices: one log, two writers minting
    // independently. There is nothing to fork — claims are content-addressed
    // piles, and the relay merges them like anyone else's.
    let mut phone = Database::new();
    let mut laptop = Database::new();
    let log = phone.add_writer(Writer::from_seed([3; 32]));
    laptop.add_writer(Writer::from_seed([3; 32]));

    rec(&mut phone, &log, 1, "from the phone");
    rec(&mut laptop, &log, 2, "from the laptop");

    let mut relay = Peer::new(1);
    let mut phone_state = MemorySyncState::new();
    let mut laptop_state = MemorySyncState::new();

    sync(
        &mut phone,
        &mut phone_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );
    sync(
        &mut laptop,
        &mut laptop_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );
    sync(
        &mut phone,
        &mut phone_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );

    assert_eq!(phone.claims().log_len(&log), 2);
    converged(&phone, &laptop);
    converged(&phone, &relay.db);
}

#[test]
fn relay_reborn_with_a_new_instance_resets_cursors_and_heals() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([4; 32]));
    for i in 0..5 {
        rec(&mut author, &log, i, &format!("rec {i}"));
    }
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );

    let mut reader = Database::new();
    let mut reader_state = MemorySyncState::new();
    sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );

    // The relay dies and comes back empty under a fresh instance.
    let mut relay = Peer::new(2);

    // The author notices the new incarnation and re-publishes everything.
    let report = sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );
    assert_eq!(report.cursor_resets, 1);
    assert_eq!(report.pushed, 5);
    converged(&author, &relay.db);

    // The reader resets too; the re-download is all duplicates.
    let report = sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );
    assert_eq!(report.cursor_resets, 1);
    assert_eq!(report.pulled, 0);
    converged(&reader, &author);
}

#[test]
fn stale_backup_under_the_same_instance_is_caught_by_the_fingerprint() {
    // The hazard cursors cannot see: a relay restored from backup that
    // KEEPS its instance id. Counts can line up again while the sets
    // differ; only the fingerprint settle notices.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([5; 32]));
    let events: Vec<SignedEvent> = (0..5)
        .map(|i| rec(&mut author, &log, i, &format!("rec {i}")))
        .collect();

    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );

    // A reader catches up fully and settles.
    let mut reader = Database::new();
    let mut reader_state = MemorySyncState::new();
    sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );

    // Restore the relay from a backup holding only the first three claims
    // — same instance, rewound arrival order.
    let mut restored = Peer::new(1);
    for e in &events[..3] {
        restored.db.ingest(e.clone()).unwrap();
    }
    let mut relay = restored;

    // The author's session: nothing to pull by cursor (count < cursor!),
    // nothing to push by cursor — but the fingerprint disagrees, the
    // hash-list diff finds the two missing claims, and the author may
    // publish them.
    let report = sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );
    assert_eq!(report.cursor_resets, 0);
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.pushed, 2);
    assert_eq!(relay.db.claims().log_len(&log), 5);
    converged(&author, &relay.db);

    // The reader settles back to a clean match without re-downloading.
    let report = sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );
    assert_eq!(report.pulled, 0);
    converged(&reader, &author);
}

#[test]
fn a_pull_only_reader_caches_the_benign_difference() {
    // A reader holding MORE than the relay (it can't publish there) should
    // reconcile once, learn the difference is benign, and never grind
    // through hash lists again until the relay's set actually changes.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([6; 32]));
    let events: Vec<SignedEvent> = (0..4)
        .map(|i| rec(&mut author, &log, i, &format!("rec {i}")))
        .collect();

    // The relay only ever got the first two claims.
    let mut relay = Peer::new(1);
    for e in &events[..2] {
        relay.db.ingest(e.clone()).unwrap();
    }

    // The reader got everything directly from the author beforehand.
    let mut reader = Database::new();
    for e in &events {
        reader.ingest(e.clone()).unwrap();
    }

    let mut state = MemorySyncState::new();
    let report = sync(&mut reader, &mut state, "relay", &mut relay, &[log], &[]);
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.pulled, 0);

    // Second session: the difference is known benign — no reconciliation,
    // and the whole sync is one Status message.
    let mut messages = 0;
    let session = SyncSession::new("relay", 2000, vec![log], vec![]);
    let report = drive(&mut reader, &mut state, session, |req| {
        messages += 1;
        respond(&mut relay.db, relay.instance, 2000, req).map_err(Error::from)
    })
    .unwrap();
    assert_eq!(report.reconciled, 0);
    assert_eq!(messages, 1);
}

#[test]
fn stripped_bodies_heal_through_reconciliation() {
    // "Have" means "have the body": a reader that settled against a
    // bodiless relay must re-reconcile when the bodies arrive, even though
    // no new rows do — counts never move, only the fingerprint does.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([7; 32]));
    let events: Vec<SignedEvent> = (0..3)
        .map(|i| rec(&mut author, &log, i, &format!("rec {i}")))
        .collect();

    // The relay first learns the claims from a lossy pipe: headers only.
    let mut relay = Peer::new(1);
    for e in &events {
        relay.db.ingest(e.without_body()).unwrap();
    }

    let mut reader = Database::new();
    let mut state = MemorySyncState::new();
    let report = sync(&mut reader, &mut state, "relay", &mut relay, &[log], &[]);
    assert_eq!(report.pulled, 3);
    assert_eq!(report.healed, 0);
    assert!(!reader.claims().contains(&events[0].id()));

    // The author publishes the real thing; the relay's bodies fill in.
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );
    assert!(relay.db.claims().contains(&events[0].id()));

    // The reader's cursor is already at the count — only the settle
    // fingerprint says anything changed. Reconciliation fetches the bodies.
    let report = sync(&mut reader, &mut state, "relay", &mut relay, &[log], &[]);
    assert_eq!(report.reconciled, 1);
    assert_eq!(report.healed, 3);
    converged(&reader, &author);
}

#[test]
fn redaction_travels_as_a_tombstone_and_the_body_is_never_fetched() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([8; 32]));
    rec(&mut author, &log, 1, "keep me");
    let regret = rec(&mut author, &log, 2, "redact me");
    redact(&mut author, &log, 3, &regret);

    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );

    let mut reader = Database::new();
    let mut reader_state = MemorySyncState::new();
    let report = sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );
    assert_eq!(report.reconciled, 0);

    // The reader holds the signed tombstone, knows why it's bodiless, and
    // never saw the content.
    assert!(reader.claims().redaction(&regret.id()).is_some());
    assert!(!reader.claims().contains(&regret.id()));
    assert!(reader.claims().get(&regret.id()).unwrap().body.is_none());
    converged(&reader, &author);
}

#[test]
fn a_pipe_cannot_smuggle_logs_you_did_not_ask_for() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([9; 32]));
    rec(&mut author, &log, 1, "subscribed content");

    // A perfectly valid claim from a log the reader never subscribed to.
    let mut other = Database::new();
    let other_log = other.add_writer(Writer::from_seed([10; 32]));
    let smuggled = rec(&mut other, &other_log, 1, "unsolicited");

    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[],
        &[log],
    );

    let mut reader = Database::new();
    let mut state = MemorySyncState::new();
    let session = SyncSession::new("relay", 1000, vec![log], vec![]);
    let report = drive(&mut reader, &mut state, session, |req| {
        let mut resp = respond(&mut relay.db, relay.instance, 1000, req).map_err(Error::from)?;
        // The hostile pipe appends the foreign event to every batch.
        if let Response::Events { events } = &mut resp {
            events.push(smuggled.clone());
        }
        Ok(resp)
    })
    .unwrap();

    assert!(report.off_plan > 0);
    assert_eq!(reader.claims().log_len(&other_log), 0);
    assert_eq!(reader.claims().log_len(&log), 1);
}

#[test]
fn sessions_move_claims_never_bytes_and_a_pull_heals_the_want() {
    // Media is pull-only: a publish session carries the claims and leaves
    // the bytes behind as a want at the receiver — which one GetBlob,
    // from anyone holding them, then heals.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([11; 32]));
    let photo = b"jpeg bytes, allegedly".to_vec();
    let blob_ref = author.attach(photo.clone(), "image/jpeg").unwrap();
    author
        .claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("at", Value::Int(1)),
                ("photo", Value::BlobRef(blob_ref.clone())),
            ]),
        )
        .unwrap();

    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    let session = SyncSession::new("relay", 1000, vec![], vec![log]);
    let report = drive(&mut author, &mut author_state, session, |req| {
        assert!(
            !matches!(req, Request::GetBlob { .. } | Request::PutBlob { .. }),
            "a session must never move blob bytes"
        );
        respond(&mut relay.db, relay.instance, 1000, req).map_err(Error::from)
    })
    .unwrap();
    assert_eq!(report.pushed, 1);
    // Claims landed; bytes did not — the want is the relay's to act on.
    assert_eq!(relay.db.claims().log_len(&log), 1);
    assert!(!relay.db.blobs().contains(&blob_ref.hash));
    assert_eq!(relay.db.missing_blobs().len(), 1);

    // One pull from the author and the want closes.
    assert!(fetch_from(&mut relay.db, &mut author, blob_ref.hash));
    assert!(relay.db.missing_blobs().is_empty());
    assert_eq!(relay.db.blobs().get(&blob_ref.hash).unwrap(), photo);
}

/// One demand-driven blob pull: `wanter` asks `holder` for `hash`.
fn fetch_from(wanter: &mut Database, holder: &mut Database, hash: vouch_core::BlobHash) -> bool {
    let response = respond(holder, InstanceId([0; 16]), 1000, Request::GetBlob { hash }).unwrap();
    match response {
        Response::Blob { bytes: Some(bytes) } => wanter.ingest_blob(hash, bytes).unwrap_or(false),
        _ => false,
    }
}

#[test]
fn corrupt_blob_bytes_are_rejected_and_the_want_survives() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([12; 32]));
    let blob_ref = author.attach(b"real bytes".to_vec(), "text/plain").unwrap();
    author
        .claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("at", Value::Int(1)),
                ("file", Value::BlobRef(blob_ref.clone())),
            ]),
        )
        .unwrap();
    let mut reader = Database::new();
    let mut state = MemorySyncState::new();
    let mut relay = Peer::new(1);
    sync(&mut author, &mut state, "relay", &mut relay, &[], &[log]);
    let mut reader_state = MemorySyncState::new();
    sync(
        &mut reader,
        &mut reader_state,
        "relay",
        &mut relay,
        &[log],
        &[],
    );
    assert_eq!(reader.missing_blobs().len(), 1);

    // A lying pipe answers the pull with garbage: rejected at ingest, the
    // want stands, and an honest pull from anyone heals it.
    assert!(
        reader
            .ingest_blob(blob_ref.hash, b"garbage".to_vec())
            .is_err()
    );
    assert!(!reader.blobs().contains(&blob_ref.hash));
    assert_eq!(reader.missing_blobs().len(), 1);
    assert!(fetch_from(&mut reader, &mut author, blob_ref.hash));
    assert!(reader.missing_blobs().is_empty());
}

#[test]
fn feeding_a_finished_or_mismatched_session_is_a_protocol_error() {
    let mut db = Database::new();
    let mut state = MemorySyncState::new();
    let mut session = SyncSession::new("peer", 0, vec![], vec![]);
    assert!(session.next_request(&db).is_none());
    assert!(matches!(
        session.feed(
            &mut db,
            &mut state,
            Response::Ack {
                stored: 0,
                rejected: 0
            }
        ),
        Err(Error::Protocol(_))
    ));

    let mut db = Database::new();
    let log = db.add_writer(Writer::from_seed([13; 32]));
    let mut state = MemorySyncState::new();
    let mut session = SyncSession::new("peer", 0, vec![log], vec![]);
    assert!(matches!(
        session.next_request(&db),
        Some(Request::Status { .. })
    ));
    assert!(matches!(
        session.feed(&mut db, &mut state, Response::Events { events: vec![] }),
        Err(Error::Protocol(_))
    ));
}

#[test]
fn culled_media_re_fetches_like_a_website() {
    // The model: claims are state, blobs are cache. A device under
    // storage pressure evicts photo bytes; the claim keeps pinning the
    // hash, so it's simply a want again — and a single demand-driven pull
    // re-fetches it from whoever serves the claim's log. Repeatable
    // forever; sync never notices.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([20; 32]));
    let photo = b"a large vacation photo".to_vec();
    let blob_ref = author.attach(photo.clone(), "image/jpeg").unwrap();
    author
        .claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("at", Value::Int(1)),
                ("photo", Value::BlobRef(blob_ref.clone())),
            ]),
        )
        .unwrap();

    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(
        &mut author,
        &mut author_state,
        "relay",
        &mut relay,
        &[log],
        &[log],
    );
    assert!(fetch_from(&mut relay.db, &mut author, blob_ref.hash));

    // The reader catches up (claims only — lazy by default) and demands
    // the photo when the UI wants it.
    let mut reader = Database::new();
    let mut state = MemorySyncState::new();
    sync(&mut reader, &mut state, "relay", &mut relay, &[log], &[]);
    assert!(fetch_from(&mut reader, &mut relay.db, blob_ref.hash));
    assert!(reader.blobs().contains(&blob_ref.hash));

    // Storage pressure: cull. Only cache is affected — convergent state
    // is untouched (fingerprints still match), fsck stays clean, and the
    // hash is wanted again.
    assert!(reader.evict_blob(&blob_ref.hash).unwrap());
    assert!(!reader.blobs().contains(&blob_ref.hash));
    assert_eq!(
        reader.claims().fingerprint(&log),
        relay.db.claims().fingerprint(&log),
        "eviction must not perturb convergent state"
    );
    assert!(reader.claims().verify_integrity().is_empty());
    assert_eq!(reader.missing_blobs().len(), 1);

    // An idle re-sync stays one message — sync genuinely doesn't care.
    let mut messages = 0;
    let session = SyncSession::new("relay", 2000, vec![log], vec![]);
    let report = drive(&mut reader, &mut state, session, |req| {
        messages += 1;
        respond(&mut relay.db, relay.instance, 2000, req).map_err(Error::from)
    })
    .unwrap();
    assert_eq!(messages, 1);
    assert_eq!(report, SyncReport::default());

    // Re-query on demand; cull again; re-query again. Cache forever.
    assert!(fetch_from(&mut reader, &mut relay.db, blob_ref.hash));
    assert_eq!(reader.blobs().get(&blob_ref.hash).unwrap(), photo);
    assert!(reader.evict_blob(&blob_ref.hash).unwrap());
    assert!(fetch_from(&mut reader, &mut relay.db, blob_ref.hash));
    assert!(reader.missing_blobs().is_empty());
}
