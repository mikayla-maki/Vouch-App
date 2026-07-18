//! The advertised-name projection: newest profile claim per log wins,
//! sanitized, absent when never set.

use vouch_core::profile::{names, sanitize_name};
use vouch_core::{Database, Draft, Writer};

#[test]
fn the_newest_profile_claim_names_the_log() {
    let mut db = Database::new();
    let alice = db.add_writer(Writer::from_seed([1; 32]));
    let bob = db.add_writer(Writer::from_seed([2; 32]));

    db.compose(&alice, Draft::new("profile").at(1).text("name", "Alice"))
        .unwrap();
    db.compose(&alice, Draft::new("profile").at(2).text("name", "Alice P."))
        .unwrap();
    db.compose(&bob, Draft::new("rec").at(3).text("subject", "no profile"))
        .unwrap();

    let names = names(db.claims());
    assert_eq!(names.get(&alice).map(String::as_str), Some("Alice P."));
    assert_eq!(names.get(&bob), None, "no profile claim, no suggestion");
}

#[test]
fn hostile_names_are_defanged() {
    assert_eq!(sanitize_name("  Alice  "), "Alice");
    assert_eq!(sanitize_name("Al\u{7}ice\n"), "Alice");
    let long = "x".repeat(200);
    assert_eq!(sanitize_name(&long).len(), 40);
    assert_eq!(sanitize_name("\n\t  "), "");
}
