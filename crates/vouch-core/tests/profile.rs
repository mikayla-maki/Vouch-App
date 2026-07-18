//! The advertised-name projection over sealed profiles: newest profile
//! claim per log wins — for readers who hold that log's key; a name is
//! only "advertised" to your granted audience, like everything else.

use std::collections::BTreeMap;

use vouch_core::e2ee::{self, Identity};
use vouch_core::profile::{names, sanitize_name};
use vouch_core::{Database, Draft, Writer};

fn seal(db: &mut Database, seed: u8, draft: Draft) {
    let id = Identity::from_seed([seed; 32]);
    let sealed = e2ee::seal_draft(&id.content_key(), &draft).unwrap();
    db.claim(&id.log_id(), sealed.body_value()).unwrap();
}

#[test]
fn the_newest_profile_claim_names_the_log_for_key_holders_only() {
    let mut db = Database::new();
    db.add_writer(Writer::from_seed([1; 32]));
    db.add_writer(Writer::from_seed([2; 32]));
    let alice = Identity::from_seed([1; 32]);
    let bob = Identity::from_seed([2; 32]);

    seal(&mut db, 1, Draft::new("profile").at(1).text("name", "Alice"));
    seal(&mut db, 1, Draft::new("profile").at(2).text("name", "Alice P."));
    seal(&mut db, 2, Draft::new("rec").at(3).text("subject", "no profile"));

    // A reader holding both keys: newest name wins; no profile, no name.
    let keys: BTreeMap<_, _> = [
        (alice.log_id(), alice.content_key()),
        (bob.log_id(), bob.content_key()),
    ]
    .into();
    let view = e2ee::decrypted_view(db.claims(), &keys);
    let resolved = names(&view);
    assert_eq!(resolved.get(&alice.log_id()).map(String::as_str), Some("Alice P."));
    assert_eq!(resolved.get(&bob.log_id()), None, "no profile claim, no suggestion");

    // A reader with NO keys resolves nothing — names are sealed speech.
    let view = e2ee::decrypted_view(db.claims(), &BTreeMap::new());
    assert!(names(&view).is_empty());
}

#[test]
fn hostile_names_are_defanged() {
    assert_eq!(sanitize_name("  Alice  "), "Alice");
    assert_eq!(sanitize_name("Al\u{7}ice\n"), "Alice");
    let long = "x".repeat(200);
    assert_eq!(sanitize_name(&long).len(), 40);
    assert_eq!(sanitize_name("\n\t  "), "");
}
