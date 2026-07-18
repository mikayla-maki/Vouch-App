//! Per-log end-to-end encryption: bodies sealed to a log's content key,
//! the key conveyed to chosen readers by grant claims.
//!
//! Everything here is *vocabulary*, not wire format. An encrypted claim
//! is an ordinary signed claim whose body is the tiny envelope map
//! `{type: "enc", n: <nonce>, ct: <ciphertext>}` — sync, relays,
//! fingerprints, and redaction never look inside a body, so nothing
//! below this module changes. Granularity is the log: one log, one key,
//! one audience. Want a different boundary? Mint a different log —
//! there is deliberately no per-claim visibility state.
//!
//! There is no plaintext user content, full stop — profiles and names
//! included (your name resolves for people you've granted, and nobody
//! else). The only cleartext vocabulary on the wire is what the engine
//! itself must read before any key exists: the `grant` wrapper (its
//! payload is sealed-box ciphertext; the wrapper must be visible or
//! nobody could ever bootstrap) and `redact` (a relay must honor
//! tombstones it cannot decrypt).
//!
//! - **Content key**: derived from the log's signing seed
//!   (HKDF-SHA256), so every device holding the seed holds the key with
//!   nothing stored or synced. Rotation is a later, additive concern
//!   (the reserved `KeyRotation` claim) — which also means revoking a
//!   reader is deferred: un-granting someone does not un-grant them.
//! - **Grants**: the content key sealed to a reader's LogId (their
//!   Ed25519 key converted to X25519, ephemeral-DH sealed-box),
//!   published as a `{type: "grant", sealed: ...}` claim in the
//!   granter's own log. Grants name no recipient — readers trial-open
//!   every grant in logs they follow, so a log's audience list never
//!   leaks. Sealing is deterministic per (seed, recipient, key), so
//!   re-granting produces a byte-identical claim that content-address
//!   dedupes to nothing.
//! - **No forward secrecy, on purpose**: recommendations are durable
//!   speech; a ratchet's lost-phone-lost-history trade is wrong here.

use std::collections::BTreeMap;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use sha2::Sha256;

use crate::cbor;
use crate::draft::Draft;
use crate::fold::ClaimView;
use crate::keys::LogId;
use crate::store::ClaimStore;
use crate::value::Value;

/// The vocabulary type of an encrypted-body envelope.
pub const ENC_TYPE: &str = "enc";
/// The vocabulary type of a key grant.
pub const GRANT_TYPE: &str = "grant";

/// A log's symmetric content key.
pub type ContentKey = [u8; 32];

fn hkdf(ikm: &[u8], info_parts: &[&[u8]]) -> [u8; 32] {
    let hk = Hkdf::<Sha256>::new(None, ikm);
    let info: Vec<u8> = info_parts.concat();
    let mut out = [0u8; 32];
    hk.expand(&info, &mut out).expect("32 bytes is a valid HKDF length");
    out
}

/// The crypto side of a log identity — the same seed a [`Writer`] signs
/// with, viewed through X25519 for sealing and through HKDF for the
/// content key. Cheap to construct; hold it wherever the seed lives.
///
/// [`Writer`]: crate::Writer
#[derive(Clone)]
pub struct Identity {
    seed: [u8; 32],
    signing: SigningKey,
}

impl Identity {
    pub fn from_seed(seed: [u8; 32]) -> Identity {
        Identity {
            seed,
            signing: SigningKey::from_bytes(&seed),
        }
    }

    pub fn log_id(&self) -> LogId {
        LogId(self.signing.verifying_key().to_bytes())
    }

    /// This log's content key: derived, never stored, identical on every
    /// device holding the seed.
    pub fn content_key(&self) -> ContentKey {
        hkdf(&self.seed, &[b"vouch content key v1"])
    }

    /// Seal this log's content key to a reader. Deterministic per
    /// (seed, recipient, key): the ephemeral secret is derived rather
    /// than random, so re-granting yields identical bytes (and a later
    /// rotated key yields a fresh ephemeral, never reusing a keystream).
    /// `None` if the recipient id isn't a valid Ed25519 point.
    pub fn grant_for(&self, recipient: LogId) -> Option<Vec<u8>> {
        let recipient_ed = recipient.verifying_key().ok()?;
        let recipient_x = x25519_dalek::PublicKey::from(recipient_ed.to_montgomery().to_bytes());
        let payload = self.content_key();

        let eph_seed = hkdf(
            &self.seed,
            &[b"vouch grant ephemeral v1", &recipient.0, &payload],
        );
        let eph_secret = x25519_dalek::StaticSecret::from(eph_seed);
        let eph_public = x25519_dalek::PublicKey::from(&eph_secret);
        let shared = eph_secret.diffie_hellman(&recipient_x);
        let seal_key = hkdf(
            shared.as_bytes(),
            &[b"vouch seal v1", eph_public.as_bytes(), recipient_x.as_bytes()],
        );

        // The seal key is unique per (ephemeral, recipient, payload), so a
        // fixed nonce cannot reuse a keystream across distinct plaintexts.
        let cipher = XChaCha20Poly1305::new((&seal_key).into());
        let ct = cipher
            .encrypt(XNonce::from_slice(&[0u8; 24]), payload.as_slice())
            .ok()?;

        let mut sealed = eph_public.as_bytes().to_vec();
        sealed.extend_from_slice(&ct);
        Some(sealed)
    }

    /// Try to open a sealed grant addressed to this identity. `None`
    /// means "not for me" — the normal case while trial-decrypting a
    /// followed log's grants.
    pub fn open_grant(&self, sealed: &[u8]) -> Option<ContentKey> {
        if sealed.len() < 32 {
            return None;
        }
        let eph_bytes: [u8; 32] = sealed[..32].try_into().ok()?;
        let eph_public = x25519_dalek::PublicKey::from(eph_bytes);
        let my_secret = x25519_dalek::StaticSecret::from(self.signing.to_scalar_bytes());
        let my_public = x25519_dalek::PublicKey::from(
            self.signing.verifying_key().to_montgomery().to_bytes(),
        );
        let shared = my_secret.diffie_hellman(&eph_public);
        let seal_key = hkdf(
            shared.as_bytes(),
            &[b"vouch seal v1", eph_public.as_bytes(), my_public.as_bytes()],
        );
        let cipher = XChaCha20Poly1305::new((&seal_key).into());
        let plain = cipher
            .decrypt(XNonce::from_slice(&[0u8; 24]), &sealed[32..])
            .ok()?;
        plain.try_into().ok()
    }
}

/// Encrypt a plaintext body into the envelope map. The nonce is random —
/// the content key encrypts many bodies.
pub fn encrypt_body(key: &ContentKey, body: &Value) -> Result<Value, crate::Error> {
    let mut nonce = [0u8; 24];
    getrandom::fill(&mut nonce).map_err(|_| crate::Error::Randomness)?;
    let plain = cbor::to_bytes(body);
    let cipher = XChaCha20Poly1305::new(key.into());
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), plain.as_slice())
        .expect("XChaCha20-Poly1305 encryption is total");
    Ok(Value::map([
        ("type", Value::text(ENC_TYPE)),
        ("n", Value::Bytes(nonce.to_vec())),
        ("ct", Value::Bytes(ct)),
    ]))
}

/// Seal a draft: encrypt the body it would have signed and return the
/// envelope draft to mint instead. The `at` timestamp and every field
/// ride inside the ciphertext; only the envelope shape is public.
pub fn seal_draft(key: &ContentKey, draft: &Draft) -> Result<Draft, crate::Error> {
    let plain = draft.body_value();
    let Value::Map(envelope) = encrypt_body(key, &plain)? else {
        unreachable!("encrypt_body always returns a map");
    };
    let mut out = Draft::new(ENC_TYPE);
    for (k, v) in envelope {
        if k != "type" {
            out = out.field(k, v);
        }
    }
    Ok(out)
}

/// Every content key this identity can currently use: its own (derived)
/// plus one per grant it can open, trial-decrypting every `grant` claim
/// in the store. Grants name no recipient, so "not for me" is the
/// silent, normal case.
pub fn collect_keys(store: &ClaimStore, identity: &Identity) -> BTreeMap<LogId, ContentKey> {
    let mut keys = BTreeMap::new();
    keys.insert(identity.log_id(), identity.content_key());
    for claim in store.by_type(GRANT_TYPE) {
        let Some(Value::Map(map)) = &claim.body else {
            continue;
        };
        let Some(Value::Bytes(sealed)) = map.get("sealed") else {
            continue;
        };
        if let Some(key) = identity.open_grant(sealed) {
            keys.entry(claim.header.log_id).or_insert(key);
        }
    }
    keys
}

/// The fold's input: exactly the envelopes this identity can open, and
/// nothing else. There is no plaintext-content concept — user speech is
/// always sealed on the wire, so an unencrypted claim is either engine
/// vocabulary (`grant`, `redact` — read by the machinery, not the fold)
/// or noise, and neither belongs in the view. Ciphertext you lack the
/// key for is likewise absent: not part of your perceptible truth.
/// References are recomputed from the decrypted plaintext — a sealed
/// claim's edges are invisible to ingest-time indexes by design.
pub fn decrypted_view(store: &ClaimStore, keys: &BTreeMap<LogId, ContentKey>) -> Vec<ClaimView> {
    let mut view = Vec::new();
    for claim in store.timeline() {
        let Some(Value::Map(map)) = &claim.body else {
            continue;
        };
        if !matches!(map.get("type"), Some(Value::Text(t)) if t == ENC_TYPE) {
            continue; // engine vocabulary or junk — never user content
        }
        let Some(key) = keys.get(&claim.header.log_id) else {
            continue; // ciphertext without a key: not part of this view
        };
        let Some(Value::Map(plain)) = decrypt_body(key, map) else {
            continue; // wrong key or tampered: nothing legible here
        };
        let (refs, _, _) = Value::Map(plain.clone()).collect_refs();
        view.push(ClaimView {
            id: claim.signed.id(),
            author: claim.header.log_id,
            received_at: claim.received_at,
            body: plain,
            refs: refs.into_iter().map(|(_, r)| r.hash).collect(),
        });
    }
    view
}

/// Decrypt an envelope map back to its plaintext body. `None` for wrong
/// key, tampering, or a malformed envelope.
pub fn decrypt_body(key: &ContentKey, envelope: &BTreeMap<String, Value>) -> Option<Value> {
    let Some(Value::Bytes(nonce)) = envelope.get("n") else {
        return None;
    };
    let Some(Value::Bytes(ct)) = envelope.get("ct") else {
        return None;
    };
    let nonce: [u8; 24] = nonce.as_slice().try_into().ok()?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let plain = cipher.decrypt(XNonce::from_slice(&nonce), ct.as_slice()).ok()?;
    let value = cbor::from_bytes(&plain).ok()?;
    value.is_map().then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn envelope_roundtrips_and_wrong_key_fails() {
        let alice = Identity::from_seed([1; 32]);
        let body = Value::map([
            ("type", Value::text("rec")),
            ("subject", Value::text("Joe's Pizza")),
        ]);
        let envelope = encrypt_body(&alice.content_key(), &body).unwrap();
        let Value::Map(map) = &envelope else { panic!() };
        assert_eq!(map.get("type"), Some(&Value::text(ENC_TYPE)));

        assert_eq!(decrypt_body(&alice.content_key(), map), Some(body));
        let mallory = Identity::from_seed([6; 32]);
        assert_eq!(decrypt_body(&mallory.content_key(), map), None);
    }

    #[test]
    fn grants_open_only_for_their_recipient_and_are_deterministic() {
        let alice = Identity::from_seed([1; 32]);
        let bob = Identity::from_seed([2; 32]);
        let mallory = Identity::from_seed([6; 32]);

        let sealed = alice.grant_for(bob.log_id()).unwrap();
        assert_eq!(bob.open_grant(&sealed), Some(alice.content_key()));
        assert_eq!(mallory.open_grant(&sealed), None);

        // Deterministic: re-granting is byte-identical, so the resulting
        // claim content-address dedupes instead of accumulating.
        assert_eq!(alice.grant_for(bob.log_id()).unwrap(), sealed);
    }

    #[test]
    fn content_key_is_a_pure_function_of_the_seed() {
        assert_eq!(
            Identity::from_seed([7; 32]).content_key(),
            Identity::from_seed([7; 32]).content_key()
        );
        assert_ne!(
            Identity::from_seed([7; 32]).content_key(),
            Identity::from_seed([8; 32]).content_key()
        );
    }
}
