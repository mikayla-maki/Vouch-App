//! Media: claims pin bulk bytes by hash (`BlobRef`); the bytes live in a
//! content-addressed [`BlobStore`] and ride a different rail from claims.
//! Blob presence is cache, not convergent state — wants stand until bytes
//! arrive from any pipe, verified on arrival; redaction orphans bytes and
//! GC forgets them.

use vouch_core::{BlobRef, BlobStore, ClaimStore, Error, SignedEvent, Value, Writer};

fn photo_rec(db: &mut Writer, ts: i64, subject: &str, blob: &BlobRef) -> SignedEvent {
    db.claim(
        ts,
        Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text(subject)),
            ("photo", Value::BlobRef(blob.clone())),
        ]),
    )
    .unwrap()
}

fn blob_ref(bytes: &[u8], mime: &str) -> BlobRef {
    BlobRef {
        hash: BlobStore::new().put(bytes.to_vec()),
        size: bytes.len() as u64,
        mime: mime.into(),
    }
}

#[test]
fn claims_sync_now_blobs_heal_later() {
    // Alice authors a rec with a photo. A follower receives the CLAIM
    // immediately; the photo bytes are a want, satisfied later from any
    // pipe — and a lying pipe can't poison the cache.
    let mut alice = Writer::from_seed([1; 32]);
    let jpeg = b"definitely a jpeg".to_vec();

    let mut alice_blobs = BlobStore::new();
    let hash = alice_blobs.put(jpeg.clone());
    let r = blob_ref(&jpeg, "image/jpeg");
    assert_eq!(r.hash, hash);
    let rec = photo_rec(&mut alice, 100, "Joe's Pizza", &r);

    let mut store = ClaimStore::new();
    let mut blobs = BlobStore::new();
    store.ingest(rec.clone()).unwrap();

    // The claim is fully usable — text renders, size/mime are in the ref
    // for the placeholder — and the want-list names exactly the photo.
    assert!(store.contains(&rec.id()));
    let wants = store.missing_blobs(&blobs);
    assert_eq!(wants, vec![r.clone()]);

    // A bad pipe serves wrong bytes: refused, still wanted.
    let err = blobs.insert_verified(r.hash, b"malware.exe".to_vec());
    assert!(matches!(err, Err(Error::BlobHashMismatch)));
    assert_eq!(store.missing_blobs(&blobs).len(), 1);

    // The right bytes arrive (from anyone): verified, stored, want gone.
    assert!(blobs.insert_verified(r.hash, jpeg.clone()).unwrap());
    assert_eq!(blobs.get(&r.hash), Some(jpeg.as_slice()));
    assert!(store.missing_blobs(&blobs).is_empty());
}

#[test]
fn blob_refs_inside_embeds_are_wanted_too() {
    // Bob vouches for Alice's photo rec. Carol follows only Bob — and her
    // want-list still includes Alice's photo, because the embedded claim is
    // ingested as a claim of its own.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let jpeg = b"slice glamour shot".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");

    let rec = photo_rec(&mut alice, 100, "Joe's Pizza", &r);
    let vouch = bob
        .claim(
            200,
            Value::map([
                ("type", Value::text("vouch")),
                ("original", Value::Embed(Box::new(rec))),
            ]),
        )
        .unwrap();

    let mut carol = ClaimStore::new();
    let carol_blobs = BlobStore::new();
    carol.ingest(vouch).unwrap();
    assert_eq!(carol.missing_blobs(&carol_blobs), vec![r]);
}

#[test]
fn shared_blobs_dedup_and_die_only_with_their_last_referrer() {
    // Two claims pin the same image. One want, one stored copy; redacting
    // one claim keeps the bytes alive, redacting both orphans them and GC
    // forgets them.
    let mut alice = Writer::from_seed([1; 32]);
    let jpeg = b"the one good photo".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");

    let first = photo_rec(&mut alice, 100, "from the patio", &r);
    let second = photo_rec(&mut alice, 200, "same place, revisited", &r);

    let mut store = ClaimStore::new();
    let mut blobs = BlobStore::new();
    store.ingest(first.clone()).unwrap();
    store.ingest(second.clone()).unwrap();
    assert_eq!(store.missing_blobs(&blobs).len(), 1); // deduped
    assert_eq!(store.blob_referrers(&r.hash).count(), 2);

    blobs.insert_verified(r.hash, jpeg).unwrap();

    let redact_first = alice
        .claim(
            300,
            Value::map([
                ("type", Value::text("redact")),
                (
                    "redacts",
                    Value::ClaimRef(vouch_core::ClaimRef {
                        log_id: alice.id(),
                        hash: first.id(),
                    }),
                ),
            ]),
        )
        .unwrap();
    store.ingest(redact_first).unwrap();
    assert_eq!(store.blob_referrers(&r.hash).count(), 1);
    assert!(blobs.gc(&store).is_empty()); // still referenced: kept
    assert!(blobs.contains(&r.hash));

    let redact_second = alice
        .claim(
            400,
            Value::map([
                ("type", Value::text("redact")),
                (
                    "redacts",
                    Value::ClaimRef(vouch_core::ClaimRef {
                        log_id: alice.id(),
                        hash: second.id(),
                    }),
                ),
            ]),
        )
        .unwrap();
    store.ingest(redact_second).unwrap();
    assert_eq!(store.blob_referrers(&r.hash).count(), 0);
    assert_eq!(blobs.gc(&store), vec![r.hash]); // orphaned: forgotten
    assert!(!blobs.contains(&r.hash));
    // The bytes are gone, but the want does NOT come back: no live body
    // references the blob, so it isn't missing — it's deleted.
    assert!(store.missing_blobs(&blobs).is_empty());
}

#[test]
fn blob_presence_is_not_convergent_state() {
    // Two stores hold the same claims; only one holds the photo bytes.
    // Claim-level state and fingerprints must compare equal — blob presence
    // is local cache, reconciled by fetching, not by the sync protocol.
    let mut alice = Writer::from_seed([1; 32]);
    let jpeg = b"photo".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");
    let rec = photo_rec(&mut alice, 100, "Joe's Pizza", &r);

    let mut a = ClaimStore::new();
    let mut a_blobs = BlobStore::new();
    a.ingest(rec.clone()).unwrap();
    a_blobs.insert_verified(r.hash, jpeg).unwrap();

    let mut b = ClaimStore::new();
    let b_blobs = BlobStore::new();
    b.ingest(rec).unwrap();

    assert_eq!(a.state_vector(), b.state_vector());
    assert_eq!(a.fingerprint(&alice.id()), b.fingerprint(&alice.id()));
    assert!(a.missing_blobs(&a_blobs).is_empty());
    assert_eq!(b.missing_blobs(&b_blobs).len(), 1);
}
