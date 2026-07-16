//! Multi-client simulation: several writers, several databases, events
//! exchanged in arbitrary orders. This is the engine's contract under test —
//! convergence, cross-path dedup, embed verification — with no I/O
//! anywhere. Two [`Database`]s exchanging `serve_since` streams are a
//! complete sync session.

use vouch_core::{ClaimHash, ClaimRef, Database, SignedEvent, Value, Writer};

/// Tiny deterministic PRNG (xorshift64*) so shuffles are reproducible
/// without a rand dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = (self.next() % (i as u64 + 1)) as usize;
            items.swap(i, j);
        }
    }
}

fn rec_body(at: i64, subject: &str) -> Value {
    Value::map([
        ("type", Value::text("rec")),
        ("at", Value::Int(at)),
        ("subject", Value::text(subject)),
        ("body", Value::text(format!("{subject} is great"))),
    ])
}

fn rec(db: &mut Writer, at: i64, subject: &str) -> SignedEvent {
    db.claim(rec_body(at, subject)).unwrap()
}

fn vouch(db: &mut Writer, at: i64, original: &SignedEvent) -> SignedEvent {
    db.claim(Value::map([
        ("type", Value::text("vouch")),
        ("at", Value::Int(at)),
        ("original", Value::Embed(Box::new(original.clone()))),
        ("body", Value::text("seconded!")),
    ]))
    .unwrap()
}

fn cref(event: &SignedEvent) -> ClaimRef {
    ClaimRef {
        log_id: event.header().unwrap().log_id,
        hash: event.id(),
    }
}

fn id_of(event: &SignedEvent) -> ClaimHash {
    event.id()
}

#[test]
fn vouch_chain_verifies_without_subscribing_to_the_source() {
    // Alice writes a rec. Bob vouches for it. Carol subscribes ONLY to Bob.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let bob_vouch = vouch(&mut bob, 200, &alice_rec);

    let mut carol = Database::new();
    let report = carol.ingest(bob_vouch.clone()).unwrap();

    // Carol's store gains exactly one row: Bob's vouch. Alice's rec is
    // content INSIDE it — verified (signature, body hash) at the walk,
    // never extracted into a row of its own.
    assert_eq!(report.newly_stored, Some(id_of(&bob_vouch)));
    assert_eq!(report.skipped_embeds, 0);

    let alice_id = id_of(&alice_rec);
    assert!(
        carol.claims().get(&alice_id).is_none(),
        "a quote is content, not a row"
    );

    // The quote reads by recursion: the verified embedded claim, with
    // Alice's authorship intact.
    let stored = carol.claims().get(&id_of(&bob_vouch)).unwrap();
    let embeds = stored.embeds();
    assert_eq!(embeds.len(), 1);
    let (_, quoted) = &embeds[0];
    assert_eq!(quoted.header.id(), alice_id);
    assert_eq!(
        quoted.header.log_id,
        alice_rec.header().unwrap().log_id,
        "the quoted claim verifies as ALICE's speech"
    );

    // And the quote IS a reference: "who quotes Alice's rec" answers
    // through the backlink index, attributed to Bob's vouch.
    assert_eq!(carol.claims().backlinks(&alice_id), vec![id_of(&bob_vouch)]);

    // Later, Carol subscribes to Alice directly: the rec arrives as its own
    // top-level event and stores as its own row, independent of the quote.
    let report = carol.ingest(alice_rec).unwrap();
    assert_eq!(report.newly_stored, Some(alice_id));
    assert!(carol.claims().contains(&alice_id));
}

#[test]
fn tampered_embed_is_skipped_but_recorded() {
    let mut alice = Writer::from_seed([1; 32]);
    let mut mallory = Writer::from_seed([6; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");

    // Mallory alters Alice's rec body inside her vouch ("Moe's" forgery).
    // The body no longer matches the signed header's body hash.
    let mut forged = alice_rec.clone();
    let body = forged.body_bytes.as_mut().unwrap();
    let pos = body
        .windows(3)
        .position(|w| w == b"Joe")
        .expect("subject text is in the body bytes");
    body[pos] = b'M';
    let mallory_vouch = vouch(&mut mallory, 200, &forged);

    let mut db = Database::new();
    let report = db.ingest(mallory_vouch.clone()).unwrap();

    // Mallory's own claim is hers and stores fine; the forgery inside is
    // recorded (she signed it), but it is not an edge and will not render.
    assert_eq!(report.newly_stored, Some(id_of(&mallory_vouch)));
    assert_eq!(report.skipped_embeds, 1);
    assert!(db.claims().contains(&id_of(&mallory_vouch)));
    assert!(!db.claims().contains(&id_of(&alice_rec)));

    // Not an edge: no backlink to the forged claim's id, and the embed
    // accessor verifies and omits it — the UI never sees the forgery.
    let stored = db.claims().get(&id_of(&mallory_vouch)).unwrap();
    assert!(stored.refs.is_empty(), "a forged embed is not a reference");
    assert!(stored.embeds().is_empty(), "a forged embed never renders");
}

#[test]
fn shuffled_replay_converges() {
    // Three writers with cross-referencing claims, including vouches.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let mut carol = Writer::from_seed([3; 32]);

    let mut events: Vec<SignedEvent> = Vec::new();
    for i in 0..8 {
        events.push(rec(&mut alice, 1000 + i, &format!("place-{i}")));
    }
    // Bob vouches some of Alice's recs and writes an entity + rec of his own.
    let bob_entity = bob
        .claim(Value::map([
            ("type", Value::text("entity")),
            ("at", Value::Int(2000)),
            ("name", Value::text("Joe's Pizza")),
        ]))
        .unwrap();
    let entity_ref = cref(&bob_entity);
    events.push(bob_entity.clone());
    events.push(
        bob.claim(Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(2001)),
            ("about", Value::ClaimRef(entity_ref)),
            ("body", Value::text("best slice in town")),
        ]))
        .unwrap(),
    );
    events.push(vouch(&mut bob, 2002, &events[0]));
    events.push(vouch(&mut bob, 2003, &events[3]));
    // Carol disavows one of Alice's recs (dangling until it arrives).
    events.push(
        carol
            .claim(Value::map([
                ("type", Value::text("disavowal")),
                ("at", Value::Int(3000)),
                ("disavows", Value::ClaimRef(cref(&events[5]))),
                ("body", Value::text("closed down last year")),
            ]))
            .unwrap(),
    );

    // Ingest the same event set in several shuffled orders.
    let mut reference: Option<_> = None;
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    for _ in 0..10 {
        let mut order = events.clone();
        rng.shuffle(&mut order);
        let mut db = Database::new();
        for event in order {
            db.ingest(event).unwrap();
        }
        let state = db.claims().state_vector();
        match &reference {
            None => reference = Some(state),
            Some(r) => assert_eq!(&state, r, "databases diverged under reordering"),
        }
    }
}

#[test]
fn dangling_backlinks_heal_when_the_target_arrives() {
    let mut alice = Writer::from_seed([1; 32]);
    let mut carol = Writer::from_seed([3; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let target = id_of(&alice_rec);
    let disavowal = carol
        .claim(Value::map([
            ("type", Value::text("disavowal")),
            ("disavows", Value::ClaimRef(cref(&alice_rec))),
        ]))
        .unwrap();

    let mut db = Database::new();
    // Disavowal arrives FIRST: the edge exists, the target doesn't.
    db.ingest(disavowal.clone()).unwrap();
    assert_eq!(db.claims().backlinks(&target).len(), 1);
    assert!(!db.claims().contains(&target));

    // Target arrives: the same query now resolves end to end.
    db.ingest(alice_rec).unwrap();
    assert!(db.claims().contains(&target));
    assert_eq!(db.claims().backlinks(&target), vec![id_of(&disavowal)]);
}

#[test]
fn same_seed_writers_collide_harmlessly() {
    // The old fork scenario: one identity, two devices, both writing at
    // once. A writer carries no position at all — there is nothing to
    // restore after a crash and nothing for two devices to collide on.
    // Just two different claims in one log, in any order.
    let mut a1 = Writer::from_seed([8; 32]);
    let mut a2 = Writer::from_seed([8; 32]);
    assert_eq!(a1.id(), a2.id());

    let first = rec(&mut a1, 100, "version one");
    let second = rec(&mut a2, 100, "version two");
    assert_ne!(id_of(&first), id_of(&second));

    let mut ab = Database::new();
    ab.ingest(first.clone()).unwrap();
    ab.ingest(second.clone()).unwrap();
    let mut ba = Database::new();
    ba.ingest(second).unwrap();
    ba.ingest(first.clone()).unwrap();

    assert_eq!(ab.claims().len(), 2);
    assert_eq!(ab.claims().state_vector(), ba.claims().state_vector());
    assert_eq!(
        ab.claims().fingerprint(&a1.id()),
        ba.claims().fingerprint(&a1.id())
    );
    assert_eq!(ab.claims().log(&a1.id()).len(), 2);
}

#[test]
fn fingerprint_flags_silent_divergence_that_cursors_cannot_see() {
    // Cursors are pipe-local arrival counts, so an AUTHOR can no longer
    // collide on numbering — but a RELAY restored from a backup can reuse
    // arrival positions. Alice publishes c1..c6 to a relay; a client syncs
    // all six (cursor = 6). The relay dies and is restored from a backup
    // holding only c1..c5; Alice then publishes c7 and c8, which land at
    // arrivals 5 and 6 on the restored relay. The client pulls "I have 6"
    // and receives only c8 — and now both sides hold seven claims, the
    // cursor equals the relay's count, every cursor-shaped signal agrees —
    // yet the client holds c6 (which the relay lost) and is missing c7.
    // The fingerprint catches it.
    let mut alice = Writer::from_seed([1; 32]);
    let log = alice.id();
    let claims: Vec<SignedEvent> = (0..8)
        .map(|i| rec(&mut alice, 1000 + i, &format!("place-{i}")))
        .collect();

    let mut relay = Database::new();
    for c in &claims[..6] {
        relay.ingest(c.clone()).unwrap();
    }
    let mut client = Database::new();
    for e in relay.claims().serve_since(&log, 0) {
        client.ingest(e).unwrap();
    }
    let mut cursor = client.claims().log_len(&log);
    assert_eq!(cursor, 6);

    // The relay is restored from a stale backup (c6 lost), then receives
    // Alice's two new claims at the recycled arrival positions.
    let mut relay = Database::new();
    for c in &claims[..5] {
        relay.ingest(c.clone()).unwrap();
    }
    relay.ingest(claims[6].clone()).unwrap();
    relay.ingest(claims[7].clone()).unwrap();

    // Fast path: the client pulls past its cursor and gets ONE claim (c8).
    let served = relay.claims().serve_since(&log, cursor);
    assert_eq!(served.len(), 1);
    for e in served {
        client.ingest(e).unwrap();
    }
    cursor += 1;

    // Every cursor-shaped signal says the two agree...
    assert_eq!(relay.claims().log_len(&log), 7);
    assert_eq!(client.claims().log_len(&log), 7);
    assert_eq!(cursor, relay.claims().log_len(&log));
    // ...the fingerprint says they don't.
    assert_ne!(
        relay.claims().fingerprint(&log),
        client.claims().fingerprint(&log)
    );

    // Mismatch triggers full reconciliation (here: replay everything both
    // ways), after which the fingerprints — and the state — agree.
    for e in relay.claims().serve_since(&log, 0) {
        client.ingest(e).unwrap();
    }
    for e in client.claims().serve_since(&log, 0) {
        relay.ingest(e).unwrap();
    }
    assert_eq!(
        relay.claims().fingerprint(&log),
        client.claims().fingerprint(&log)
    );
    assert_eq!(
        relay.claims().state_vector(),
        client.claims().state_vector()
    );
    assert_eq!(relay.claims().log(&log).len(), 8);
}

#[test]
fn one_claim_many_endorsements_share_one_identity() {
    // Alice's rec reaches a reader via three paths: directly, and quoted in
    // two different vouches. Rows are top-level events only — the quotes
    // carry copies as content — but identity is the content hash, so all
    // three paths agree on WHICH claim is being endorsed: the backlink
    // index answers "who vouches for Alice's rec" across both quotes.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let mut dana = Writer::from_seed([4; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let alice_id = id_of(&alice_rec);

    let mut db = Database::new();
    let bob_vouch = vouch(&mut bob, 200, &alice_rec);
    let dana_vouch = vouch(&mut dana, 300, &alice_rec);
    db.ingest(bob_vouch.clone()).unwrap();
    db.ingest(dana_vouch.clone()).unwrap();
    db.ingest(alice_rec).unwrap();

    // Three rows: the two vouches and Alice's own rec.
    assert_eq!(db.claims().len(), 3);
    assert_eq!(db.claims().log(&alice.id()).len(), 1);
    assert_eq!(db.claims().by_type("vouch").len(), 2);

    // Both quotes backlink to the same identity, and each renders its own
    // verified copy of the same content.
    let mut endorsers = db.claims().backlinks(&alice_id);
    endorsers.sort();
    let mut expected = vec![id_of(&bob_vouch), id_of(&dana_vouch)];
    expected.sort();
    assert_eq!(endorsers, expected);
    for vid in &expected {
        let embeds = db.claims().get(vid).unwrap().embeds();
        assert_eq!(embeds[0].1.header.id(), alice_id);
    }
}

#[test]
fn absurd_embed_nesting_is_contained_not_fatal() {
    // One writer russian-dolls 80 vouches around a rec — past the 64-deep
    // cap (itself generous headroom over any chain humanity could produce).
    // Ingest succeeds: one row (the top-level event), the quote chain
    // indexed down to the depth cap, and one skipped embed where the walk
    // stopped descending — never an error, never a partially-failed ingest.
    let mut writer = Writer::from_seed([7; 32]);
    let mut event = rec(&mut writer, 0, "the bottom");
    for i in 0..80 {
        event = vouch(&mut writer, i, &event);
    }

    let mut db = Database::new();
    let report = db.ingest(event.clone()).unwrap();
    assert!(db.claims().contains(&id_of(&event)));
    assert_eq!(report.newly_stored, Some(id_of(&event)));
    assert_eq!(report.skipped_embeds, 1);
    // One edge per layer the walk entered: MAX_EMBED_DEPTH quote edges.
    let stored = db.claims().get(&id_of(&event)).unwrap();
    assert_eq!(stored.refs.len(), vouch_core::MAX_EMBED_DEPTH);
}

#[test]
fn quotes_are_content_not_rows() {
    // Database A learns Alice's rec only as a quote inside Bob's vouch.
    // Database B subscribes to Alice too and holds the rec as a row. The
    // store's rows are exactly the top-level events its logs delivered —
    // a quote changes nothing about what A "has" for sync purposes.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let bob_vouch = vouch(&mut bob, 200, &alice_rec);

    let mut a = Database::new();
    a.ingest(bob_vouch.clone()).unwrap();

    let mut b = Database::new();
    b.ingest(bob_vouch.clone()).unwrap();
    b.ingest(alice_rec.clone()).unwrap();

    // A holds one row and no trace of Alice's log; B holds two rows.
    assert!(a.claims().get(&id_of(&alice_rec)).is_none());
    assert_eq!(a.claims().log_len(&alice.id()), 0);
    assert_eq!(a.claims().fingerprint(&alice.id()), [0u8; 32]);
    assert!(b.claims().contains(&id_of(&alice_rec)));

    // Yet both READ the same content for the quote — A through Bob's
    // vouch, B either way.
    let a_quote = a.claims().get(&id_of(&bob_vouch)).unwrap().embeds();
    assert_eq!(a_quote[0].1.header.id(), id_of(&alice_rec));

    // And on Bob's log — the one they both follow — they fully agree.
    assert_eq!(
        a.claims().fingerprint(&bob.id()),
        b.claims().fingerprint(&bob.id())
    );
}

#[test]
fn log_hashes_reports_ids_and_body_bits_in_canonical_order() {
    let mut alice = Writer::from_seed([40; 32]);
    let log = alice.id();
    let full = rec(&mut alice, 1, "with body");
    let stripped = rec(&mut alice, 2, "body withheld");

    let mut db = Database::new();
    db.ingest(full.clone()).unwrap();
    db.ingest(stripped.without_body()).unwrap();

    let mut expected = vec![(full.id(), true), (stripped.id(), false)];
    expected.sort_by_key(|(id, _)| *id);
    assert_eq!(db.claims().log_hashes(&log), expected);
    // The list is the reconciliation view of "what do you hold": same set,
    // different bodies → different lists, exactly like the fingerprint.
    let mut other = Database::new();
    other.ingest(full).unwrap();
    other.ingest(stripped).unwrap();
    assert_ne!(
        other.claims().log_hashes(&log),
        db.claims().log_hashes(&log)
    );
}
