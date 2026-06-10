//! Claims: signed headers referencing bodies by hash.
//!
//! A claim is split in two, and the split is load-bearing:
//!
//! ```text
//! header = [ uint version, bytes-32 log_id, uint sequence,
//!            int timestamp_ms, bytes-32 body_hash ]
//! signature = Ed25519::sign(signing_key, canonical_header_bytes)
//! id        = BLAKE3(canonical_header_bytes)
//! body      = canonical CBOR map, shipped alongside, pinned by body_hash
//! ```
//!
//! The signature covers the header; the header pins the body. So a body can
//! be *dropped* (redaction) while the header — the claim's existence, place,
//! and identity — stays verifiable forever. A header without its body is a
//! signed tombstone.
//!
//! `sequence` is **advisory**: a shared language for ordering and sync
//! between cooperating clients, never an enforced invariant. Identity is
//! the hash; duplicate sequences are valid data, not errors. (Drift that
//! sequence numbers can't see — a writer reusing numbers after a crash —
//! is caught by the sync layer's set fingerprints, not by the header.)

use ed25519_dalek::Signature;

use crate::cbor::{self, Decoder};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::{ClaimHash, Value};

/// Current wire-format version. Structural changes to the signed layout bump
/// this; new claim types and fields never do.
pub const WIRE_VERSION: u16 = 1;

/// Maximum encoded body size in bytes. A normative wire-format rule, not a
/// courtesy: stores must agree on which claims are valid or their
/// fingerprints diverge, so writers refuse to sign past this and verifiers
/// refuse to accept past it.
///
/// 64 KiB is roughly ten thousand words — a short story per claim, or
/// thousands of refs. A body is one piece of speech; bulk data (images,
/// media) belongs in content-addressed blobs pinned from the body by hash,
/// which is what keeps logs small enough to full-sync and hold in memory.
pub const MAX_BODY_SIZE: usize = 64 * 1024;

/// The signed metadata of a claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventHeader {
    pub version: u16,
    pub log_id: LogId,
    /// Advisory position in the author's log, starting at 1. A shared
    /// coordinate for cooperating clients; never unique by guarantee.
    pub sequence: u64,
    /// Author-claimed creation time, Unix milliseconds. For display, never
    /// for correctness.
    pub timestamp: i64,
    /// BLAKE3 hash of the canonical body bytes.
    pub body_hash: [u8; 32],
}

impl EventHeader {
    /// The canonical encoding: the exact bytes that are signed and hashed.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        cbor::head(&mut out, 4, 5);
        cbor::head(&mut out, 0, self.version as u64);
        cbor::head(&mut out, 2, 32);
        out.extend_from_slice(&self.log_id.0);
        cbor::head(&mut out, 0, self.sequence);
        cbor::encode_int(&mut out, self.timestamp);
        cbor::head(&mut out, 2, 32);
        out.extend_from_slice(&self.body_hash);
        out
    }

    /// The claim id this header encodes to.
    pub fn id(&self) -> ClaimHash {
        ClaimHash(*blake3::hash(&self.canonical_bytes()).as_bytes())
    }

    /// Decode canonical header bytes. Strict: non-canonical input is
    /// rejected, so `decode(bytes).canonical_bytes() == bytes` always holds.
    pub fn decode(bytes: &[u8]) -> Result<EventHeader, Error> {
        let mut d = Decoder::new(bytes);
        let n = d.expect(4, "header must be a 5-element array")?;
        if n != 5 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "header must be a 5-element array",
            });
        }
        let version = d.expect(0, "version must be an unsigned integer")?;
        let version = u16::try_from(version).map_err(|_| Error::UnsupportedVersion(u16::MAX))?;
        if version != WIRE_VERSION {
            return Err(Error::UnsupportedVersion(version));
        }
        let log_id = LogId(d.bytes32("database id must be 32 bytes")?);
        let sequence = d.expect(0, "sequence must be an unsigned integer")?;
        let timestamp = d.int("timestamp must be an integer")?;
        let body_hash = d.bytes32("body hash must be 32 bytes")?;
        d.done()?;
        Ok(EventHeader {
            version,
            log_id,
            sequence,
            timestamp,
            body_hash,
        })
    }
}

/// A decoded claim: header plus body, where `body: None` is a tombstone —
/// the claim verifiably existed, its content is gone (redacted, or simply
/// not transferred yet).
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub header: EventHeader,
    pub body: Option<Value>,
}

/// A claim as transmitted and stored: canonical header bytes, the signature
/// over them, and (unless tombstoned) the canonical body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedEvent {
    pub header_bytes: Vec<u8>,
    pub signature: Signature,
    pub body_bytes: Option<Vec<u8>>,
}

impl SignedEvent {
    /// The claim's identity: BLAKE3 of the header bytes. Cheap, and valid
    /// even before verification (it's an address, not a judgment).
    pub fn id(&self) -> ClaimHash {
        ClaimHash(*blake3::hash(&self.header_bytes).as_bytes())
    }

    /// Decode the header without checking the signature.
    pub fn header(&self) -> Result<EventHeader, Error> {
        EventHeader::decode(&self.header_bytes)
    }

    /// Decode and verify: signature against the header bytes as received
    /// (using the log id from the decoded header as the verifying
    /// key), then the body against the header's body hash.
    pub fn verify(&self) -> Result<Claim, Error> {
        let header = self.header()?;
        let key = header.log_id.verifying_key()?;
        key.verify_strict(&self.header_bytes, &self.signature)
            .map_err(|_| Error::BadSignature {
                log_id: header.log_id,
            })?;
        let body = match &self.body_bytes {
            None => None,
            Some(bytes) => {
                if bytes.len() > MAX_BODY_SIZE {
                    return Err(Error::BodyTooLarge(bytes.len()));
                }
                if *blake3::hash(bytes).as_bytes() != header.body_hash {
                    return Err(Error::BodyHashMismatch);
                }
                let value = cbor::from_bytes(bytes)?;
                if !value.is_map() {
                    return Err(Error::BodyNotMap);
                }
                Some(value)
            }
        };
        Ok(Claim { header, body })
    }

    /// This event as a signed tombstone: same header and signature, body
    /// dropped.
    pub fn without_body(&self) -> SignedEvent {
        SignedEvent {
            header_bytes: self.header_bytes.clone(),
            signature: self.signature,
            body_bytes: None,
        }
    }

    /// Wire encoding for standalone transmission:
    /// `[bytes header, bytes-64 signature, bytes body | null]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        cbor::encode_signed_event(&mut out, self);
        out
    }

    /// Decode the standalone wire form.
    pub fn decode(buf: &[u8]) -> Result<SignedEvent, Error> {
        let mut d = Decoder::new(buf);
        let n = d.expect(4, "signed event must be a 3-element array")?;
        if n != 3 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "signed event must be a 3-element array",
            });
        }
        let hlen = d.expect(2, "header must be a byte string")?;
        let header_bytes = d.take(hlen)?.to_vec();
        let slen = d.expect(2, "signature must be a byte string")?;
        if slen != 64 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "signature must be 64 bytes",
            });
        }
        let signature = Signature::from_slice(d.take(64)?).map_err(|_| Error::Cbor {
            offset: 0,
            reason: "invalid signature bytes",
        })?;
        let body_bytes = if d.peek_null() {
            d.skip_null();
            None
        } else {
            let blen = d.expect(2, "body must be null or a byte string")?;
            Some(d.take(blen)?.to_vec())
        };
        d.done()?;
        Ok(SignedEvent {
            header_bytes,
            signature,
            body_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::writer::Writer;

    #[test]
    fn decode_is_strict_inverse_of_encode() {
        let mut db = Writer::from_seed([1; 32]);
        let event = db
            .claim(
                1_700_000_000_000,
                Value::map([("type", Value::text("rec")), ("subject", Value::text("x"))]),
            )
            .unwrap();
        let claim = event.verify().unwrap();
        assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
        assert_eq!(claim.header.sequence, 1);
        assert_eq!(claim.header.version, WIRE_VERSION);
        assert_eq!(claim.header.id(), event.id());
    }

    #[test]
    fn tampered_header_fails_verification() {
        let mut db = Writer::from_seed([2; 32]);
        let mut event = db
            .claim(0, Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let last = event.header_bytes.len() - 1;
        event.header_bytes[last] ^= 1;
        assert!(event.verify().is_err());
    }

    #[test]
    fn tampered_body_fails_the_hash_pin() {
        let mut db = Writer::from_seed([2; 32]);
        let mut event = db
            .claim(0, Value::map([("subject", Value::text("Joe's"))]))
            .unwrap();
        let body = event.body_bytes.as_mut().unwrap();
        let pos = body.windows(3).position(|w| w == b"Joe").unwrap();
        body[pos] = b'M';
        assert!(matches!(event.verify(), Err(Error::BodyHashMismatch)));
    }

    #[test]
    fn oversized_bodies_are_refused_at_both_ends() {
        let mut db = Writer::from_seed([4; 32]);

        // The writer won't sign one...
        let huge = Value::map([("body", Value::text("x".repeat(MAX_BODY_SIZE + 1)))]);
        assert!(matches!(db.claim(0, huge), Err(Error::BodyTooLarge(_))));

        // ...and the verifier won't accept one, before spending any work
        // hashing it (so the size check also bounds adversarial input).
        let mut event = db
            .claim(0, Value::map([("type", Value::text("rec"))]))
            .unwrap();
        event.body_bytes = Some(vec![0; MAX_BODY_SIZE + 1]);
        assert!(matches!(event.verify(), Err(Error::BodyTooLarge(_))));

        // A generous-but-legal body is fine.
        let big = Value::map([("body", Value::text("x".repeat(MAX_BODY_SIZE - 64)))]);
        db.claim(0, big).unwrap().verify().unwrap();
    }

    #[test]
    fn tombstone_verifies_without_body() {
        let mut db = Writer::from_seed([3; 32]);
        let event = db
            .claim(7, Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let tomb = event.without_body();
        let claim = tomb.verify().unwrap();
        assert!(claim.body.is_none());
        assert_eq!(tomb.id(), event.id());
    }

    #[test]
    fn signed_event_wire_roundtrip_with_and_without_body() {
        let mut db = Writer::from_seed([3; 32]);
        let event = db
            .claim(7, Value::map([("type", Value::text("rec"))]))
            .unwrap();
        for e in [event.clone(), event.without_body()] {
            let back = SignedEvent::decode(&e.encode()).unwrap();
            assert_eq!(back, e);
            back.verify().unwrap();
        }
    }
}
