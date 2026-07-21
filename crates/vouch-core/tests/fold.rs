//! The materializer over sealed claims — there is no plaintext content in
//! Vouch, so every fold test writes the way the app does: seal with the
//! author's content key, mint the envelope, fold through a decrypted
//! view. `rec` roots plus `edit` patches (source-author only) plus
//! `comment`s (any author), per-field causal frontiers, no minted IDs.
//!
//! Key possession is NOT what these tests probe (e2ee_flow.rs covers
//! who-can-read-what): the view here holds every participant's key so
//! the fold semantics themselves are on stage.

use std::collections::BTreeMap;

use vouch_core::e2ee::{self, ContentKey, Identity};
use vouch_core::fold::{ClaimView, fold};
use vouch_core::{ClaimRef, Database, Draft, LogId, Event, Value, Writer};

fn accept_all(_: &ClaimView) -> bool {
    true
}

/// A database with writers for the given seeds.
fn db_with(seeds: &[u8]) -> Database {
    let mut db = Database::new();
    for &seed in seeds {
        db.add_writer(Writer::from_seed([seed; 32]));
    }
    db
}

fn log_of(seed: u8) -> LogId {
    Identity::from_seed([seed; 32]).log_id()
}

/// Seal `draft` with seed's content key and mint it into their log.
fn seal(db: &mut Database, seed: u8, draft: Draft) -> Event {
    let id = Identity::from_seed([seed; 32]);
    let sealed = e2ee::seal_draft(&id.content_key(), &draft).unwrap();
    db.claim(&id.log_id(), sealed.body_value()).unwrap()
}

/// Every test participant's key, so the fold — not key possession — is
/// what's under test.
fn all_keys() -> BTreeMap<LogId, ContentKey> {
    (1u8..=8)
        .map(|seed| {
            let id = Identity::from_seed([seed; 32]);
            (id.log_id(), id.content_key())
        })
        .collect()
}

fn run(db: &Database) -> Vec<vouch_core::Component> {
    let view = e2ee::decrypted_view(db.claims(), &all_keys());
    fold(&view, "rec", "edit", "comment", &accept_all)
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

fn rec_ref(seed: u8, event: &Event) -> ClaimRef {
    ClaimRef {
        log_id: log_of(seed),
        hash: event.id(),
    }
}

#[test]
fn a_same_author_edit_sets_an_unambiguous_field() {
    let mut db = db_with(&[1]);
    let rec = seal(
        &mut db,
        1,
        Draft::new("rec")
            .text("subject", "Joe's Pizza")
            .text("body", "Great!"),
    );
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "123 Main St"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(c.claims.len(), 2);
    assert_eq!(text(c, "subject"), vec!["Joe's Pizza"]);
    assert_eq!(text(c, "address"), vec!["123 Main St"]);
}

#[test]
fn an_edit_from_anyone_else_is_inert() {
    let mut db = db_with(&[1, 2]);
    let rec = seal(&mut db, 1, Draft::new("rec").text("subject", "Joe's Pizza"));
    // Bob tries to "edit" Alice's rec. Still a real, stored claim — it
    // just doesn't count toward any field.
    seal(
        &mut db,
        2,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "1 Bob's Way"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(c.claims.len(), 2); // both claims are in the component
    assert!(text(c, "address").is_empty()); // but the edit didn't take
}

#[test]
fn the_same_authors_concurrent_edits_expose_a_conflict() {
    // Alice edits from her phone and her laptop while offline; neither
    // edit knows about the other. The conflict is exposed, not resolved.
    let mut db = db_with(&[1]);
    let rec = seal(&mut db, 1, Draft::new("rec").text("subject", "Taco place"));
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "1 Phone St"),
    );
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "2 Laptop Ave"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    assert_eq!(
        text(&components[0], "address"),
        vec!["1 Phone St", "2 Laptop Ave"]
    );
}

#[test]
fn a_reconciling_edit_that_resets_the_field_collapses_the_frontier() {
    let mut db = db_with(&[1]);
    let rec = seal(&mut db, 1, Draft::new("rec").text("subject", "Taco place"));
    let phone = seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "1 Phone St"),
    );
    let laptop = seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "2 Laptop Ave"),
    );
    // One more edit referencing both concurrent edits, re-asserting the
    // field: an ordinary edit, nothing special — a merge commit.
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &phone), rec_ref(1, &laptop)]))
            .text("address", "3 Reconciled Rd"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    assert_eq!(text(&components[0], "address"), vec!["3 Reconciled Rd"]);
}

#[test]
fn a_reconciling_edit_that_ignores_the_field_leaves_its_conflict_standing() {
    let mut db = db_with(&[1]);
    let rec = seal(&mut db, 1, Draft::new("rec").text("subject", "Taco place"));
    let phone = seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "1 Phone St"),
    );
    let laptop = seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "2 Laptop Ave"),
    );
    // References both but only adds a note: the address conflict stands.
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &phone), rec_ref(1, &laptop)]))
            .text("note", "need to pick one of these"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert_eq!(text(c, "address"), vec!["1 Phone St", "2 Laptop Ave"]);
    assert_eq!(text(c, "note"), vec!["need to pick one of these"]);
}

#[test]
fn anyone_can_comment_without_affecting_fields() {
    let mut db = db_with(&[1, 2]);
    let rec = seal(&mut db, 1, Draft::new("rec").text("subject", "Taco place"));
    seal(
        &mut db,
        2,
        Draft::new("comment")
            .field("of", of([rec_ref(1, &rec)]))
            .text("text", "the address is 123 Main St, I think"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    let c = &components[0];
    assert!(text(c, "address").is_empty()); // a comment never sets a field
    assert_eq!(c.comments.len(), 1);
    assert_eq!(c.comments[0].author, log_of(2));
    assert_eq!(c.comments[0].text, "the address is 123 Main St, I think");
}

#[test]
fn two_independently_filed_recs_collate_once_something_links_them() {
    let mut db = db_with(&[1, 2, 3]);
    let rec_a = seal(&mut db, 1, Draft::new("rec").text("subject", "Taco Palace"));
    let rec_b = seal(
        &mut db,
        2,
        Draft::new("rec").text("subject", "That taco place"),
    );

    assert_eq!(run(&db).len(), 2, "independent until linked");

    // Carol notices they're the same place; linking rides in a comment
    // (she owns neither, so it can't be an edit).
    seal(
        &mut db,
        3,
        Draft::new("comment")
            .field("of", of([rec_ref(1, &rec_a), rec_ref(2, &rec_b)]))
            .text("text", "pretty sure these are the same place"),
    );

    let components = run(&db);
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].claims.len(), 3);
    assert_eq!(components[0].comments.len(), 1);
}

#[test]
fn the_fold_is_independent_of_arrival_order() {
    let mut forward = db_with(&[1, 2]);
    let rec = seal(&mut forward, 1, Draft::new("rec").text("subject", "Taco place"));
    let edit = seal(
        &mut forward,
        1,
        Draft::new("edit")
            .field("of", of([rec_ref(1, &rec)]))
            .text("address", "1 Main St"),
    );
    let comment = seal(
        &mut forward,
        2,
        Draft::new("comment")
            .field("of", of([rec_ref(1, &rec)]))
            .text("text", "love this place"),
    );

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
