//! Conformance test vectors.
//!
//! These constants are the cross-language contract: an implementation in any
//! language must produce these exact bytes for these exact claims. If a
//! change to vouch-core breaks this test, that change is a wire-format break
//! and needs a version bump and a migration story — not an updated constant.

use vouch_core::{EventHeader, SignedEvent, Value, WIRE_VERSION, Writer};

const SEED: [u8; 32] = [0x42; 32];

const LOG_ID_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";

/// Claim 1: body {type:"rec", at:1750000000000, subject:"Joe's Pizza",
/// body:"best slice in town"}.
const E1_HEADER_HEX: &str = "830158202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db125820978a35021eac05b71205e92efc0a18bad14fcdfee3ff577a6c67840453a2b36e";
const E1_SIG_HEX: &str = "100f851c664bfe0ef7970032a5377f3c1c2306eaee9dde2e619c0f25b91e56485acb737e49e3591ffbc5383a230c1728ceb46094aeea09b07ee0225f173df304";
const E1_BODY_HEX: &str = "a46261741b000001977420dc0064626f6479726265737420736c69636520696e20746f776e647479706563726563677375626a6563746b4a6f6527732050697a7a61";
const E1_ID_HEX: &str = "a237200c0079773d1d1210211d03ef5e292b15b59980a98c2cdf263acce3b72f";

/// Claim 2: body {type:"rec", at:1750000100000, subject:"Blue Bottle"}.
const E2_HEADER_HEX: &str = "830158202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db1258202e3d24d2f4011e5abad88d4a5bafce1e879b7fe7eca08faa5e7e88e173272c0f";
const E2_ID_HEX: &str = "ac3bf475ecadc2ee691d559e099def630a84ab05aaa926ab968b15da46827c0e";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn vector_writer() -> (Writer, SignedEvent, SignedEvent) {
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
    assert_eq!(e1.signature.to_bytes().to_vec(), unhex(E1_SIG_HEX));
    assert_eq!(
        e1.body_bytes.as_deref(),
        Some(unhex(E1_BODY_HEX).as_slice())
    );
    assert_eq!(format!("{}", e1.id()), E1_ID_HEX);

    assert_eq!(e2.header_bytes, unhex(E2_HEADER_HEX));
    assert_eq!(format!("{}", e2.id()), E2_ID_HEX);
}

#[test]
fn vector_bytes_decode_verify_and_reencode() {
    let event = SignedEvent {
        header_bytes: unhex(E1_HEADER_HEX),
        signature: vouch_core::Signature::from_slice(&unhex(E1_SIG_HEX)).unwrap(),
        body_bytes: Some(unhex(E1_BODY_HEX)),
    };
    let claim = event.verify().expect("vector must verify");
    assert_eq!(claim.header.version, WIRE_VERSION);
    assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
    assert_eq!(claim.header.id(), event.id());
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
fn tombstone_of_the_vector_still_verifies() {
    let (_, e1, _) = vector_writer();
    let tomb = e1.without_body();
    let claim = tomb.verify().unwrap();
    assert!(claim.body.is_none());
    assert_eq!(format!("{}", tomb.id()), E1_ID_HEX);
}
