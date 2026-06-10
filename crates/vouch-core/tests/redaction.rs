//! Cooperative deletion: `redact` claims drop bodies from conformant
//! stores, leaving signed tombstones (header + signature, body gone) so
//! cursors, backfill, and verification stay coherent. Redaction is monotone
//! and converges under any arrival order; mere body *absence* (a lossy or
//! hostile peer stripping content) is recoverable, because only a signed
//! redact claim makes it permanent.

use vouch_core::{ClaimHash, ClaimRef, ClaimStore, SignedEvent, Value, Writer};

fn rec(db: &mut Writer, ts: i64, subject: &str) -> SignedEvent {
    db.claim(
        ts,
        Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text(subject)),
        ]),
    )
    .unwrap()
}

fn redact(db: &mut Writer, ts: i64, target: &SignedEvent) -> SignedEvent {
    db.claim(
        ts,
        Value::map([
            ("type", Value::text("redact")),
            ("redacts", Value::ClaimRef(cref(target))),
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
fn redact_drops_body_keeps_signed_tombstone_and_cursor() {
    let mut alice = Writer::from_seed([1; 32]);
    let mut carol = Writer::from_seed([3; 32]);

    let regret = rec(&mut alice, 100, "place I should not have reviewed");
    let target = id_of(&regret);
    // The regretted rec links nowhere, but a third party links TO it.
    let disavowal = carol
        .claim(
            150,
            Value::map([
                ("type", Value::text("disavowal")),
                ("disavows", Value::ClaimRef(cref(&regret))),
            ]),
        )
        .unwrap();
    let redaction = redact(&mut alice, 200, &regret);
    let redaction_id = id_of(&redaction);

    let mut store = ClaimStore::new();
    store.ingest(regret).unwrap();
    store.ingest(disavowal.clone()).unwrap();
    store.ingest(redaction).unwrap();

    // Body gone, signed tombstone present and independently verifiable,
    // advisory cursor not regressed.
    assert!(!store.contains(&target));
    let tomb = store.get(&target).expect("tombstone remains");
    assert!(tomb.body.is_none());
    assert!(tomb.signed.body_bytes.is_none());
    tomb.signed.verify().expect("tombstone still verifies");
    assert_eq!(store.redaction(&target), Some(redaction_id));
    assert_eq!(store.max_sequence(&alice.id()), Some(2));
    // The log shows only the redact claim; the timeline never shows the
    // tombstone.
    assert_eq!(store.log(&alice.id()).len(), 1);
    assert!(store.timeline().iter().all(|c| c.signed.id() != target));
    // Backlinks TO the tombstone survive (the disavowal still points there);
    // that's history about the claim, not content of it.
    assert_eq!(store.backlinks(&target).count(), 2); // disavowal + redact claim
    assert!(store.contains(&id_of(&disavowal)));
}

#[test]
fn redaction_before_content_preempts_it() {
    let mut alice = Writer::from_seed([1; 32]);

    let regret = rec(&mut alice, 100, "oops");
    let target = id_of(&regret);
    let redaction = redact(&mut alice, 200, &regret);

    // Redaction arrives FIRST. When the content shows up, only its header
    // is kept — the body is suppressed on arrival.
    let mut store = ClaimStore::new();
    store.ingest(redaction.clone()).unwrap();
    let report = store.ingest(regret.clone()).unwrap();
    assert_eq!(report.redacted_skips, 1);
    assert!(!store.contains(&target));
    assert!(store.get(&target).unwrap().body.is_none());

    // Both orders converge.
    let mut other = ClaimStore::new();
    other.ingest(regret).unwrap();
    other.ingest(redaction).unwrap();
    assert_eq!(store.state_vector(), other.state_vector());
}

#[test]
fn serve_since_serves_signed_tombstones_and_backfill_converges() {
    let mut alice = Writer::from_seed([1; 32]);

    let keep = rec(&mut alice, 100, "still good"); // seq 1
    let regret = rec(&mut alice, 110, "oops"); // seq 2
    let redaction = redact(&mut alice, 200, &regret); // seq 3

    let mut store = ClaimStore::new();
    for e in [keep.clone(), regret.clone(), redaction.clone()] {
        store.ingest(e).unwrap();
    }

    // The serve stream contains the live claims and, in place of the
    // redacted one, its signed tombstone — never the redacted body.
    let served: Vec<SignedEvent> = store
        .serve_since(&alice.id(), 0)
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(served.len(), 3);
    assert_eq!(served[0], keep);
    assert_eq!(served[1], regret.without_body());
    assert_eq!(served[2], redaction);

    // Tombstones are ordinary ingestible events: a backfiller applying the
    // served stream converges to the server's exact state.
    let mut backfiller = ClaimStore::new();
    for e in served {
        backfiller.ingest(e).unwrap();
    }
    assert_eq!(backfiller.state_vector(), store.state_vector());
}

#[test]
fn body_stripping_by_a_peer_is_recoverable() {
    // A lossy or hostile peer serves a claim without its body. That is NOT
    // a redaction: the body heals from any other pipe. Only the author's
    // signed redact claim makes bodilessness permanent.
    let mut alice = Writer::from_seed([1; 32]);
    let full = rec(&mut alice, 100, "Joe's Pizza");
    let id = id_of(&full);

    let mut store = ClaimStore::new();
    store.ingest(full.without_body()).unwrap();
    assert!(!store.contains(&id));
    assert_eq!(store.redaction(&id), None); // absence, not redaction

    let report = store.ingest(full.clone()).unwrap();
    assert_eq!(report.bodies_attached, 1);
    assert!(store.contains(&id));

    // Convergence: header-only and full copies in either order end the same.
    let mut other = ClaimStore::new();
    other.ingest(full.clone()).unwrap();
    other.ingest(full.without_body()).unwrap();
    assert_eq!(store.state_vector(), other.state_vector());
}

#[test]
fn non_author_redaction_is_mere_speech() {
    let mut alice = Writer::from_seed([1; 32]);
    let mut mallory = Writer::from_seed([6; 32]);

    let alice_rec = rec(&mut alice, 100, "Joe's Pizza");
    let target = id_of(&alice_rec);
    // Mallory "redacts" Alice's claim. It's a validly-signed claim in
    // Mallory's log — stored like anything she says — but the engine gives
    // it no effect on Alice's claim.
    let bogus = redact(&mut mallory, 200, &alice_rec);

    let mut store = ClaimStore::new();
    store.ingest(alice_rec).unwrap();
    store.ingest(bogus.clone()).unwrap();

    assert!(store.contains(&target));
    assert_eq!(store.redaction(&target), None);
    assert!(store.contains(&id_of(&bogus)));
}

#[test]
fn redaction_is_monotone_under_any_interleaving() {
    let mut alice = Writer::from_seed([1; 32]);
    let content = rec(&mut alice, 100, "oops"); // seq 1
    let target = id_of(&content);
    let bystander = rec(&mut alice, 110, "fine"); // seq 2
    let redaction = redact(&mut alice, 200, &content); // seq 3

    let events = [content, bystander.clone(), redaction];
    let orders: &[[usize; 3]] = &[
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut reference: Option<_> = None;
    for order in orders {
        let mut store = ClaimStore::new();
        for &i in order {
            store.ingest(events[i].clone()).unwrap();
        }
        assert!(!store.contains(&target), "body survived order {order:?}");
        assert!(store.redaction(&target).is_some());
        assert!(store.contains(&id_of(&bystander)));
        let state = store.state_vector();
        match &reference {
            None => reference = Some(state),
            Some(r) => assert_eq!(&state, r, "diverged under order {order:?}"),
        }
    }
}

#[test]
fn redacting_the_redaction_does_not_resurrect() {
    // (1) content, (2) redacts it, (3) redacts the redaction. The inner
    // redaction's effect must survive every arrival order — applying a
    // redaction happens whenever its body is *seen*, even if that body is
    // itself about to be suppressed. Chained redaction hides reasons, never
    // restores content.
    let mut alice = Writer::from_seed([1; 32]);

    let content = rec(&mut alice, 100, "deeply regretted");
    let target = id_of(&content);
    let redaction = redact(&mut alice, 200, &content);
    let meta = redact(&mut alice, 300, &redaction);

    let events = [content, redaction.clone(), meta];
    let orders: &[[usize; 3]] = &[
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];
    let mut reference: Option<_> = None;
    for order in orders {
        let mut store = ClaimStore::new();
        for &i in order {
            store.ingest(events[i].clone()).unwrap();
        }
        assert!(!store.contains(&target), "content survived order {order:?}");
        assert!(store.redaction(&target).is_some());
        assert!(!store.contains(&id_of(&redaction)));
        let state = store.state_vector();
        match &reference {
            None => reference = Some(state),
            Some(r) => assert_eq!(&state, r, "diverged under order {order:?}"),
        }
    }
}
