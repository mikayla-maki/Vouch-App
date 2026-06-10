//! Conformance test vectors.
//!
//! These constants are the cross-language contract: an implementation in any
//! language must produce these exact bytes for these exact claims. If a
//! change to vouch-core breaks this test, that change is a wire-format break
//! and needs a version bump and a migration story — not an updated constant.

use vouch_core::{EventHeader, SignedEvent, Value, WIRE_VERSION, Writer};

const SEED: [u8; 32] = [0x42; 32];

const LOG_ID_HEX: &str = "2152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12";

/// Claim 1: seq 1, ts 1750000000000,
/// body {type:"rec", subject:"Joe's Pizza", body:"best slice in town"}.
const E1_HEADER_HEX: &str = "850158202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12011b000001977420dc005820573d6f4a832745d6af9233d330c2260dc68c7b2f647a6dce23ed3c9ea51de08b";
const E1_SIG_HEX: &str = "85ae8f06c477cdeb54ce26bb0f1ee0bb21023b6f233e7b593b33b50232e3fd79a086ff28c428a6eb369543237485a0dbb7d2e1405440ada32c863c12a27b0c0f";
const E1_BODY_HEX: &str = "a364626f6479726265737420736c69636520696e20746f776e647479706563726563677375626a6563746b4a6f6527732050697a7a61";
const E1_ID_HEX: &str = "2bf40161424da6d64588e5c3dafafc1301b187b7f714f28c09752444248a2816";

/// Claim 2: seq 2, ts 1750000100000,
/// body {type:"rec", subject:"Blue Bottle"}.
const E2_HEADER_HEX: &str = "850158202152f8d19b791d24453242e15f2eab6cb7cffa7b6a5ed30097960e069881db12021b00000197742262a058201762b10e3d0475434fd49a3bfe7d41063d32b04dd173d335763fe332545181ba";
const E2_ID_HEX: &str = "ab27f49d6207b7f7adcfeae777e605165cd57c7f5de08d7c00ef9734447c177f";

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

fn vector_writer() -> (Writer, SignedEvent, SignedEvent) {
    let mut db = Writer::from_seed(SEED);
    let e1 = db
        .claim(
            1_750_000_000_000,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Joe's Pizza")),
                ("body", Value::text("best slice in town")),
            ]),
        )
        .unwrap();
    let e2 = db
        .claim(
            1_750_000_100_000,
            Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Blue Bottle")),
            ]),
        )
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
    assert_eq!(claim.header.sequence, 1);
    assert_eq!(claim.header.timestamp, 1_750_000_000_000);
    assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
    assert_eq!(claim.header.id(), event.id());
}

#[test]
fn sequence_advances_in_the_second_vector() {
    let header2 = EventHeader::decode(&unhex(E2_HEADER_HEX)).unwrap();
    assert_eq!(header2.sequence, 2);
    assert_eq!(header2.timestamp, 1_750_000_100_000);
}

#[test]
fn map_keys_sort_by_encoded_bytes_in_the_vector() {
    // The body's canonical key order is body, type, subject — length-first
    // (4, 4, 7), then bytewise. A plain string sort would give body,
    // subject, type; getting this wrong is the most likely cross-language
    // canonicalization bug, so the vector pins it.
    let bytes = unhex(E1_BODY_HEX);
    let body_pos = bytes.windows(4).position(|w| w == b"body").unwrap();
    let type_pos = bytes.windows(4).position(|w| w == b"type").unwrap();
    let subject_pos = bytes.windows(7).position(|w| w == b"subject").unwrap();
    assert!(body_pos < type_pos && type_pos < subject_pos);
}

#[test]
fn tombstone_of_the_vector_still_verifies() {
    let (_, e1, _) = vector_writer();
    let tomb = e1.without_body();
    let claim = tomb.verify().unwrap();
    assert!(claim.body.is_none());
    assert_eq!(format!("{}", tomb.id()), E1_ID_HEX);
}
