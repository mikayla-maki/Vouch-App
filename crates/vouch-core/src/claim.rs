//! Claims: signed headers referencing bodies by hash.
//!
//! A claim is split in two, and the split is load-bearing:
//!
//! ```text
//! header = [ uint version, bytes-32 log_id, bytes-32 body_hash ]
//! signature = Ed25519::sign(signing_key, canonical_header_bytes)
//! id        = BLAKE3(canonical_header_bytes)
//! body      = canonical CBOR map, shipped alongside, pinned by body_hash
//! ```
//!
//! The signature covers the header; the header pins the body. So a body can
//! be *dropped* (redaction) while the header — the claim's existence and
//! identity — stays verifiable forever. A header without its body is a
//! signed tombstone.
//!
//! The header is exactly what verification needs and nothing else: a
//! version to decode by, a key to verify with, a hash to pin the content.
//! Everything the author *means* — including when they claim they said it
//! (the vocabulary's `at` field) — lives in the body, transitively signed
//! via `body_hash`, and redactable with it: a tombstone reveals nothing
//! but "this key once signed something with this hash", not even when.
//! There is no sequence number and no prev pointer — sync coordinates are
//! pipe-local arrival positions (see the store), and drift between pipes
//! is caught by per-log set fingerprints.
//!
//! Identity is therefore exactly (author × content): the same author
//! signing byte-identical bodies produces ONE claim — saying the same
//! thing twice is saying it once. Corollary: redacting a claim redacts
//! those exact bytes from that author for good; "republishing" is new
//! speech (a superseding claim, a new `at`) with a new identity.

use ed25519_dalek::Signature;

use crate::cbor::{self, Decoder};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::{ClaimHash, Value};

/// Domain-separation prefix for claim signatures. The signed message is
/// `SIGNING_DOMAIN ++ canonical_header_bytes`, never the header bytes
/// alone, so a Vouch claim signature can never be replayed as a valid
/// signature over some other protocol's message under a reused key (and
/// vice versa). The claim id stays `BLAKE3(header_bytes)` — a hash needs no
/// domain separation, only a signature does. Changing this string is a wire
/// break (it changes every signature).
pub const SIGNING_DOMAIN: &[u8] = b"vouch-claim-sig-v1";

/// The exact bytes a claim signature covers: the domain prefix followed by
/// the canonical header.
pub fn signing_input(header_bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SIGNING_DOMAIN.len() + header_bytes.len());
    out.extend_from_slice(SIGNING_DOMAIN);
    out.extend_from_slice(header_bytes);
    out
}

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
    /// BLAKE3 hash of the canonical body bytes.
    pub body_hash: [u8; 32],
}

impl EventHeader {
    /// The canonical encoding: the exact bytes that are signed and hashed.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        cbor::head(&mut out, 4, 3);
        cbor::head(&mut out, 0, self.version as u64);
        cbor::head(&mut out, 2, 32);
        out.extend_from_slice(&self.log_id.0);
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
        let n = d.expect(4, "header must be a 3-element array")?;
        if n != 3 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "header must be a 3-element array",
            });
        }
        let version = d.expect(0, "version must be an unsigned integer")?;
        if version != WIRE_VERSION as u64 {
            return Err(Error::UnsupportedVersion(version));
        }
        let log_id = LogId(d.bytes32("log id must be 32 bytes")?);
        let body_hash = d.bytes32("body hash must be 32 bytes")?;
        d.done()?;
        Ok(EventHeader {
            // Checked equal to WIRE_VERSION above, so it fits u16.
            version: WIRE_VERSION,
            log_id,
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
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
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

    /// Decode and verify: signature against the domain-separated header
    /// bytes as received (using the log id from the decoded header as the
    /// verifying key), then the body against the header's body hash.
    pub fn verify(&self) -> Result<Claim, Error> {
        let header = self.header()?;
        let key = header.log_id.verifying_key()?;
        key.verify_strict(&signing_input(&self.header_bytes), &self.signature)
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
        let (event, n) = Self::decode_prefix(buf)?;
        if n != buf.len() {
            return Err(Error::Cbor {
                offset: n,
                reason: "trailing bytes after value",
            });
        }
        Ok(event)
    }

    /// Decode one wire frame from the front of `buf`, returning the event
    /// and the bytes consumed. This is how a CBOR sequence (RFC 8742) of
    /// events — a persistence file, a backup, a batch — is read: frames are
    /// simply concatenated, and a torn final frame fails to decode without
    /// corrupting anything before it.
    pub fn decode_prefix(buf: &[u8]) -> Result<(SignedEvent, usize), Error> {
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
        Ok((
            SignedEvent {
                header_bytes,
                signature,
                body_bytes,
            },
            d.pos(),
        ))
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
            .claim(Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("x")),
            ]))
            .unwrap();
        let claim = event.verify().unwrap();
        assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
        assert_eq!(claim.header.version, WIRE_VERSION);
        assert_eq!(claim.header.id(), event.id());
    }

    #[test]
    fn tampered_header_fails_verification() {
        let mut db = Writer::from_seed([2; 32]);
        let mut event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let last = event.header_bytes.len() - 1;
        event.header_bytes[last] ^= 1;
        assert!(event.verify().is_err());
    }

    #[test]
    fn tampered_body_fails_the_hash_pin() {
        let mut db = Writer::from_seed([2; 32]);
        let mut event = db
            .claim(Value::map([("subject", Value::text("Joe's"))]))
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
        assert!(matches!(db.claim(huge), Err(Error::BodyTooLarge(_))));

        // ...and the verifier won't accept one, before spending any work
        // hashing it (so the size check also bounds adversarial input).
        let mut event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        event.body_bytes = Some(vec![0; MAX_BODY_SIZE + 1]);
        assert!(matches!(event.verify(), Err(Error::BodyTooLarge(_))));

        // A generous-but-legal body is fine.
        let big = Value::map([("body", Value::text("x".repeat(MAX_BODY_SIZE - 64)))]);
        db.claim(big).unwrap().verify().unwrap();
    }

    #[test]
    fn tombstone_verifies_without_body() {
        let mut db = Writer::from_seed([3; 32]);
        let event = db
            .claim(Value::map([("type", Value::text("rec"))]))
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
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        for e in [event.clone(), event.without_body()] {
            let back = SignedEvent::decode(&e.encode()).unwrap();
            assert_eq!(back, e);
            back.verify().unwrap();
        }
    }

    #[test]
    fn signature_is_domain_separated() {
        // The signature covers SIGNING_DOMAIN ++ header, never the bare
        // header — so the raw signature over the header bytes alone (what a
        // different protocol reusing the key might produce) does NOT verify
        // as a Vouch claim.
        use ed25519_dalek::{Signer, SigningKey};
        let key = SigningKey::from_bytes(&[3; 32]);
        let mut db = Writer::from_seed([3; 32]);
        let event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();

        // A signature over the bare header bytes (no domain prefix).
        let bare = SignedEvent {
            header_bytes: event.header_bytes.clone(),
            signature: key.sign(&event.header_bytes),
            body_bytes: event.body_bytes.clone(),
        };
        assert!(matches!(bare.verify(), Err(Error::BadSignature { .. })));
        // The properly domain-separated one verifies.
        event.verify().unwrap();
    }

    #[test]
    fn writer_refuses_to_sign_an_undecodable_body() {
        // A BlobRef size beyond i64::MAX encodes (u64) but no conformant
        // decoder accepts it. The writer round-trips before signing, so it
        // refuses rather than mint a permanently-unverifiable claim.
        use crate::value::{BlobHash, BlobRef};
        let mut db = Writer::from_seed([5; 32]);
        let body = Value::map([(
            "photo",
            Value::BlobRef(BlobRef {
                hash: BlobHash([1; 32]),
                size: u64::MAX,
                mime: "image/png".into(),
            }),
        )]);
        assert!(matches!(db.claim(body), Err(Error::Cbor { .. })));
    }

    #[test]
    fn unsupported_version_reports_the_claimed_value_faithfully() {
        // A version that doesn't fit the u16 field must be reported as-is,
        // not silently truncated to 65535.
        let mut bytes = Vec::new();
        cbor::head(&mut bytes, 4, 3);
        cbor::head(&mut bytes, 0, 999_999); // version, way out of u16 range
        cbor::head(&mut bytes, 2, 32);
        bytes.extend_from_slice(&[0u8; 32]);
        cbor::head(&mut bytes, 2, 32);
        bytes.extend_from_slice(&[0u8; 32]);
        assert!(matches!(
            EventHeader::decode(&bytes),
            Err(Error::UnsupportedVersion(999_999))
        ));
    }
}
