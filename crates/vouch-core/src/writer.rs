//! The writing side of a log: a signing key plus advisory position.

use ed25519_dalek::{Signer, SigningKey};

use crate::cbor;
use crate::claim::{EventHeader, MAX_BODY_SIZE, SignedEvent, WIRE_VERSION};
use crate::error::Error;
use crate::keys::LogId;
use crate::value::Value;

/// The one writer of a log you own: holds the signing key, appends signed
/// claims. It holds no data — claims go to a [`ClaimStore`](crate::ClaimStore)
/// like anyone else's; this is the pen, not the notebook.
///
/// The sequence counter is *advisory* — a writer that loses it (restored
/// from a mnemonic on a new device) just reuses numbers. Nothing forks,
/// nothing conflicts, nothing needs coordination; cooperating clients
/// merely get better sync hints when the writer resumes from an accurate
/// value, and the sync layer's set fingerprints catch the drift either way.
pub struct Writer {
    signing_key: SigningKey,
    next_sequence: u64,
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
    /// vectors). Starts at sequence 1.
    pub fn from_seed(seed: [u8; 32]) -> Writer {
        Writer {
            signing_key: SigningKey::from_bytes(&seed),
            next_sequence: 1,
        }
    }

    /// Resume a writer for an existing log. The sequence is an advisory
    /// hint for the claims this writer will produce — a stale value is
    /// harmless (reused numbers are just data), an accurate value keeps
    /// sync cursors useful.
    pub fn resume(seed: [u8; 32], next_sequence: u64) -> Writer {
        Writer {
            signing_key: SigningKey::from_bytes(&seed),
            next_sequence,
        }
    }

    /// The log this writer writes: its identity is the public key.
    pub fn id(&self) -> LogId {
        LogId(self.signing_key.verifying_key().to_bytes())
    }

    /// The advisory sequence the next claim will carry.
    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    /// Author and sign a claim. The body must be a CBOR map.
    pub fn claim(&mut self, timestamp_ms: i64, body: Value) -> Result<SignedEvent, Error> {
        if !body.is_map() {
            return Err(Error::BodyNotMap);
        }
        let body_bytes = cbor::to_bytes(&body);
        if body_bytes.len() > MAX_BODY_SIZE {
            return Err(Error::BodyTooLarge(body_bytes.len()));
        }
        let header = EventHeader {
            version: WIRE_VERSION,
            log_id: self.id(),
            sequence: self.next_sequence,
            timestamp: timestamp_ms,
            body_hash: *blake3::hash(&body_bytes).as_bytes(),
        };
        let header_bytes = header.canonical_bytes();
        let signature = self.signing_key.sign(&header_bytes);
        self.next_sequence += 1;
        Ok(SignedEvent {
            header_bytes,
            signature,
            body_bytes: Some(body_bytes),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequences_advance() {
        let mut db = Writer::from_seed([9; 32]);
        let a = db
            .claim(0, Value::map([("type", Value::text("rec"))]))
            .unwrap();
        let b = db
            .claim(0, Value::map([("type", Value::text("warning"))]))
            .unwrap();
        assert_eq!(a.verify().unwrap().header.sequence, 1);
        assert_eq!(b.verify().unwrap().header.sequence, 2);
    }

    #[test]
    fn deterministic_identity_from_seed() {
        let a = Writer::from_seed([5; 32]);
        let b = Writer::from_seed([5; 32]);
        assert_eq!(a.id(), b.id());
    }

    #[test]
    fn body_must_be_a_map() {
        let mut db = Writer::from_seed([5; 32]);
        assert!(matches!(db.claim(0, Value::Int(5)), Err(Error::BodyNotMap)));
    }
}
