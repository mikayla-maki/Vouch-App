//! Claims: MAC'd headers referencing bodies by hash.
//!
//! A claim is split in two, and the split is load-bearing:
//!
//! ```text
//! header = [ uint version, bytes-32 log_id, bytes-32 body_hash ]
//! tag    = HMAC-SHA256(K_auth, MAC_DOMAIN ++ canonical_header_bytes)
//! id     = BLAKE3(canonical_header_bytes)
//! body   = canonical CBOR map, shipped alongside, pinned by body_hash
//! ```
//!
//! The tag covers the header; the header pins the body. So a body can
//! be *dropped* (redaction) while the header — the claim's existence and
//! identity — stays verifiable to the audience forever. A header without
//! its body is an authenticated tombstone.
//!
//! **Why a MAC and not a signature — deniability.** `K_auth` derives from
//! the log's content key (see [`e2ee`](crate::e2ee)), so exactly the
//! audience that can read a claim can authenticate it — and, because a MAC
//! key verifies and forges with the same bytes, none of them can prove
//! authorship to anyone outside. There are no signatures on the wire at
//! all; an Ed25519 signature exists only inside ciphertext, as the payload
//! of an `attest` claim, when the author chooses to go on the record.
//! Stores, relays, and wiretaps hold bytes that prove nothing.
//!
//! It follows that authenticity is judged where reading is: at read time,
//! by key-holders ([`e2ee::decrypted_view`](crate::e2ee::decrypted_view)).
//! Ingest and relays [`check`](Event::check) structure only — the real
//! gate against strangers' bytes is the transport (publish-gated
//! mailboxes; the per-pipe log check in sync).
//!
//! The header is exactly what authentication needs and nothing else: a
//! version to decode by, a log to look the key up under, a hash to pin the
//! content. Everything the author *means* — including when they claim they
//! said it (the vocabulary's `at` field) — lives in the body, transitively
//! authenticated via `body_hash`, and redactable with it: a tombstone
//! reveals nothing but "this log once uttered something with this hash",
//! not even when. There is no sequence number and no prev pointer — sync
//! coordinates are pipe-local arrival positions (see the store), and drift
//! between pipes is caught by per-log set fingerprints.
//!
//! Identity is therefore exactly (author × content): the same author
//! uttering byte-identical bodies produces ONE claim — saying the same
//! thing twice is saying it once. (HMAC is deterministic, so the tag
//! agrees.) Corollary: redacting a claim redacts those exact bytes from
//! that author for good; "republishing" is new speech (a superseding
//! claim, a new `at`) with a new identity.

use hmac::{Hmac, Mac};
use sha2::Sha256;

use crate::cbor::{self, Decoder};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::{ClaimHash, Value};

/// A log's MAC key — derived from its content key
/// ([`e2ee::auth_key`](crate::e2ee::auth_key)), so read-audience =
/// verify-audience, always.
pub type AuthKey = [u8; 32];

/// Domain-separation prefix for claim tags. The MAC'd message is
/// `MAC_DOMAIN ++ canonical_header_bytes`, never the header bytes alone,
/// so a Vouch claim tag can never be replayed as a valid MAC over some
/// other protocol's message under a reused key (and vice versa). The claim
/// id stays `BLAKE3(header_bytes)` — a hash needs no domain separation,
/// only a keyed tag does. Changing this string is a wire break (it changes
/// every tag).
pub const MAC_DOMAIN: &[u8] = b"vouch-claim-mac-v1";

/// The tag for a canonical header under a log's auth key.
pub fn header_tag(key: &AuthKey, header_bytes: &[u8]) -> [u8; 32] {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(MAC_DOMAIN);
    mac.update(header_bytes);
    mac.finalize().into_bytes().into()
}

/// Current wire-format version. Structural changes to the authenticated
/// layout bump this; new claim types and fields never do. v2: signatures
/// left the wire (deniable claims — a 32-byte MAC tag replaced the 64-byte
/// Ed25519 signature).
pub const WIRE_VERSION: u16 = 2;

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

/// A claim as transmitted and stored: canonical header bytes, the MAC tag
/// over them, and (unless tombstoned) the canonical body bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub struct Event {
    pub header_bytes: Vec<u8>,
    pub tag: [u8; 32],
    pub body_bytes: Option<Vec<u8>>,
}

impl Event {
    /// The claim's identity: BLAKE3 of the header bytes. Cheap, and valid
    /// even before verification (it's an address, not a judgment).
    pub fn id(&self) -> ClaimHash {
        ClaimHash(*blake3::hash(&self.header_bytes).as_bytes())
    }

    /// Decode the header without judging authenticity.
    pub fn header(&self) -> Result<EventHeader, Error> {
        EventHeader::decode(&self.header_bytes)
    }

    /// Structural validity — everything a keyless party (ingest, a relay,
    /// fsck) can and must check: the header decodes, the body respects the
    /// size cap and matches the header's pin, and is a CBOR map.
    /// Deliberately NOT authenticity: that takes the audience's key
    /// ([`verify_tag`](Event::verify_tag)) and happens at read time.
    pub fn check(&self) -> Result<Claim, Error> {
        let header = self.header()?;
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

    /// Authenticate the header to the audience holding `key`: the tag under
    /// the log's auth key, compared in constant time. True means "someone
    /// holding this log's address uttered exactly this" — which convinces a
    /// reader and proves nothing to anyone else, since every reader could
    /// have computed the same tag.
    pub fn verify_tag(&self, key: &AuthKey) -> bool {
        let mut mac =
            Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
        mac.update(MAC_DOMAIN);
        mac.update(&self.header_bytes);
        mac.verify_slice(&self.tag).is_ok()
    }

    /// This event as a tombstone: same header and tag, body dropped.
    pub fn without_body(&self) -> Event {
        Event {
            header_bytes: self.header_bytes.clone(),
            tag: self.tag,
            body_bytes: None,
        }
    }

    /// Wire encoding for standalone transmission:
    /// `[bytes header, bytes-32 tag, bytes body | null]`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        cbor::encode_event(&mut out, self);
        out
    }

    /// Decode the standalone wire form.
    pub fn decode(buf: &[u8]) -> Result<Event, Error> {
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
    pub fn decode_prefix(buf: &[u8]) -> Result<(Event, usize), Error> {
        let mut d = Decoder::new(buf);
        let n = d.expect(4, "event must be a 3-element array")?;
        if n != 3 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "event must be a 3-element array",
            });
        }
        let hlen = d.expect(2, "header must be a byte string")?;
        let header_bytes = d.take(hlen)?.to_vec();
        let tlen = d.expect(2, "tag must be a byte string")?;
        if tlen != 32 {
            return Err(Error::Cbor {
                offset: 0,
                reason: "tag must be 32 bytes",
            });
        }
        let tag: [u8; 32] = d.take(32)?.try_into().expect("took exactly 32 bytes");
        let body_bytes = if d.peek_null() {
            d.skip_null();
            None
        } else {
            let blen = d.expect(2, "body must be null or a byte string")?;
            Some(d.take(blen)?.to_vec())
        };
        Ok((
            Event {
                header_bytes,
                tag,
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
        let claim = event.check().unwrap();
        assert_eq!(claim.header.canonical_bytes(), event.header_bytes);
        assert_eq!(claim.header.version, WIRE_VERSION);
        assert_eq!(claim.header.id(), event.id());
    }

    #[test]
    fn tampered_header_fails_the_structural_check() {
        let mut db = Writer::from_seed([2; 32]);
        let mut event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let last = event.header_bytes.len() - 1;
        event.header_bytes[last] ^= 1;
        // The header no longer pins the body it ships with (and no longer
        // decodes canonically) — a keyless party already rejects it.
        assert!(event.check().is_err());
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
        assert!(matches!(event.check(), Err(Error::BodyHashMismatch)));
    }

    #[test]
    fn oversized_bodies_are_refused_at_both_ends() {
        let mut db = Writer::from_seed([4; 32]);

        // The writer won't utter one...
        let huge = Value::map([("body", Value::text("x".repeat(MAX_BODY_SIZE + 1)))]);
        assert!(matches!(db.claim(huge), Err(Error::BodyTooLarge(_))));

        // ...and the checker won't accept one, before spending any work
        // hashing it (so the size check also bounds adversarial input).
        let mut event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        event.body_bytes = Some(vec![0; MAX_BODY_SIZE + 1]);
        assert!(matches!(event.check(), Err(Error::BodyTooLarge(_))));

        // A generous-but-legal body is fine.
        let big = Value::map([("body", Value::text("x".repeat(MAX_BODY_SIZE - 64)))]);
        db.claim(big).unwrap().check().unwrap();
    }

    #[test]
    fn tombstone_keeps_identity_and_authenticity_without_its_body() {
        let mut db = Writer::from_seed([3; 32]);
        let key = crate::e2ee::auth_key(&crate::e2ee::Identity::from_seed([3; 32]).content_key());
        let event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let tomb = event.without_body();
        let claim = tomb.check().unwrap();
        assert!(claim.body.is_none());
        assert_eq!(tomb.id(), event.id());
        // The tag covers the header, so redaction never orphans
        // authenticity: the audience can still tell a real tombstone from
        // an invented one.
        assert!(tomb.verify_tag(&key));
    }

    #[test]
    fn event_wire_roundtrip_with_and_without_body() {
        let mut db = Writer::from_seed([3; 32]);
        let event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();
        for e in [event.clone(), event.without_body()] {
            let back = Event::decode(&e.encode()).unwrap();
            assert_eq!(back, e);
            back.check().unwrap();
        }
    }

    #[test]
    fn tags_authenticate_to_key_holders_and_nobody_else() {
        let mut db = Writer::from_seed([3; 32]);
        let audience =
            crate::e2ee::auth_key(&crate::e2ee::Identity::from_seed([3; 32]).content_key());
        let stranger =
            crate::e2ee::auth_key(&crate::e2ee::Identity::from_seed([9; 32]).content_key());
        let event = db
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();

        // The audience's key verifies; any other key sees noise. (That the
        // SAME audience key could also have FORGED the tag is the point —
        // deniability — and is what header_tag being public API states.)
        assert!(event.verify_tag(&audience));
        assert!(!event.verify_tag(&stranger));
        assert_eq!(event.tag, header_tag(&audience, &event.header_bytes));

        // Domain separation: a MAC over the bare header bytes (what some
        // other protocol reusing the key might produce) is not a claim tag.
        let mut mac = Hmac::<Sha256>::new_from_slice(&audience).unwrap();
        mac.update(&event.header_bytes);
        let bare: [u8; 32] = mac.finalize().into_bytes().into();
        assert_ne!(event.tag, bare);

        // And a tampered tag fails even for the audience.
        let mut forged = event.clone();
        forged.tag[0] ^= 1;
        assert!(!forged.verify_tag(&audience));
    }

    #[test]
    fn writer_refuses_to_utter_an_undecodable_body() {
        // A BlobRef size beyond i64::MAX encodes (u64) but no conformant
        // decoder accepts it. The writer round-trips before tagging, so it
        // refuses rather than mint a permanently-unreadable claim.
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
