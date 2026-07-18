//! The `rec` typed projection: `recommendations()` and `Recommendation::timeline()`.

use vouch_core::{ClaimRef, Database, Draft, StoredClaim, TimelineEntry, Value, Writer};

fn accept_all(_: &StoredClaim) -> bool {
    true
}

fn of(refs: impl IntoIterator<Item = ClaimRef>) -> Value {
    Value::Array(refs.into_iter().map(Value::ClaimRef).collect())
}

#[test]
fn timeline_orders_everything_and_flags_what_is_still_current() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    let bob = db.add_writer(Writer::from_seed([2; 32]));

    let rec = db
        .compose(
            &alice,
            Draft::new("rec")
                .at(1)
                .text("subject", "Joe's Pizza")
                .text("body", "Great!"),
        )
        .unwrap();
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };
    db.compose(
        &alice,
        Draft::new("edit")
            .at(2)
            .field("of", of([rec_ref]))
            .text("subject", "Joe's Pizzeria"),
    )
    .unwrap();
    db.compose(
        &bob,
        Draft::new("comment")
            .at(3)
            .field("of", of([rec_ref]))
            .text("text", "love this place"),
    )
    .unwrap();

    let recs = vouch_core::rec::recommendations(db.claims(), &accept_all);
    assert_eq!(recs.len(), 1);
    let timeline = recs[0].timeline();

    // subject: 2 entries (original + edit), body: 1 entry (untouched),
    // comment: 1 entry. Oldest first.
    assert_eq!(timeline.len(), 4);
    assert_eq!(timeline[0].at(), 1);
    assert_eq!(timeline[3].at(), 3);

    let subject_entries: Vec<&TimelineEntry> = timeline
        .iter()
        .filter(|e| matches!(e, TimelineEntry::Field { field, .. } if field == "subject"))
        .collect();
    assert_eq!(subject_entries.len(), 2);
    let TimelineEntry::Field { current, value, .. } = subject_entries[0] else {
        unreachable!()
    };
    assert!(!current, "the original subject has been superseded");
    assert_eq!(*value, Value::text("Joe's Pizza"));
    let TimelineEntry::Field { current, value, .. } = subject_entries[1] else {
        unreachable!()
    };
    assert!(*current, "the edit is what's showing now");
    assert_eq!(*value, Value::text("Joe's Pizzeria"));

    let comment_entries: Vec<&TimelineEntry> = timeline
        .iter()
        .filter(|e| matches!(e, TimelineEntry::Comment { .. }))
        .collect();
    assert_eq!(comment_entries.len(), 1);
    assert_eq!(comment_entries[0].author(), bob);
}
