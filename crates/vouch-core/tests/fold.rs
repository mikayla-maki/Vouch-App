//! The materializer prototype: `rec` claims plus `edit` claims (source-
//! author only) that patch individual fields, plus `comment` claims (any
//! author, never merged) — folded into components with per-field causal
//! frontiers. No minted ids anywhere: identity is connectivity, and
//! conflicts are exposed rather than silently resolved.

use vouch_core::fold::fold;
use vouch_core::{ClaimRef, Database, Draft, LogId, StoredClaim, Value, Writer};

fn accept_all(_: &StoredClaim) -> bool {
    true
}

fn run(db: &Database) -> Vec<vouch_core::Component> {
    fold(db.claims(), "rec", "edit", "comment", &accept_all)
}

fn text(component: &vouch_core::Component, field: &str) -> Vec<String> {
    let mut values: Vec<String> = component
        .fields
        .get(field)
        .map(|f| {
            f.frontier
                .iter()
                .filter_map(|c| match &c.value {
                    Value::Text(t) => Some(t.clone()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    values.sort_unstable();
    values
}

fn of(refs: impl IntoIterator<Item = ClaimRef>) -> Value {
    Value::Array(refs.into_iter().map(Value::ClaimRef).collect())
}

#[test]
fn a_same_author_edit_sets_an_unambiguous_field() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));

    let rec = db
        .compose(
            &alice,
            Draft::new("rec")
                .text("subject", "Joe's Pizza")
                .text("body", "Great!"),
        )
        .unwrap();
    db.compose(
        &alice,
        Draft::new("edit")
            .field(
                "of",
                of([ClaimRef {
                    log_id: alice,
                    hash: rec.id(),
                }]),
            )
            .text("address", "123 Main St"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(c.claims.len(), 2);
    assert_eq!(text(c, "subject"), vec!["Joe's Pizza"]);
    assert_eq!(text(c, "address"), vec!["123 Main St"]);
}

#[test]
fn an_edit_from_anyone_else_is_inert() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    let bob = db.add_writer(Writer::from_seed([2; 32]));

    let rec = db
        .compose(&alice, Draft::new("rec").text("subject", "Joe's Pizza"))
        .unwrap();
    // Bob tries to "edit" Alice's rec directly. It's still a real, stored
    // claim (the debug viewer would show it) — it just doesn't count.
    db.compose(
        &bob,
        Draft::new("edit")
            .field(
                "of",
                of([ClaimRef {
                    log_id: alice,
                    hash: rec.id(),
                }]),
            )
            .text("address", "1 Bob's Way"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(c.claims.len(), 2); // both claims are still in the component
    assert!(text(c, "address").is_empty()); // but the edit didn't take
}

#[test]
fn the_same_authors_concurrent_edits_expose_a_conflict() {
    // The realistic case now that edits are author-scoped: Alice edits
    // from her phone and her laptop while offline, neither aware of the
    // other, and syncs both later.
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));

    let rec = db
        .compose(&alice, Draft::new("rec").text("subject", "Taco place"))
        .unwrap();
    let of_rec = of([ClaimRef {
        log_id: alice,
        hash: rec.id(),
    }]);

    db.compose(
        &alice,
        Draft::new("edit")
            .field("of", of_rec.clone())
            .text("address", "1 Phone St"),
    )
    .unwrap();
    db.compose(
        &alice,
        Draft::new("edit")
            .field("of", of_rec)
            .text("address", "2 Laptop Ave"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let addresses = text(&components[0], "address");
    assert_eq!(addresses, vec!["1 Phone St", "2 Laptop Ave"]);
}

#[test]
fn a_reconciling_edit_that_resets_the_field_collapses_the_frontier() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));

    let rec = db
        .compose(&alice, Draft::new("rec").text("subject", "Taco place"))
        .unwrap();
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };
    let edit_phone = db
        .compose(
            &alice,
            Draft::new("edit")
                .field("of", of([rec_ref]))
                .text("address", "1 Phone St"),
        )
        .unwrap();
    let edit_laptop = db
        .compose(
            &alice,
            Draft::new("edit")
                .field("of", of([rec_ref]))
                .text("address", "2 Laptop Ave"),
        )
        .unwrap();

    // Alice reconciles her own conflict: one more edit referencing both,
    // re-asserting the field. Nothing special — just an ordinary edit.
    db.compose(
        &alice,
        Draft::new("edit")
            .field(
                "of",
                of([
                    ClaimRef {
                        log_id: alice,
                        hash: edit_phone.id(),
                    },
                    ClaimRef {
                        log_id: alice,
                        hash: edit_laptop.id(),
                    },
                ]),
            )
            .text("address", "3 Reconciled Rd"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    assert_eq!(text(&components[0], "address"), vec!["3 Reconciled Rd"]);
}

#[test]
fn a_reconciling_edit_that_ignores_the_field_leaves_its_conflict_standing() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));

    let rec = db
        .compose(&alice, Draft::new("rec").text("subject", "Taco place"))
        .unwrap();
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };
    let edit_phone = db
        .compose(
            &alice,
            Draft::new("edit")
                .field("of", of([rec_ref]))
                .text("address", "1 Phone St"),
        )
        .unwrap();
    let edit_laptop = db
        .compose(
            &alice,
            Draft::new("edit")
                .field("of", of([rec_ref]))
                .text("address", "2 Laptop Ave"),
        )
        .unwrap();

    // Alice references both but only touches a different field — she
    // hasn't resolved the address conflict, just added a note.
    db.compose(
        &alice,
        Draft::new("edit")
            .field(
                "of",
                of([
                    ClaimRef {
                        log_id: alice,
                        hash: edit_phone.id(),
                    },
                    ClaimRef {
                        log_id: alice,
                        hash: edit_laptop.id(),
                    },
                ]),
            )
            .text("note", "need to pick one of these"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(text(c, "address"), vec!["1 Phone St", "2 Laptop Ave"]);
    assert_eq!(text(c, "note"), vec!["need to pick one of these"]);
}

#[test]
fn anyone_can_comment_without_affecting_fields() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    let bob = db.add_writer(Writer::from_seed([2; 32]));

    let rec = db
        .compose(&alice, Draft::new("rec").text("subject", "Taco place"))
        .unwrap();
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };

    db.compose(
        &bob,
        Draft::new("comment")
            .field("of", of([rec_ref]))
            .text("text", "the address is 123 Main St, I think"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert!(text(c, "address").is_empty()); // a comment never sets a field
    assert_eq!(c.comments.len(), 1);
    assert_eq!(c.comments[0].author, bob);
    assert_eq!(c.comments[0].text, "the address is 123 Main St, I think");
}

#[test]
fn two_independently_filed_recs_collate_once_something_links_them() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    let bob = db.add_writer(Writer::from_seed([2; 32]));
    let carol = db.add_writer(Writer::from_seed([3; 32]));

    let rec_a = db
        .compose(&alice, Draft::new("rec").text("subject", "Taco Palace"))
        .unwrap();
    let rec_b = db
        .compose(&bob, Draft::new("rec").text("subject", "That taco place"))
        .unwrap();

    // Before anything links them, they're independent.
    assert_eq!(run(&db).len(), 2);

    // Carol notices they're the same place. Linking isn't an edit (she
    // owns neither), so it rides in as a comment.
    db.compose(
        &carol,
        Draft::new("comment")
            .field(
                "of",
                of([
                    ClaimRef {
                        log_id: alice,
                        hash: rec_a.id(),
                    },
                    ClaimRef {
                        log_id: bob,
                        hash: rec_b.id(),
                    },
                ]),
            )
            .text("text", "pretty sure these are the same place"),
    )
    .unwrap();

    let components = run(&db);
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].claims.len(), 3);
    assert_eq!(components[0].comments.len(), 1);
}

#[test]
fn the_fold_is_independent_of_arrival_order() {
    let mut forward = Database::new();
    let alice = forward.add_writer(Writer::from_seed([1; 32]));
    let bob: LogId = forward.add_writer(Writer::from_seed([2; 32]));

    let rec = forward
        .compose(&alice, Draft::new("rec").text("subject", "Taco place"))
        .unwrap();
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };
    let edit = forward
        .compose(
            &alice,
            Draft::new("edit")
                .field("of", of([rec_ref]))
                .text("address", "1 Main St"),
        )
        .unwrap();
    let comment = forward
        .compose(
            &bob,
            Draft::new("comment")
                .field("of", of([rec_ref]))
                .text("text", "love this place"),
        )
        .unwrap();

    // The same three events, ingested into a fresh store in reverse order.
    let mut backward = Database::new();
    for event in [comment, edit, rec] {
        backward.ingest(event).unwrap();
    }

    let mut a = run(&forward);
    let mut b = run(&backward);
    a.sort_by_key(|c| c.id);
    b.sort_by_key(|c| c.id);
    assert_eq!(a, b);
}
