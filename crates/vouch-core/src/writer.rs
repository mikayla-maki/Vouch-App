//! The writing side of a log: just key material, no position or data.

use ed25519_dalek::SigningKey;

use crate::claim::{self, AuthKey, Event, EventHeader, MAX_BODY_SIZE, WIRE_VERSION};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::Value;
use crate::{cbor, e2ee};

/// The one writer of a log you own: holds the key material, appends
/// MAC-tagged claims. It holds no data and no position — a pure pen. There
/// is nothing to resume after a crash and nothing two devices restored from
/// the same mnemonic can disagree about: a writer's entire state is the
/// seed.
///
/// The signing key names the log (LogId = the Ed25519 public key) and gates
/// publishing at the transport; claims themselves are authenticated by the
/// derived MAC key — deniably, to the audience only. See
/// [`claim`](crate::claim)'s module doc for why.
pub struct Writer {
    signing_key: SigningKey,
    tag_key: AuthKey,
}

impl Writer {
    /// Generate a fresh identity from OS randomness.
    pub fn generate() -> Result<Writer, Error> {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|_| Error::Randomness)?;
        Ok(Self::from_seed(seed))
    }

    /// Deterministic construction from a 32-byte seed (the BIP39-backed
    /// secret in the real app; fixed bytes in tests and conformance
    /// vectors).
    pub fn from_seed(seed: [u8; 32]) -> Writer {
        Writer {
            signing_key: SigningKey::from_bytes(&seed),
            tag_key: e2ee::auth_key(&e2ee::Identity::from_seed(seed).content_key()),
        }
    }

    /// The log this writer writes: its identity is the public key.
    pub fn id(&self) -> LogId {
        LogId(self.signing_key.verifying_key().to_bytes())
    }

    /// Author a claim. The body must be a CBOR map. Everything the claim
    /// says — including when it claims to be from (`at`) — is the body's
    /// business; the writer only addresses and tags it.
    pub fn claim(&mut self, body: Value) -> Result<Event, Error> {
        if !body.is_map() {
            return Err(Error::BodyNotMap);
        }
        let body_bytes = cbor::to_bytes(&body);
        if body_bytes.len() > MAX_BODY_SIZE {
            return Err(Error::BodyTooLarge(body_bytes.len()));
        }
        // Never utter what a conformant decoder would reject. The encoder is
        // total but the decoder is strict (depth cap, integers bounded to
        // i64, etc.), so a body that round-trips is guaranteed acceptable
        // everywhere — a writer can't mint a permanently-unreadable claim
        // (e.g. a BlobRef size > i64::MAX, or nesting past the depth cap).
        cbor::from_bytes(&body_bytes)?;
        let header = EventHeader {
            version: WIRE_VERSION,
            log_id: self.id(),
            body_hash: *blake3::hash(&body_bytes).as_bytes(),
        };
        let header_bytes = header.canonical_bytes();
        let tag = claim::header_tag(&self.tag_key, &header_bytes);
        Ok(Event {
            header_bytes,
            tag,
            body_bytes: Some(body_bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deterministic_identity_from_seed() {
        let a = Writer::from_seed([5; 32]);
        let b = Writer::from_seed([5; 32]);
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn body_must_be_a_map() {
        let mut db = Writer::from_seed([5; 32]);
        assert!(matches!(db.claim(Value::Int(5)), Err(Error::BodyNotMap)));
    }
}
