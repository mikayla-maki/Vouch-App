//! Conformance test vectors.
//!
//! These constants are the cross-language contract: an implementation in any
//! language must produce these exact bytes for these exact claims. If a
//! change to vouch-core breaks this test, that change is a wire-format break
//! and needs a version bump and a migration story — not an updated constant.
//!
//! (History: v1 vectors carried Ed25519 signatures. WIRE_VERSION 2 removed
//! signatures from the wire — claims are tagged with HMAC-SHA256 under the
//! log's derived auth key — so every constant here was regenerated with the
//! version bump, and the v1 vectors died with the alpha wipe.)

use vouch_core::e2ee::{self, Identity};
use vouch_core::{Event, EventHeader, Value, WIRE_VERSION, Writer, header_tag};

const SEED: [u8; 32] = [0x42; 32];

const LOG_ID_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";

/// Claim 1: body {type:"rec", at:1750000000000, subject:"Joe's Pizza",
/// body:"best slice in town"}.
const E1_HEADER_HEX: &str = "830258202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db125820978a35021eac05b71205e92efc0a18bad14fcdfee3ff577a6c67840453a2b36e";
const E1_TAG_HEX: &str = "40ca637ccda7f811b82c5c19fa0b9cd44cf3034ce9e35b646b22899126e7c907";
const E1_BODY_HEX: &str = "a46261741b000001977420dc0064626f6479726265737420736c69636520696e20746f776e647479706563726563677375626a6563746b4a6f6527732050697a7a61";
const E1_ID_HEX: &str = "27e4253355b8d1bed00dfb1a0d3a2416c7d756921a7b5b417efd1edc98e77fcb";

/// Claim 2: body {type:"rec", at:1750000100000, subject:"Blue Bottle"}.
const E2_HEADER_HEX: &str = "830258202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1258202e3d24d2f4011e5abad88d4a5bafce1e879b7fe7eca08faa5e7e88e173272c0f";
const E2_ID_HEX: &str = "7cbf4ccf44913932ca16421421ed0a0f68f9b7b285b3dc70532faaf7169ea8b6";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn auth_key() -> vouch_core::AuthKey {
    e2ee::auth_key(&Identity::from_seed(SEED).content_key())
}

fn vector_writer() -> (Writer, Event, Event) {
    let mut db = Writer::from_seed(SEED);
    let e1 = db
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(1_750_000_000_000)),
            ("subject", Value::text("Joe's Pizza")),
            ("body", Value::text("best slice in town")),
        ]))
        .unwrap();
    let e2 = db
        .claim(Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(1_750_000_100_000)),
            ("subject", Value::text("Blue Bottle")),
        ]))
        .unwrap();
    (db, e1, e2)
}

#[test]
fn writer_reproduces_the_vectors_exactly() {
    let (db, e1, e2) = vector_writer();
    assert_eq!(format!("{}", db.id()), LOG_ID_HEX);

    assert_eq!(e1.header_bytes, unhex(E1_HEADER_HEX));
    assert_eq!(e1.tag.to_vec(), unhex(E1_TAG_HEX));
    assert_eq!(
        e1.body_bytes.as_deref(),
        Some(unhex(E1_BODY_HEX).as_slice())
    );
    assert_eq!(format!("{}", e1.id()), E1_ID_HEX);

    assert_eq!(e2.header_bytes, unhex(E2_HEADER_HEX));
    assert_eq!(format!("{}", e2.id()), E2_ID_HEX);
}

#[test]
fn vector_bytes_decode_authenticate_and_reencode() {
    let event = Event {
        header_bytes: unhex(E1_HEADER_HEX),
        tag: unhex(E1_TAG_HEX).try_into().unwrap(),
        body_bytes: Some(unhex(E1_BODY_HEX)),
    };
    let claim = event.check().expect("vector must be well-formed");
    assert_eq!(claim.header.version, WIRE_VERSION);
    assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
    assert_eq!(claim.header.id(), event.id());
    // The tag is deterministic HMAC, so it is itself a vector: the derived
    // auth key must both verify it and reproduce it byte-for-byte.
    assert!(event.verify_tag(&auth_key()));
    assert_eq!(event.tag, header_tag(&auth_key(), &event.header_bytes));
}

#[test]
fn the_second_vector_decodes() {
    let header2 = EventHeader::decode(&unhex(E2_HEADER_HEX)).unwrap();
    assert_eq!(format!("{}", header2.log_id), LOG_ID_HEX);
}

#[test]
fn map_keys_sort_by_encoded_bytes_in_the_vector() {
    // The body's canonical key order is at, body, type, subject —
    // length-first (2, 4, 4, 7), then bytewise. A plain string sort would
    // give at, body, subject, type; getting this wrong is the most likely
    // cross-language canonicalization bug, so the vector pins it.
    let bytes = unhex(E1_BODY_HEX);
    let at_pos = bytes.windows(3).position(|w| w == b"\x62at").unwrap();
    let body_pos = bytes.windows(4).position(|w| w == b"body").unwrap();
    let type_pos = bytes.windows(4).position(|w| w == b"type").unwrap();
    let subject_pos = bytes.windows(7).position(|w| w == b"subject").unwrap();
    assert!(at_pos < body_pos && body_pos < type_pos && type_pos < subject_pos);
}

#[test]
fn tombstone_of_the_vector_still_authenticates() {
    let (_, e1, _) = vector_writer();
    let tomb = e1.without_body();
    let claim = tomb.check().unwrap();
    assert!(claim.body.is_none());
    assert!(tomb.verify_tag(&auth_key()));
    assert_eq!(format!("{}", tomb.id()), E1_ID_HEX);
}
