//! A relay's retention policy: `Database::gc_claims_older_than` — a hard,
//! time-bounded delete, never a cursor-driven one. See the doc comment on
//! `ClaimStore::purge_older_than` for why cursors can't answer "safe to
//! delete" in a world where anyone might follow a log years from now.

use vouch_core::{ClaimRef, Database, Value, Writer};

#[test]
fn a_claim_older_than_the_cutoff_is_purged_a_newer_one_survives() {
    // Mint both claims on one throwaway database (so signing succeeds),
    // then ingest them into the "relay" with received_at under our
    // control, exactly as a real relay would record arrival time itself.
    let mut minter = Database::new();
    let alice = minter.add_writer(Writer::from_seed([1; 32]));
    let old = minter
        .claim(
            &alice,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("old")),
            ]),
        )
        .unwrap();
    let new = minter
        .claim(
            &alice,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("new")),
            ]),
        )
        .unwrap();

    let mut relay = Database::new();
    relay.ingest_at(old.clone(), 1_000).unwrap();
    relay.ingest_at(new.clone(), 9_000).unwrap();

    assert!(relay.claims().contains(&old.id()));
    assert!(relay.claims().contains(&new.id()));

    let purged = relay.gc_claims_older_than(5_000).unwrap();
    assert_eq!(purged, vec![old.id()]);
    assert!(!relay.claims().contains(&old.id()));
    assert!(relay.claims().contains(&new.id()));
}

#[test]
fn purging_leaves_a_surviving_claims_reference_to_it_dangling() {
    let mut minter = Database::new();
    let alice = minter.add_writer(Writer::from_seed([1; 32]));

    let target = minter
        .claim(&alice, Value::map([("type", Value::text("rec"))]))
        .unwrap();
    // `referencer` points at `target` — an outgoing edge.
    let referencer = minter
        .claim(
            &alice,
            Value::map([
                ("type", Value::text("edit")),
                (
                    "of",
                    Value::array([Value::ClaimRef(ClaimRef {
                        log_id: alice,
                        hash: target.id(),
                    })]),
                ),
            ]),
        )
        .unwrap();

    let mut relay = Database::new();
    relay.ingest_at(target.clone(), 1_000).unwrap();
    relay.ingest_at(referencer.clone(), 2_000).unwrap();

    assert_eq!(relay.claims().backlinks(&target.id()), vec![referencer.id()]);

    // Purge only `target` (received before 1_500); `referencer` (received
    // at 2_000) survives and still references it.
    let purged = relay.gc_claims_older_than(1_500).unwrap();
    assert_eq!(purged, vec![target.id()]);

    assert!(relay.claims().contains(&referencer.id()));
    assert!(!relay.claims().contains(&target.id()));
    assert_eq!(
        relay.claims().backlinks(&target.id()),
        vec![referencer.id()],
        "the surviving claim's reference to the purged one is left as-is, by design"
    );
}
