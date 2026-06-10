//! The composition under test: a [`Database`] mints (attach → claim),
//! ingests from any pipe, serves peers, and maintains itself — a complete
//! sync session is two `Database`s and a loop, no I/O anywhere.

use vouch_core::{ClaimRef, Database, Error, LogId, SignedEvent, Value, Writer};

fn pull(from: &Database, into: &mut Database, log: &LogId, since: u64) {
    let events: Vec<SignedEvent> = from
        .claims()
        .serve_since(log, since)
        .into_iter()
        .cloned()
        .collect();
    for e in events {
        into.ingest(e).unwrap();
    }
}

#[test]
fn mint_sync_heal_redact_gc_full_session() {
    // Alice mints a rec with a photo: attach the bytes, pin the ref, claim.
    let mut alice = Database::new();
    let log = alice.add_writer(Writer::from_seed([1; 32]));
    let jpeg = b"golden hour at the counter".to_vec();
    let photo = alice.attach(jpeg.clone(), "image/jpeg");
    let rec = alice
        .claim(
            &log,
            100,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Joe's Pizza")),
                ("photo", Value::BlobRef(photo.clone())),
            ]),
        )
        .unwrap();
    // Her own claim is an ordinary claim in her own database, and the
    // attach-then-claim flow means she wants nothing.
    assert!(alice.claims().contains(&rec.id()));
    assert!(alice.missing_blobs().is_empty());

    // A follower catches up: claims now, fingerprint confirms, photo later.
    let mut bob = Database::new();
    pull(&alice, &mut bob, &log, 0);
    assert_eq!(
        bob.claims().fingerprint(&log),
        alice.claims().fingerprint(&log)
    );
    assert_eq!(bob.missing_blobs(), vec![photo.clone()]);

    // The photo heals from whatever pipe has it.
    assert!(bob.ingest_blob(photo.hash, jpeg).unwrap());
    assert!(bob.missing_blobs().is_empty());
    assert_eq!(bob.blobs().get(&photo.hash).map(<[u8]>::len), Some(26));

    // Alice regrets the rec and mints a redaction (seq 2).
    alice
        .claim(
            &log,
            200,
            Value::map([
                ("type", Value::text("redact")),
                (
                    "redacts",
                    Value::ClaimRef(ClaimRef {
                        log_id: log,
                        hash: rec.id(),
                    }),
                ),
            ]),
        )
        .unwrap();
    // Locally: body gone, photo orphaned, GC forgets the bytes.
    assert!(!alice.claims().contains(&rec.id()));
    assert_eq!(alice.gc_blobs(), vec![photo.hash]);

    // Bob pulls the increment; "seen is applied" drops the body, and his
    // own GC sweep forgets the photo too. The databases agree exactly.
    pull(&alice, &mut bob, &log, 1);
    assert!(!bob.claims().contains(&rec.id()));
    assert_eq!(bob.gc_blobs(), vec![photo.hash]);
    assert_eq!(
        bob.claims().fingerprint(&log),
        alice.claims().fingerprint(&log)
    );
    assert_eq!(bob.claims().state_vector(), alice.claims().state_vector());
}

#[test]
fn minting_requires_an_owned_log() {
    let mut db = Database::new();
    let stranger = Writer::from_seed([9; 32]).id();
    let body = Value::map([("type", Value::text("rec"))]);

    let err = db.claim(&stranger, 0, body.clone()).unwrap_err();
    assert!(matches!(err, Error::NotOurLog(id) if id == stranger));

    // A created log can be minted into immediately.
    let own = db.create_log().unwrap();
    db.claim(&own, 0, body).unwrap();
    assert_eq!(db.owned_logs().collect::<Vec<_>>(), vec![&own]);
    assert_eq!(db.claims().log(&own).len(), 1);
}

#[test]
fn a_relay_is_a_database_with_no_writers() {
    // Alice publishes to a relay; Bob has never met Alice and syncs only
    // from the relay. The relay holds keys for nobody and understands
    // nothing — it's the same Database type doing store-and-forward.
    let mut alice = Database::new();
    let log = alice.add_writer(Writer::from_seed([1; 32]));
    for (i, place) in ["Joe's", "Blue Bottle", "the park"].iter().enumerate() {
        alice
            .claim(
                &log,
                i as i64,
                Value::map([
                    ("type", Value::text("rec")),
                    ("subject", Value::text(*place)),
                ]),
            )
            .unwrap();
    }

    let mut relay = Database::new();
    pull(&alice, &mut relay, &log, 0);
    assert_eq!(relay.owned_logs().count(), 0);

    let mut bob = Database::new();
    pull(&relay, &mut bob, &log, 0);
    assert_eq!(bob.claims().log(&log).len(), 3);
    assert_eq!(
        bob.claims().fingerprint(&log),
        alice.claims().fingerprint(&log)
    );
}
