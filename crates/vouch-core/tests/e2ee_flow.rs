//! The whole E2EE story, end to end at the database level: alice seals
//! her speech and hands bob her address; bob folds plaintext out of the
//! synced ciphertext; mallory — holding the same ciphertext but only
//! the LogId — folds nothing.

use std::collections::BTreeMap;

use vouch_core::e2ee::{self, Address, Identity};
use vouch_core::fold::ClaimView;
use vouch_core::{Database, Draft, LogId, Event, Value, Writer};

fn accept_all(_: &ClaimView) -> bool {
    true
}

fn pull(from: &Database, into: &mut Database, log: &LogId) {
    let events: Vec<Event> = from.claims().serve_since(log, 0);
    for e in events {
        into.ingest(e).unwrap();
    }
}

fn recs_visible_to(db: &Database, me: &Identity, follows: &[Address]) -> Vec<String> {
    let keys = e2ee::keys_for(me, follows);
    let view = e2ee::decrypted_view(db.claims(), &keys);
    let mut subjects: Vec<String> = vouch_core::rec::recommendations(&view, &accept_all)
        .into_iter()
        .map(|r| r.subject)
        .collect();
    subjects.sort();
    subjects
}

#[test]
fn an_address_holder_folds_plaintext_and_a_stranger_folds_nothing() {
    let alice_seed = [1u8; 32];
    let alice_id = Identity::from_seed(alice_seed);
    let mut alice = Database::new();
    let alice_log = alice.add_writer(Writer::from_seed(alice_seed));

    // Everything seals — profile included. Nothing about bob ever
    // enters alice's log: the grant happened out of band, when she
    // texted him her address.
    let profile = Draft::new("profile").at(1).text("name", "Alice");
    let sealed_profile = e2ee::seal_draft(&alice_id.content_key(), &profile).unwrap();
    alice.claim(&alice_log, sealed_profile.body_value()).unwrap();
    let rec = Draft::new("rec")
        .at(2)
        .text("subject", "Secret taco spot")
        .text("body", "The one behind the laundromat");
    let sealed_rec = e2ee::seal_draft(&alice_id.content_key(), &rec).unwrap();
    alice.claim(&alice_log, sealed_rec.body_value()).unwrap();

    // Both bob and mallory receive the identical claim set — the relay
    // and the wire treat them the same. Only bob holds alice's address.
    let bob_id = Identity::from_seed([2u8; 32]);
    let mut bob = Database::new();
    pull(&alice, &mut bob, &alice_log);
    let mallory_id = Identity::from_seed([6u8; 32]);
    let mut mallory = Database::new();
    pull(&alice, &mut mallory, &alice_log);

    let alice_address = alice_id.address();
    assert_eq!(
        recs_visible_to(&bob, &bob_id, &[alice_address]),
        vec!["Secret taco spot".to_string()],
        "the pasted address let bob fold alice's sealed rec into plaintext"
    );
    assert_eq!(
        recs_visible_to(&mallory, &mallory_id, &[]),
        Vec::<String>::new(),
        "mallory holds the ciphertext and reads nothing"
    );

    // Names are sealed speech like everything else: bob resolves alice's
    // name through her address; mallory resolves nothing at all.
    let bob_keys = e2ee::keys_for(&bob_id, &[alice_address]);
    let bob_view = e2ee::decrypted_view(bob.claims(), &bob_keys);
    assert_eq!(
        vouch_core::profile::names(&bob_view)
            .get(&alice_log)
            .map(String::as_str),
        Some("Alice")
    );
    let mallory_keys = e2ee::keys_for(&mallory_id, &[]);
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

/// The full going-on-the-record loop through real storage and sync: alice
/// mints a sealed rec, then attests it; bob (following her) sees the rec
/// badge as on-the-record. When she edits past the attested claim, the
/// badge downgrades — an attestation binds words, not the thread.
#[test]
fn an_attested_rec_reads_on_the_record_until_edited_past_it() {
    let alice_seed = [1u8; 32];
    let alice_id = Identity::from_seed(alice_seed);
    let mut alice = Database::new();
    let alice_log = alice.add_writer(Writer::from_seed(alice_seed));

    let rec = Draft::new("rec")
        .at(1)
        .text("subject", "Delfina")
        .text("body", "Overrated, honestly");
    let sealed = e2ee::seal_draft(&alice_id.content_key(), &rec).unwrap();
    let rec_event = alice.claim(&alice_log, sealed.body_value()).unwrap();

    // Going on the record: an ordinary sealed claim whose payload is the
    // one signature in the system.
    let attest = alice_id.attest(rec_event.id(), &rec.body_value()).at(2);
    let sealed_attest = e2ee::seal_draft(&alice_id.content_key(), &attest).unwrap();
    alice.claim(&alice_log, sealed_attest.body_value()).unwrap();

    let mut bob = Database::new();
    pull(&alice, &mut bob, &alice_log);
    let bob_id = Identity::from_seed([2u8; 32]);
    let keys = e2ee::keys_for(&bob_id, &[alice_id.address()]);
    let view = e2ee::decrypted_view(bob.claims(), &keys);
    let recs = vouch_core::rec::recommendations(&view, &accept_all);
    assert_eq!(recs.len(), 1);
    assert!(recs[0].on_the_record(), "bob sees alice went on the record");
    assert!(!recs[0].attested_earlier());

    // Alice edits the body: the shown text is no longer what she signed.
    let edit = Draft::new("edit")
        .at(3)
        .field(
            "of",
            Value::Array(vec![Value::ClaimRef(vouch_core::ClaimRef {
                log_id: alice_log,
                hash: rec_event.id(),
            })]),
        )
        .text("body", "Fine, the pizza is good actually");
    let sealed_edit = e2ee::seal_draft(&alice_id.content_key(), &edit).unwrap();
    alice.claim(&alice_log, sealed_edit.body_value()).unwrap();

    pull(&alice, &mut bob, &alice_log);
    let view = e2ee::decrypted_view(bob.claims(), &keys);
    let recs = vouch_core::rec::recommendations(&view, &accept_all);
    assert_eq!(recs.len(), 1);
    assert_eq!(recs[0].body, "Fine, the pizza is good actually");
    assert!(
        !recs[0].on_the_record(),
        "the edit is new, unattested speech"
    );
    assert!(
        recs[0].attested_earlier(),
        "but the earlier attestation is remembered"
    );

    // A forged attestation from someone else's log binds nothing: mallory
    // (who holds alice's address, so she can read AND mint valid-tagged
    // claims into her own log) attests alice's claim — inert.
    let mallory_seed = [6u8; 32];
    let mallory_id = Identity::from_seed(mallory_seed);
    let mut mallory = Database::new();
    let mallory_log = mallory.add_writer(Writer::from_seed(mallory_seed));
    let forged = mallory_id.attest(rec_event.id(), &rec.body_value()).at(4);
    let sealed_forged = e2ee::seal_draft(&mallory_id.content_key(), &forged).unwrap();
    mallory.claim(&mallory_log, sealed_forged.body_value()).unwrap();
    pull(&mallory, &mut bob, &mallory_log);

    let keys = e2ee::keys_for(&bob_id, &[alice_id.address(), mallory_id.address()]);
    let view = e2ee::decrypted_view(bob.claims(), &keys);
    let recs = vouch_core::rec::recommendations(&view, &accept_all);
    let rec = recs.iter().find(|r| r.subject == "Delfina").unwrap();
    assert!(
        !rec.on_the_record(),
        "only the author's own signature escalates their words"
    );
}
