//! Multi-client simulation: several writers, several databases, events
//! exchanged in arbitrary orders. This is the engine's contract under test —
//! convergence, cross-path dedup, provenance verification — with no I/O
//! anywhere. Two [`Database`]s exchanging `serve_since` streams are a
//! complete sync session.

use vouch_core::{ClaimHash, ClaimRef, Database, Provenance, SignedEvent, Value, Writer};

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

fn rec_body(subject: &str) -> Value {
    Value::map([
        ("type", Value::text("rec")),
        ("subject", Value::text(subject)),
        ("body", Value::text(format!("{subject} is great"))),
    ])
}

fn rec(db: &mut Writer, ts: i64, subject: &str) -> SignedEvent {
    db.claim(ts, rec_body(subject)).unwrap()
}

fn vouch(db: &mut Writer, ts: i64, original: &SignedEvent) -> SignedEvent {
    db.claim(
        ts,
        Value::map([
            ("type", Value::text("vouch")),
            ("original", Value::Embed(Box::new(original.clone()))),
            ("body", Value::text("seconded!")),
        ]),
    )
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

    // Carol holds both claims: Bob's vouch (direct) and Alice's rec
    // (embedded), with Alice's signature verified along the way.
    assert_eq!(report.newly_stored.len(), 2);
    assert_eq!(report.rejected_embeds, 0);

    let alice_id = id_of(&alice_rec);
    let stored = carol
        .claims()
        .get(&alice_id)
        .expect("alice's rec is queryable");
    assert_eq!(stored.provenance, Provenance::Embedded);
    assert_eq!(
        carol.claims().get(&id_of(&bob_vouch)).unwrap().provenance,
        Provenance::Direct
    );

    // Later, Carol subscribes to Alice directly: the same artifact arrives,
    // dedup kicks in, provenance upgrades.
    let report = carol.ingest(alice_rec).unwrap();
    assert!(report.newly_stored.is_empty());
    assert_eq!(report.duplicates, 1);
    assert_eq!(
        carol.claims().get(&alice_id).unwrap().provenance,
        Provenance::Direct
    );
}

#[test]
fn tampered_embed_is_rejected_but_recorded() {
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

    // Mallory's own claim is hers and stores fine; the forgery does not.
    assert_eq!(report.newly_stored.len(), 1);
    assert_eq!(report.rejected_embeds, 1);
    assert!(db.claims().contains(&id_of(&mallory_vouch)));
    assert!(!db.claims().contains(&id_of(&alice_rec)));
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
        .claim(
            2000,
            Value::map([
                ("type", Value::text("entity")),
                ("name", Value::text("Joe's Pizza")),
            ]),
        )
        .unwrap();
    let entity_ref = cref(&bob_entity);
    events.push(bob_entity.clone());
    events.push(
        bob.claim(
            2001,
            Value::map([
                ("type", Value::text("rec")),
                ("about", Value::ClaimRef(entity_ref)),
                ("body", Value::text("best slice in town")),
            ]),
        )
        .unwrap(),
    );
    events.push(vouch(&mut bob, 2002, &events[0]));
    events.push(vouch(&mut bob, 2003, &events[3]));
    // Carol disavows one of Alice's recs (dangling until it arrives).
    events.push(
        carol
            .claim(
                3000,
                Value::map([
                    ("type", Value::text("disavowal")),
                    ("disavows", Value::ClaimRef(cref(&events[5]))),
                    ("body", Value::text("closed down last year")),
                ]),
            )
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
        .claim(
            200,
            Value::map([
                ("type", Value::text("disavowal")),
                ("disavows", Value::ClaimRef(cref(&alice_rec))),
            ]),
        )
        .unwrap();

    let mut db = Database::new();
    // Disavowal arrives FIRST: the edge exists, the target doesn't.
    db.ingest(disavowal.clone()).unwrap();
    assert_eq!(db.claims().backlinks(&target).count(), 1);
    assert!(!db.claims().contains(&target));

    // Target arrives: the same query now resolves end to end.
    db.ingest(alice_rec).unwrap();
    assert!(db.claims().contains(&target));
    let disavowers: Vec<_> = db.claims().backlinks(&target).collect();
    assert_eq!(disavowers, vec![&id_of(&disavowal)]);
}

#[test]
fn same_seed_writers_collide_harmlessly() {
    // The old fork scenario: one identity, two devices, both write "claim 1"
    // with different content. Under content-addressed identity these are
    // simply two different claims that share an advisory sequence — both
    // store, nothing conflicts. Order doesn't matter.
    let mut a1 = Writer::from_seed([8; 32]);
    let mut a2 = Writer::from_seed([8; 32]);
    assert_eq!(a1.id(), a2.id());

    let first = rec(&mut a1, 100, "version one");
    let second = rec(&mut a2, 100, "version two");
    assert_ne!(id_of(&first), id_of(&second));
    assert_eq!(
        first.header().unwrap().sequence,
        second.header().unwrap().sequence
    );

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
    // Alice mints claims 1..=6 in her database and syncs them to a
    // follower. Then her device dies; she restores from a backup taken at
    // claim 5 and mints a DIFFERENT claim 6, then 7 and 8. The follower
    // catches up "since 6": it receives 7 and 8, and both sides now sit at
    // max_sequence 8 with equal claim counts — every cursor agrees — yet
    // the follower holds old-6 (which Alice lost) and is missing new-6
    // (which "since 6" will never resend). The fingerprint catches this.
    let mut alice = Database::new();
    let log = alice.add_writer(Writer::from_seed([1; 32]));
    let mut early: Vec<SignedEvent> = (0..6)
        .map(|i| alice.claim(&log, i, rec_body("original")).unwrap())
        .collect();
    let old_six = early.pop().unwrap();

    let mut follower = Database::new();
    for e in early.iter().chain([&old_six]) {
        follower.ingest(e.clone()).unwrap();
    }

    // Alice's device dies. She restores from the claim-5 backup: the five
    // early claims, plus a writer resumed at sequence 6 — old-6 is lost.
    let mut alice = Database::new();
    let log = alice.add_writer(Writer::resume([1; 32], 6));
    for e in &early {
        alice.ingest(e.clone()).unwrap();
    }
    let new_six = alice.claim(&log, 50, rec_body("rewritten")).unwrap();
    alice.claim(&log, 51, rec_body("seven")).unwrap();
    alice.claim(&log, 52, rec_body("eight")).unwrap();
    assert_eq!(
        old_six.header().unwrap().sequence,
        new_six.header().unwrap().sequence
    );

    // The fast path: follower pulls "since 6" and gets 7 and 8.
    let served: Vec<SignedEvent> = alice
        .claims()
        .serve_since(&log, 6)
        .into_iter()
        .cloned()
        .collect();
    for e in served {
        follower.ingest(e).unwrap();
    }

    // Every cursor-shaped signal says the databases agree...
    assert_eq!(follower.claims().max_sequence(&log), Some(8));
    assert_eq!(alice.claims().max_sequence(&log), Some(8));
    assert_eq!(follower.claims().len(), alice.claims().len());
    // ...the fingerprint says they don't.
    assert_ne!(
        follower.claims().fingerprint(&log),
        alice.claims().fingerprint(&log)
    );

    // Mismatch triggers full reconciliation (here: replay everything both
    // ways), after which the fingerprints — and the state — agree.
    let from_alice: Vec<SignedEvent> = alice
        .claims()
        .serve_since(&log, 0)
        .into_iter()
        .cloned()
        .collect();
    for e in from_alice {
        follower.ingest(e).unwrap();
    }
    let from_follower: Vec<SignedEvent> = follower
        .claims()
        .serve_since(&log, 0)
        .into_iter()
        .cloned()
        .collect();
    for e in from_follower {
        alice.ingest(e).unwrap();
    }
    assert_eq!(
        follower.claims().fingerprint(&log),
        alice.claims().fingerprint(&log)
    );
    assert_eq!(
        follower.claims().state_vector(),
        alice.claims().state_vector()
    );
    assert_eq!(follower.claims().log(&log).len(), 9); // 1..=5, both 6s, 7, 8
}

#[test]
fn cross_path_dedup_gives_one_claim_many_endorsements() {
    // Alice's rec reaches a reader via three paths: directly, and embedded
    // in two different vouches. Dedup is by content hash.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);
    let mut dana = Writer::from_seed([4; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let alice_id = id_of(&alice_rec);

    let mut db = Database::new();
    db.ingest(vouch(&mut bob, 200, &alice_rec)).unwrap();
    db.ingest(vouch(&mut dana, 300, &alice_rec)).unwrap();
    db.ingest(alice_rec).unwrap();

    // Three events referencing the same content → exactly one stored copy
    // of Alice's claim (plus the two vouches).
    assert_eq!(db.claims().len(), 3);
    assert_eq!(db.claims().log(&alice.id()).len(), 1);
    assert_eq!(
        db.claims().get(&alice_id).unwrap().provenance,
        Provenance::Direct
    );
    assert_eq!(db.claims().by_type("vouch").count(), 2);
}

#[test]
fn absurd_embed_nesting_is_contained_not_fatal() {
    // One writer russian-dolls 80 vouches around a rec — past the 64-deep
    // cap (itself generous headroom over any chain humanity could produce).
    // Ingest succeeds: the outer claims store down to the depth cap, the
    // rest is one rejected embed — never an error, never a partially-failed
    // ingest.
    let mut writer = Writer::from_seed([7; 32]);
    let mut event = rec(&mut writer, 0, "the bottom");
    for i in 0..80 {
        event = vouch(&mut writer, i, &event);
    }

    let mut db = Database::new();
    let report = db.ingest(event.clone()).unwrap();
    assert!(db.claims().contains(&id_of(&event)));
    assert_eq!(report.rejected_embeds, 1);
    assert_eq!(report.newly_stored.len(), 65); // depths 0..=64 inclusive
}

#[test]
fn local_metadata_is_not_convergent_state() {
    // Database A learns Alice's rec only via Bob's vouch (provenance:
    // Embedded). Database B subscribes to Alice too and gets it directly
    // (provenance: Direct). Sync exchanges claims by id, so this difference
    // can never be reconciled — which is exactly why it must not be part of
    // the convergent state. Substance (headers + bodies + redactions) is
    // equal, so the databases must compare equal.
    let mut alice = Writer::from_seed([1; 32]);
    let mut bob = Writer::from_seed([2; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let bob_vouch = vouch(&mut bob, 200, &alice_rec);

    let mut a = Database::new();
    a.ingest(bob_vouch.clone()).unwrap();

    let mut b = Database::new();
    b.ingest(bob_vouch).unwrap();
    b.ingest(alice_rec.clone()).unwrap();

    // The local views genuinely differ...
    assert_eq!(
        a.claims().get(&id_of(&alice_rec)).unwrap().provenance,
        Provenance::Embedded
    );
    assert_eq!(
        b.claims().get(&id_of(&alice_rec)).unwrap().provenance,
        Provenance::Direct
    );
    // ...but the convergent state does not.
    assert_eq!(a.claims().state_vector(), b.claims().state_vector());
}
