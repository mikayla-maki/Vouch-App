//! Cooperative deletion: `redact` claims drop bodies from conformant
//! stores, leaving signed tombstones (header + signature, body gone) so
//! cursors, backfill, and verification stay coherent. Redaction is monotone
//! and converges under any arrival order; mere body *absence* (a lossy or
//! hostile peer stripping content) is recoverable, because only a signed
//! redact claim makes it permanent.

use vouch_core::{ClaimHash, ClaimRef, ClaimStore, Event, Value, Writer};

fn rec(db: &mut Writer, at: i64, subject: &str) -> Event {
    db.claim(Value::map([
        ("type", Value::text("rec")),
        ("at", Value::Int(at)),
        ("subject", Value::text(subject)),
    ]))
    .unwrap()
}

fn redact(db: &mut Writer, at: i64, target: &Event) -> Event {
    db.claim(Value::map([
        ("type", Value::text("redact")),
        ("at", Value::Int(at)),
        ("redacts", Value::ClaimRef(cref(target))),
    ]))
    .unwrap()
}

fn vouch(db: &mut Writer, at: i64, original: &Event) -> Event {
    db.claim(Value::map([
        ("type", Value::text("vouch")),
        ("at", Value::Int(at)),
        ("original", Value::Embed(Box::new(original.clone()))),
    ]))
    .unwrap()
}

fn cref(event: &Event) -> ClaimRef {
    ClaimRef {
        log_id: event.header().unwrap().log_id,
        hash: event.id(),
    }
}

fn id_of(event: &Event) -> ClaimHash {
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
        .claim(Value::map([
            ("type", Value::text("disavowal")),
            ("disavows", Value::ClaimRef(cref(&regret))),
        ]))
        .unwrap();
    let redaction = redact(&mut alice, 200, &regret);
    let redaction_id = id_of(&redaction);

    let mut store = ClaimStore::new();
    store.ingest(regret).unwrap();
    store.ingest(disavowal.clone()).unwrap();
    store.ingest(redaction).unwrap();

    // Body gone, tombstone present and structurally sound,
    // advisory cursor not regressed.
    assert!(!store.contains(&target));
    let tomb = store.get(&target).expect("tombstone remains");
    assert!(tomb.body.is_none());
    assert!(tomb.event.body_bytes.is_none());
    tomb.event.check().expect("tombstone still checks out");
    assert_eq!(store.redaction(&target), Some(redaction_id));
    assert_eq!(store.log_len(&alice.id()), 2);
    // The log shows only the redact claim; the timeline never shows the
    // tombstone.
    assert_eq!(store.log(&alice.id()).len(), 1);
    assert!(store.timeline().iter().all(|c| c.event.id() != target));
    // Backlinks TO the tombstone survive (the disavowal still points there);
    // that's history about the claim, not content of it.
    assert_eq!(store.backlinks(&target).len(), 2); // disavowal + redact claim
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

    let keep = rec(&mut alice, 100, "still good"); // arrival 0
    let regret = rec(&mut alice, 110, "oops"); // arrival 1
    let redaction = redact(&mut alice, 200, &regret); // arrival 2

    let mut store = ClaimStore::new();
    for e in [keep.clone(), regret.clone(), redaction.clone()] {
        store.ingest(e).unwrap();
    }

    // The serve stream contains the live claims and, in place of the
    // redacted one, its signed tombstone — never the redacted body.
    let served: Vec<Event> = store.serve_since(&alice.id(), 0);
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
    let content = rec(&mut alice, 100, "oops"); // arrival 0
    let target = id_of(&content);
    let bystander = rec(&mut alice, 110, "fine"); // arrival 1
    let redaction = redact(&mut alice, 200, &content); // arrival 2

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
    // redaction's effect must survive every arrival order. A redact claim's
    // body is pure machinery (a hash pointer) and the sole carrier of the
    // fact it encodes, so redacting a redact does NOT drop its body — the
    // original stays redacted. Chained redaction never restores content.
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
        // The redact claim KEEPS its body (it's the carrier of target->R),
        // even though it is itself redacted.
        assert!(store.contains(&id_of(&redaction)));
        assert!(store.redaction(&id_of(&redaction)).is_some());
        let state = store.state_vector();
        match &reference {
            None => reference = Some(state),
            Some(r) => assert_eq!(&state, r, "diverged under order {order:?}"),
        }
    }
}

// ── Adversarial-review regressions ──────────────────────────────────────

#[test]
fn redacting_a_quote_takes_the_quoted_content_with_it() {
    // The merged-embed contract: a quote is part of the speech that carries
    // it — it was never a row, so redacting the container removes the quote,
    // interior and all, with nothing orphaned and nothing to clean up. Both
    // arrival orders converge on the same nothing.
    let mut bob = Writer::from_seed([2; 32]); // author of the quoted rec
    let mut alice = Writer::from_seed([1; 32]); // author of the vouch + redact

    let inner = rec(&mut bob, 50, "the original");
    let inner_id = id_of(&inner);
    let container = vouch(&mut alice, 100, &inner); // quotes bob's rec
    let container_id = id_of(&container);
    let redaction = redact(&mut alice, 200, &container);

    let mut a = ClaimStore::new();
    a.ingest(container.clone()).unwrap();
    // While the quote lives, its edges live: the quote backlinks Bob's rec.
    assert_eq!(a.backlinks(&inner_id), vec![container_id]);
    a.ingest(redaction.clone()).unwrap();

    let mut b = ClaimStore::new();
    b.ingest(redaction).unwrap();
    b.ingest(container).unwrap();

    for (store, order) in [(&a, "container-first"), (&b, "redact-first")] {
        assert!(
            store.get(&inner_id).is_none(),
            "{order}: a quote is content, not a row"
        );
        assert!(
            store.get(&container_id).unwrap().embeds().is_empty(),
            "{order}: the redacted quote renders nothing"
        );
        assert!(
            store.backlinks(&inner_id).is_empty(),
            "{order}: the dead quote's edges died with it"
        );
        assert_eq!(store.log_len(&bob.id()), 0, "{order}: bob's log untouched");
    }
    assert_eq!(a.state_vector(), b.state_vector());
    assert!(a.verify_integrity().is_empty());

    // Bob's own copy is unaffected by Alice redacting HER quote: anyone who
    // follows Bob still gets the original from him.
    let mut bobs_friend = ClaimStore::new();
    bobs_friend.ingest(inner).unwrap();
    assert!(bobs_friend.contains(&inner_id));
}

#[test]
fn an_embedded_redact_is_quotation_not_authority() {
    // A redact claim arriving INSIDE a quote has no engine effect: quoting
    // is speech, and redaction authority flows only through the author's
    // own log as a top-level event. Both arrival orders agree.
    let mut bob = Writer::from_seed([2; 32]);
    let mut alice = Writer::from_seed([1; 32]);

    let y = rec(&mut bob, 50, "bob's regret");
    let y_id = id_of(&y);
    let inner_redact = redact(&mut bob, 60, &y); // bob redacts his own y
    let container = vouch(&mut alice, 100, &inner_redact); // alice quotes the redact
    let outer_redact = redact(&mut alice, 200, &container);

    let mut a = ClaimStore::new();
    for e in [y.clone(), container.clone(), outer_redact.clone()] {
        a.ingest(e).unwrap();
    }
    let mut b = ClaimStore::new();
    for e in [outer_redact, container, y.clone()] {
        b.ingest(e).unwrap();
    }

    for (store, order) in [(&a, "y-first"), (&b, "y-last")] {
        assert_eq!(
            store.redaction(&y_id),
            None,
            "{order}: a quoted redact must not censor"
        );
        assert!(store.contains(&y_id), "{order}: y keeps its body");
    }
    assert_eq!(a.state_vector(), b.state_vector());

    // Delivered top-level — from Bob's own log, as sync would — the same
    // redact claim takes effect.
    a.ingest(inner_redact.clone()).unwrap();
    assert_eq!(a.redaction(&y_id), Some(id_of(&inner_redact)));
    assert!(!a.contains(&y_id));
}

#[test]
fn redacting_a_redaction_survives_backup_restore() {
    // CRITICAL regression: a redact claim's body is the ONLY carrier of the
    // fact it encodes. When R1 (redacting X) is itself redacted by R2,
    // dropping R1's body would erase X->R1 from the wire — a store rebuilt
    // from a tombstone backup (events()) could never learn it, and
    // re-delivering X would un-redact it. R1's body must survive.
    let mut alice = Writer::from_seed([1; 32]);
    let content = rec(&mut alice, 100, "deeply regretted");
    let x = id_of(&content);
    let r1 = redact(&mut alice, 200, &content);
    let r2 = redact(&mut alice, 300, &r1);

    let mut a = ClaimStore::new();
    for e in [content.clone(), r1.clone(), r2] {
        a.ingest(e).unwrap();
    }
    assert!(!a.contains(&x));
    assert_eq!(a.redaction(&x), Some(id_of(&r1)));

    // Restore a fresh store from A's serialized event stream — the
    // documented backup/recovery path.
    let mut b = ClaimStore::new();
    for e in a.events() {
        b.ingest(e).unwrap();
    }
    assert!(!b.contains(&x), "X resurrected through backup/restore");
    assert_eq!(b.redaction(&x), Some(id_of(&r1)));
    assert_eq!(a.state_vector(), b.state_vector());

    // The smoking gun: re-delivering the ORIGINAL full content must not
    // resurrect it.
    let report = b.ingest(content).unwrap();
    assert_eq!(report.redacted_skips, 1);
    assert!(!b.contains(&x));
}

#[test]
fn fsck_flags_a_redaction_with_no_backing_claim() {
    // MAJOR regression: a fabricated or dangling redaction row censors a
    // claim with no authority behind it. fsck must catch it.
    use vouch_core::storage::{ClaimStorage, MemoryClaimStorage};

    let mut alice = Writer::from_seed([1; 32]);
    let content = rec(&mut alice, 100, "Joe's Pizza");

    let mut storage = MemoryClaimStorage::new();
    storage
        .set_redaction(id_of(&content), ClaimHash([9; 32]))
        .unwrap();
    let store = ClaimStore::with_storage(Box::new(storage));
    let problems = store.verify_integrity();
    assert!(
        problems
            .iter()
            .any(|p| p.contains("not backed by a valid redact claim")),
        "fabricated redaction not flagged: {problems:?}"
    );

    // A genuine redaction is clean.
    let mut healthy = ClaimStore::new();
    healthy.ingest(content.clone()).unwrap();
    healthy.ingest(redact(&mut alice, 200, &content)).unwrap();
    assert!(healthy.verify_integrity().is_empty());
}

#[test]
fn received_at_is_local_metadata_not_convergent_state() {
    // MAJOR coverage gap: received_at (when THIS store learned the claim)
    // must be excluded from state_vector and fingerprint, like arrival.
    let mut alice = Writer::from_seed([1; 32]);
    let content = rec(&mut alice, 100, "Joe's Pizza");

    let mut early = ClaimStore::new();
    early.ingest_at(content.clone(), 1_000).unwrap();
    let mut late = ClaimStore::new();
    late.ingest_at(content.clone(), 9_999_999).unwrap();

    assert_eq!(early.get(&id_of(&content)).unwrap().received_at, 1_000);
    assert_eq!(late.get(&id_of(&content)).unwrap().received_at, 9_999_999);
    assert_eq!(early.state_vector(), late.state_vector());
    assert_eq!(
        early.fingerprint(&alice.id()),
        late.fingerprint(&alice.id())
    );
}

#[test]
fn redaction_tiebreak_is_smallest_redactor_in_every_order() {
    // MAJOR coverage gap: two distinct redact claims target the same
    // content; the recorded redactor must be order-independent (smallest
    // claim id wins) or fingerprints diverge.
    let mut alice = Writer::from_seed([1; 32]);
    let content = rec(&mut alice, 100, "regret");
    let target = id_of(&content);
    let r_a = redact(&mut alice, 200, &content);
    let r_b = redact(&mut alice, 300, &content);
    let winner = id_of(&r_a).min(id_of(&r_b));
    assert_ne!(id_of(&r_a), id_of(&r_b));

    for order in [[0, 1, 2], [2, 1, 0], [1, 2, 0]] {
        let events = [content.clone(), r_a.clone(), r_b.clone()];
        let mut store = ClaimStore::new();
        for &i in &order {
            store.ingest(events[i].clone()).unwrap();
        }
        assert_eq!(store.redaction(&target), Some(winner), "order {order:?}");
    }
}
