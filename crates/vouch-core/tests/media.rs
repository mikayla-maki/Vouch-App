//! Media: claims pin bulk bytes by hash (`BlobRef`); the bytes live in a
//! content-addressed blob store and ride a different rail from claims.
//! Blob presence is cache, not convergent state — wants stand until bytes
//! arrive from any pipe, verified on arrival; redaction orphans bytes and
//! GC forgets them. These tests run [`BlobStore`] (the verification
//! logic) over the memory backend — the same logic any backend gets.

use vouch_core::{BlobRef, BlobStore, ClaimStore, Error, Event, Value, Writer};

fn photo_rec(db: &mut Writer, at: i64, subject: &str, blob: &BlobRef) -> Event {
    db.claim(Value::map([
        ("type", Value::text("rec")),
        ("at", Value::Int(at)),
        ("subject", Value::text(subject)),
        ("photo", Value::BlobRef(blob.clone())),
    ]))
    .unwrap()
}

fn blob_ref(bytes: &[u8], mime: &str) -> BlobRef {
    BlobRef {
        hash: BlobStore::new().put(bytes.to_vec()).unwrap(),
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
    let hash = alice_blobs.put(jpeg.clone()).unwrap();
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
    assert_eq!(blobs.get(&r.hash), Some(jpeg));
    assert!(store.missing_blobs(&blobs).is_empty());
}

#[test]
fn blob_refs_inside_embeds_are_wanted_too() {
    // Bob vouches for Alice's photo rec. Carol follows only Bob — and her
    // want-list still includes Alice's photo, because a quote that shows a
    // photo pins it: edges collect THROUGH the embed, attributed to Bob's
    // vouch. That attribution is what makes the want satisfiable — Carol's
    // pipes serve Bob's log, and the referrer lives there.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let jpeg = b"slice glamour shot".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");

    let rec = photo_rec(&mut alice, 100, "Joe's Pizza", &r);
    let vouch = bob
        .claim(Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(rec))),
        ]))
        .unwrap();

    let mut carol = ClaimStore::new();
    let carol_blobs = BlobStore::new();
    carol.ingest(vouch.clone()).unwrap();
    assert_eq!(carol.missing_blobs(&carol_blobs), vec![r.clone()]);
    assert_eq!(
        carol.blob_referrers(&r.hash),
        vec![vouch.id()],
        "the QUOTE is the referrer — the photo routes wherever bob's log goes"
    );
}

#[test]
fn redacting_a_quote_unpins_its_media() {
    // Bob's vouch quotes Alice's photo rec; the quote is the photo's only
    // referrer here. While it lives, gc refuses the bytes. Bob redacts his
    // vouch: quote, interior, and pin all die together — the bytes orphan
    // and gc forgets them. Nothing needed cleaning up by hand.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let jpeg = b"borrowed glamour shot".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");

    let rec = photo_rec(&mut alice, 100, "Joe's Pizza", &r);
    let vouch = bob
        .claim(Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(rec))),
        ]))
        .unwrap();

    let mut store = ClaimStore::new();
    let mut blobs = BlobStore::new();
    store.ingest(vouch.clone()).unwrap();
    assert!(blobs.insert_verified(r.hash, jpeg).unwrap());
    assert!(
        blobs.gc(&store).unwrap().is_empty(),
        "a live quote pins its photo"
    );

    let redact = bob
        .claim(Value::map([
            ("type", Value::text("redact")),
            (
                "redacts",
                Value::ClaimRef(vouch_core::ClaimRef {
                    log_id: vouch.header().unwrap().log_id,
                    hash: vouch.id(),
                }),
            ),
        ]))
        .unwrap();
    store.ingest(redact).unwrap();

    assert!(store.blob_referrers(&r.hash).is_empty());
    assert_eq!(blobs.gc(&store).unwrap(), vec![r.hash]);
    assert!(blobs.get(&r.hash).is_none());
    assert!(store.verify_integrity().is_empty());
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
    assert_eq!(store.blob_referrers(&r.hash).len(), 2);

    blobs.insert_verified(r.hash, jpeg).unwrap();

    let redact_first = alice
        .claim(Value::map([
            ("type", Value::text("redact")),
            (
                "redacts",
                Value::ClaimRef(vouch_core::ClaimRef {
                    log_id: alice.id(),
                    hash: first.id(),
                }),
            ),
        ]))
        .unwrap();
    let report = store.ingest(redact_first).unwrap();
    assert_eq!(report.redactions_applied, 1);
    assert_eq!(store.blob_referrers(&r.hash).len(), 1);
    assert!(blobs.gc(&store).unwrap().is_empty()); // still referenced
    assert!(blobs.contains(&r.hash));

    let redact_second = alice
        .claim(Value::map([
            ("type", Value::text("redact")),
            (
                "redacts",
                Value::ClaimRef(vouch_core::ClaimRef {
                    log_id: alice.id(),
                    hash: second.id(),
                }),
            ),
        ]))
        .unwrap();
    store.ingest(redact_second).unwrap();
    assert_eq!(store.blob_referrers(&r.hash).len(), 0);
    assert_eq!(blobs.gc(&store).unwrap(), vec![r.hash]); // orphaned
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

#[test]
fn eviction_is_cache_management_not_deletion() {
    // Evict drops bytes a live claim still pins — the opposite contract
    // from GC, which refuses to touch exactly those.
    let mut alice = Writer::from_seed([9; 32]);
    let jpeg = b"seasonal menu photo".to_vec();
    let r = blob_ref(&jpeg, "image/jpeg");
    let rec = photo_rec(&mut alice, 1, "Joe's Pizza", &r);

    let mut claims = ClaimStore::new();
    let mut blobs = BlobStore::new();
    claims.ingest(rec).unwrap();
    blobs.insert_verified(r.hash, jpeg.clone()).unwrap();

    // GC will not remove it: it's referenced.
    assert!(blobs.gc(&claims).unwrap().is_empty());

    // Eviction will: that's the point. The referrer index — claim state —
    // is untouched, so the want re-derives instantly.
    assert!(blobs.evict(&r.hash).unwrap());
    assert!(!blobs.contains(&r.hash));
    assert_eq!(claims.blob_referrers(&r.hash).len(), 1);
    assert_eq!(claims.missing_blobs(&blobs).len(), 1);

    // Healing is the ordinary fill-in path, and idempotent.
    assert!(blobs.insert_verified(r.hash, jpeg.clone()).unwrap());
    assert!(claims.missing_blobs(&blobs).is_empty());
    // Evicting what isn't there reports false, errors never.
    assert!(blobs.evict(&r.hash).unwrap());
    assert!(!blobs.evict(&r.hash).unwrap());
}
