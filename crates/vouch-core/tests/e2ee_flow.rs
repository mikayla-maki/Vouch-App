//! The whole E2EE story, end to end at the database level: alice seals
//! her recs, grants bob, and syncs; bob folds plaintext out of them;
//! mallory — holding the same ciphertext — folds nothing.

use std::collections::BTreeMap;

use vouch_core::e2ee::{self, Identity};
use vouch_core::fold::ClaimView;
use vouch_core::{Database, Draft, LogId, SignedEvent, Value, Writer};

fn accept_all(_: &ClaimView) -> bool {
    true
}

fn pull(from: &Database, into: &mut Database, log: &LogId) {
    let events: Vec<SignedEvent> = from.claims().serve_since(log, 0);
    for e in events {
        into.ingest(e).unwrap();
    }
}

fn recs_visible_to(db: &Database, me: &Identity) -> Vec<String> {
    let keys = e2ee::collect_keys(db.claims(), me);
    let view = e2ee::decrypted_view(db.claims(), &keys);
    let mut subjects: Vec<String> = vouch_core::rec::recommendations(&view, &accept_all)
        .into_iter()
        .map(|r| r.subject)
        .collect();
    subjects.sort();
    subjects
}

#[test]
fn a_granted_reader_folds_plaintext_and_a_stranger_folds_nothing() {
    let alice_seed = [1u8; 32];
    let alice_id = Identity::from_seed(alice_seed);
    let mut alice = Database::new();
    let alice_log = alice.add_writer(Writer::from_seed(alice_seed));

    // Everything seals — profile included; her grant for bob rides in
    // her own log as the one readable (but sealed-box) wrapper.
    let profile = Draft::new("profile").at(1).text("name", "Alice");
    let sealed_profile = e2ee::seal_draft(&alice_id.content_key(), &profile).unwrap();
    alice.claim(&alice_log, sealed_profile.body_value()).unwrap();
    let rec = Draft::new("rec")
        .at(2)
        .text("subject", "Secret taco spot")
        .text("body", "The one behind the laundromat");
    let sealed_rec = e2ee::seal_draft(&alice_id.content_key(), &rec).unwrap();
    alice.claim(&alice_log, sealed_rec.body_value()).unwrap();

    let bob_id = Identity::from_seed([2u8; 32]);
    let grant = alice_id.grant_for(bob_id.log_id()).unwrap();
    alice
        .claim(
            &alice_log,
            Draft::new(e2ee::GRANT_TYPE)
                .field("sealed", Value::Bytes(grant))
                .body_value(),
        )
        .unwrap();

    // Both bob and mallory receive the identical claim set.
    let mut bob = Database::new();
    pull(&alice, &mut bob, &alice_log);
    let mallory_id = Identity::from_seed([6u8; 32]);
    let mut mallory = Database::new();
    pull(&alice, &mut mallory, &alice_log);

    assert_eq!(
        recs_visible_to(&bob, &bob_id),
        vec!["Secret taco spot".to_string()],
        "the grant let bob fold alice's sealed rec into plaintext"
    );
    assert_eq!(
        recs_visible_to(&mallory, &mallory_id),
        Vec::<String>::new(),
        "mallory holds the ciphertext and reads nothing"
    );

    // Names are sealed speech like everything else: bob resolves alice's
    // name through his grant; mallory resolves nothing at all.
    let bob_keys = e2ee::collect_keys(bob.claims(), &bob_id);
    let bob_view = e2ee::decrypted_view(bob.claims(), &bob_keys);
    assert_eq!(
        vouch_core::profile::names(&bob_view)
            .get(&alice_log)
            .map(String::as_str),
        Some("Alice")
    );
    let mallory_keys = e2ee::collect_keys(mallory.claims(), &mallory_id);
    let mallory_view = e2ee::decrypted_view(mallory.claims(), &mallory_keys);
    assert!(vouch_core::profile::names(&mallory_view).is_empty());
}

#[test]
fn sealed_edits_fold_with_their_sealed_original() {
    let seed = [3u8; 32];
    let id = Identity::from_seed(seed);
    let key = id.content_key();
    let mut db = Database::new();
    let log = db.add_writer(Writer::from_seed(seed));

    let rec = Draft::new("rec").at(1).text("subject", "Old name").text("body", "...");
    let sealed = e2ee::seal_draft(&key, &rec).unwrap();
    let rec_event = db.claim(&log, sealed.body_value()).unwrap();

    // The edit references the ORIGINAL claim's hash — the hash of the
    // envelope, since that's the claim that exists on the wire. The
    // reference lives inside the edit's own ciphertext.
    let edit = Draft::new("edit")
        .at(2)
        .field(
            "of",
            Value::array([Value::ClaimRef(vouch_core::ClaimRef {
                log_id: log,
                hash: rec_event.id(),
            })]),
        )
        .text("subject", "New name");
    let sealed_edit = e2ee::seal_draft(&key, &edit).unwrap();
    db.claim(&log, sealed_edit.body_value()).unwrap();

    let mut keys = BTreeMap::new();
    keys.insert(log, key);
    let view = e2ee::decrypted_view(db.claims(), &keys);
    let recs = vouch_core::rec::recommendations(&view, &accept_all);
    assert_eq!(recs.len(), 1, "envelope + sealed edit are one component");
    assert_eq!(recs[0].subject, "New name");
}
