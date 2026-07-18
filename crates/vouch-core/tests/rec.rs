//! The `rec` typed projection over sealed claims: `recommendations()` and
//! `Recommendation::timeline()`.

use std::collections::BTreeMap;

use vouch_core::e2ee::{self, Identity};
use vouch_core::fold::ClaimView;
use vouch_core::{ClaimRef, Database, Draft, SignedEvent, TimelineEntry, Value, Writer};

fn accept_all(_: &ClaimView) -> bool {
    true
}

fn seal(db: &mut Database, seed: u8, draft: Draft) -> SignedEvent {
    let id = Identity::from_seed([seed; 32]);
    let sealed = e2ee::seal_draft(&id.content_key(), &draft).unwrap();
    db.claim(&id.log_id(), sealed.body_value()).unwrap()
}

fn recs(db: &Database) -> Vec<vouch_core::Recommendation> {
    let keys: BTreeMap<_, _> = (1u8..=4)
        .map(|s| {
            let id = Identity::from_seed([s; 32]);
            (id.log_id(), id.content_key())
        })
        .collect();
    let view = e2ee::decrypted_view(db.claims(), &keys);
    vouch_core::rec::recommendations(&view, &accept_all)
}

fn of(refs: impl IntoIterator<Item = ClaimRef>) -> Value {
    Value::Array(refs.into_iter().map(Value::ClaimRef).collect())
}

#[test]
fn timeline_orders_everything_and_flags_what_is_still_current() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    db.add_writer(Writer::from_seed([2; 32]));
    let bob = Identity::from_seed([2; 32]).log_id();

    let rec = seal(
        &mut db,
        1,
        Draft::new("rec")
            .at(1)
            .text("subject", "Joe's Pizza")
            .text("body", "Great!"),
    );
    let rec_ref = ClaimRef {
        log_id: alice,
        hash: rec.id(),
    };
    seal(
        &mut db,
        1,
        Draft::new("edit")
            .at(2)
            .field("of", of([rec_ref]))
            .text("subject", "Joe's Pizzeria"),
    );
    seal(
        &mut db,
        2,
        Draft::new("comment")
            .at(3)
            .field("of", of([rec_ref]))
            .text("text", "love this place"),
    );

    let recs = recs(&db);
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
