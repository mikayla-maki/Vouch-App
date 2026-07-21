//! Per-log end-to-end encryption: bodies sealed to a log's content key,
//! the key conveyed by the [`Address`] itself — sharing your address IS
//! the grant.
//!
//! Everything here is *vocabulary*, not wire format. An encrypted claim
//! is an ordinary MAC-tagged claim whose body is the tiny envelope map
//! `{type: "enc", n: <nonce>, ct: <ciphertext>}` — sync, relays,
//! fingerprints, and redaction never look inside a body, so nothing
//! below this module changes. Granularity is the log: one log, one key,
//! one audience. Want a different boundary? Mint a different log —
//! there is deliberately no per-claim visibility state.
//!
//! **Speech is deniable by default.** The content key also derives the
//! MAC key claims are tagged under ([`auth_key`]), so the audience that
//! can read a claim is exactly the audience that can authenticate it —
//! and none of them can prove authorship onward, because any of them
//! could have forged the tag. Escalation is a choice: an [`attest`]
//! claim (`Identity::attest`) carries the one Ed25519 signature in the
//! system, sealed inside ciphertext until someone holding the plaintext
//! discloses it — at which point strangers verify the author's exact
//! words ([`verify_attest`]) with no key at all. Publishing to a relay
//! authenticates the same deniable way ([`publish_proof`]): a DH
//! handshake the relay can check but could equally have simulated.
//!
//! [`attest`]: Identity::attest
//!
//! There is no plaintext user content, full stop — profiles and names
//! included (your name resolves for people holding your address, and
//! nobody else). The only cleartext vocabulary on the wire is what the
//! engine must read before any key exists: `redact` (a relay must honor
//! tombstones it cannot decrypt).
//!
//! - **Content key**: derived from the log's signing seed
//!   (HKDF-SHA256), so every device holding the seed holds the key with
//!   nothing stored or synced. Rotation is a later, additive concern
//!   (the reserved `KeyRotation` claim) — which also means revoking a
//!   reader is deferred: an address, once shared, stays legible.
//! - **Addresses are capabilities**: the pasteable string carries both
//!   halves — the LogId (routing: which mailbox to follow; the only
//!   half a relay ever sees) and the content key (reading). Follow ⇒
//!   read, in one paste, with no round trip back to the author. The
//!   key half never touches the wire: it lives in the paste and in the
//!   follower's local state. One-way-ness is preserved where it
//!   matters — the author learns nothing when someone follows.
//! - **Sealed-box grants** ([`Identity::grant_for`]): the primitive for
//!   conveying a content key *in-band* to a specific recipient key.
//!   Not used by follows (the address does that job out of band); kept
//!   as the building block device delegation and key rotation will
//!   ride on.
//! - **No forward secrecy, on purpose**: recommendations are durable
//!   speech; a ratchet's lost-phone-lost-history trade is wrong here.

use std::collections::BTreeMap;
use std::fmt;

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{Signer, SigningKey, Verifier};

use hkdf::Hkdf;
use sha2::Sha256;

use crate::cbor;
use crate::claim::AuthKey;
use crate::draft::Draft;
use crate::fold::ClaimView;
use crate::keys::LogId;
use crate::store::ClaimStore;
use crate::value::{ClaimHash, ClaimRef, Value};

/// The vocabulary type of an encrypted-body envelope.
pub const ENC_TYPE: &str = "enc";

/// The vocabulary type of an attestation — the going-on-the-record claim.
/// Sealed and MAC'd like all speech: minting one shows only your audience
/// you went on the record. Its `sig` payload is the one Ed25519 signature
/// in the system, and it becomes evidence exactly when someone holding the
/// plaintext chooses to show it around — disclosure is the escalation.
pub const ATTEST_TYPE: &str = "attest";

/// A log's symmetric content key.
pub type ContentKey = [u8; 32];

/// The MAC key claims are tagged under: derived from the content key, so
/// holding the address IS holding it — the verify-audience is exactly the
/// read-audience, and the deniability set is "everyone with this log's
/// address." (Derived from `K`, not the seed, precisely so the address
/// format didn't change.)
pub fn auth_key(key: &ContentKey) -> AuthKey {
    hkdf(key, &[b"vouch auth key v1"])
}

/// A shareable address: the LogId to follow plus the content key to
/// read what you find there. The string form (`vouch:` + 128 hex) is
/// the thing you text a friend — handing it over is the grant, so
/// following someone implies reading them. Share it where you'd share
/// the speech it unlocks: a log posted publicly is a public log, by
/// choice.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Address {
    pub log: LogId,
    pub key: ContentKey,
}

impl Address {
    /// Parse the string form: an optional `vouch:` prefix, then the
    /// LogId and content key as 128 hex characters. `None` for
    /// anything else — including a bare 64-hex LogId, which routes but
    /// cannot read and so is not an address.
    pub fn parse(text: &str) -> Option<Address> {
        let hex = text.trim();
        let hex = hex.strip_prefix("vouch:").unwrap_or(hex);
        if hex.len() != 128 {
            return None;
        }
        let mut bytes = [0u8; 64];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
        }
        Some(Address {
            log: LogId(bytes[..32].try_into().expect("split of 64 bytes")),
            key: bytes[32..].try_into().expect("split of 64 bytes"),
        })
    }
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "vouch:{}", self.log)?;
        for b in &self.key {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Address({})", self.log.short())
    }
}

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

    /// The address you hand to friends: LogId + content key in one
    /// string. Publishing the key half reveals nothing about the
    /// signing seed (HKDF is one-way) — read ≠ write.
    pub fn address(&self) -> Address {
        Address {
            log: self.log_id(),
            key: self.content_key(),
        }
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
    /// means "not for me" — the normal case while trial-decrypting,
    /// since grants name no recipient.
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

/// Every content key a reader can currently use: their own (derived)
/// plus one per followed address. This is what an app builds from its
/// follows list and passes to [`decrypted_view`].
pub fn keys_for(identity: &Identity, follows: &[Address]) -> BTreeMap<LogId, ContentKey> {
    let mut keys = BTreeMap::new();
    keys.insert(identity.log_id(), identity.content_key());
    for address in follows {
        keys.entry(address.log).or_insert(address.key);
    }
    keys
}

/// The fold's input: exactly the envelopes these keys open — decrypted
/// AND authenticated — and nothing else. There is no plaintext-content
/// concept — user speech is always sealed on the wire, so an unencrypted
/// claim is either engine vocabulary (`redact` — read by the machinery,
/// not the fold) or noise, and neither belongs in the view. Ciphertext
/// you lack the key for is likewise absent: not part of your perceptible
/// truth.
///
/// This is also where authenticity lives now: the header tag is checked
/// here, under the auth key derived from the same content key that opens
/// the envelope. Read time, not ingest — stores and relays are blind
/// carriers and judge nothing. A claim whose tag fails is exactly as
/// absent as one that won't decrypt.
/// References are recomputed from the decrypted plaintext — a sealed
/// claim's edges are invisible to ingest-time indexes by design.
pub fn decrypted_view(store: &ClaimStore, keys: &BTreeMap<LogId, ContentKey>) -> Vec<ClaimView> {
    let auth: BTreeMap<LogId, AuthKey> =
        keys.iter().map(|(log, key)| (*log, auth_key(key))).collect();
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
        if !claim.event.verify_tag(&auth[&claim.header.log_id]) {
            continue; // unauthenticated bytes: not speech, not visible
        }
        let Some(Value::Map(plain)) = decrypt_body(key, map) else {
            continue; // wrong key or tampered: nothing legible here
        };
        let (refs, _, _) = Value::Map(plain.clone()).collect_refs();
        view.push(ClaimView {
            id: claim.event.id(),
            author: claim.header.log_id,
            received_at: claim.received_at,
            body: plain,
            refs: refs.into_iter().map(|(_, r)| r.hash).collect(),
        });
    }
    view
}

/// One publish-auth challenge: a relay's ephemeral X25519 keypair plus a
/// nonce. The relay holds the whole struct for the round trip and sends
/// `public ‖ nonce` to the client; nothing here outlives the connection.
pub struct PublishChallenge {
    pub secret: [u8; 32],
    pub public: [u8; 32],
    pub nonce: [u8; 16],
}

/// Domain for the publish-auth session key derivation.
const PUBLISH_AUTH_DOMAIN: &[u8] = b"vouch publish auth v1";

/// Mint a fresh challenge (relay side, one per authenticating connection).
pub fn publish_challenge() -> Result<PublishChallenge, crate::Error> {
    let mut secret = [0u8; 32];
    getrandom::fill(&mut secret).map_err(|_| crate::Error::Randomness)?;
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| crate::Error::Randomness)?;
    let public = *x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(secret))
        .as_bytes();
    Ok(PublishChallenge {
        secret,
        public,
        nonce,
    })
}

/// The shared MAC key both ends of a publish handshake derive: the log's
/// Ed25519 identity viewed through X25519, DH'd against the relay's
/// ephemeral, bound to this exact challenge.
fn publish_auth_secret(shared: &[u8], log: &LogId, eph_pub: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    hkdf(shared, &[PUBLISH_AUTH_DOMAIN, &log.0, eph_pub, nonce])
}

fn proof_mac(key: &[u8; 32]) -> hmac::Hmac<Sha256> {
    use hmac::Mac;
    let mut mac = <hmac::Hmac<Sha256> as Mac>::new_from_slice(key).expect("any key length");
    mac.update(b"vouch publish proof v1");
    mac
}

/// Prove key possession for publishing (client side): answer the relay's
/// challenge with a MAC under the DH-agreed secret.
///
/// **Deniable by construction.** The relay verifies by computing the same
/// DH from its ephemeral secret — which means the relay could also have
/// *minted* this proof itself. Its logs therefore prove nothing about who
/// published; it could have simulated every transcript it holds. An
/// eavesdropper (holding neither private key) can neither verify nor
/// forge. This replaces per-claim signatures as the publish gate: same
/// spam posture — you still can't publish into a mailbox without the
/// log's key — zero persistent evidence of the speech act.
pub fn publish_proof(identity: &Identity, eph_pub: &[u8; 32], nonce: &[u8; 16]) -> [u8; 32] {
    use hmac::Mac;
    let my_secret = x25519_dalek::StaticSecret::from(identity.signing.to_scalar_bytes());
    let shared = my_secret.diffie_hellman(&x25519_dalek::PublicKey::from(*eph_pub));
    let key = publish_auth_secret(shared.as_bytes(), &identity.log_id(), eph_pub, nonce);
    proof_mac(&key).finalize().into_bytes().into()
}

/// Verify a publish proof (relay side): recompute the DH from the
/// challenge's ephemeral secret and the log's public key. `false` for an
/// invalid log id or a wrong proof.
pub fn verify_publish_proof(log: LogId, challenge: &PublishChallenge, proof: &[u8; 32]) -> bool {
    let Ok(ed) = log.verifying_key() else {
        return false;
    };
    let their_public = x25519_dalek::PublicKey::from(ed.to_montgomery().to_bytes());
    let eph_secret = x25519_dalek::StaticSecret::from(challenge.secret);
    let shared = eph_secret.diffie_hellman(&their_public);
    let key = publish_auth_secret(shared.as_bytes(), &log, &challenge.public, &challenge.nonce);
    use hmac::Mac;
    proof_mac(&key).verify_slice(proof).is_ok() // constant-time compare
}

/// Domain-separation prefix for attestation signatures — the only Ed25519
/// signature in the system, and it never touches the wire in the clear:
/// it rides inside ciphertext until someone holding the plaintext
/// discloses it.
pub const ATTEST_DOMAIN: &[u8] = b"vouch attest v1";

fn attest_input(claim: &ClaimHash, content_hash: &[u8; 32]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(ATTEST_DOMAIN.len() + 64);
    msg.extend_from_slice(ATTEST_DOMAIN);
    msg.extend_from_slice(&claim.0);
    msg.extend_from_slice(content_hash);
    msg
}

/// Go on the record: an attestation over one of this identity's own
/// claims. Binds the exact claim (its id) and the exact words (the hash
/// of the canonical plaintext body — which seals in the claimed `at`):
/// "I said this, then." Edits after attestation are new, unattested
/// speech.
///
/// Returns the plaintext draft; seal it like any other claim
/// ([`seal_draft`]) and mint it. It is always a separate claim — one
/// mechanism, whether you attest in the same breath or years later.
impl Identity {
    pub fn attest(&self, claim: ClaimHash, plaintext_body: &Value) -> Draft {
        let content_hash = *blake3::hash(&cbor::to_bytes(plaintext_body)).as_bytes();
        let sig = self.signing.sign(&attest_input(&claim, &content_hash));
        Draft::new(ATTEST_TYPE)
            .field(
                "of",
                Value::ClaimRef(ClaimRef {
                    log_id: self.log_id(),
                    hash: claim,
                }),
            )
            .field("content", Value::Bytes(content_hash.to_vec()))
            .field("sig", Value::Bytes(sig.to_bytes().to_vec()))
    }
}

/// Verify an attestation against the words it claims to cover. This is
/// the stranger-facing check — no content key required: given the
/// author's LogId (public), the attested claim id, the plaintext body
/// someone disclosed, and the attest claim's fields, it holds iff the
/// author's key really signed those exact words for that exact claim.
/// Anyone shown the plaintext and the attestation can run it; that
/// portability is what "on the record" means.
pub fn verify_attest(
    author: LogId,
    claim: ClaimHash,
    plaintext_body: &Value,
    attest_body: &BTreeMap<String, Value>,
) -> bool {
    let Some(Value::Bytes(content)) = attest_body.get("content") else {
        return false;
    };
    let Some(Value::Bytes(sig)) = attest_body.get("sig") else {
        return false;
    };
    let Ok(content_hash): Result<[u8; 32], _> = content.as_slice().try_into() else {
        return false;
    };
    if *blake3::hash(&cbor::to_bytes(plaintext_body)).as_bytes() != content_hash {
        return false; // the words shown are not the words attested
    }
    let Ok(sig) = ed25519_dalek::Signature::from_slice(sig) else {
        return false;
    };
    let Ok(key) = author.verifying_key() else {
        return false;
    };
    key.verify(&attest_input(&claim, &content_hash), &sig).is_ok()
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
    fn addresses_roundtrip_and_reject_malformed_input() {
        let alice = Identity::from_seed([1; 32]);
        let address = alice.address();
        let text = address.to_string();
        assert!(text.starts_with("vouch:"));
        assert_eq!(text.len(), "vouch:".len() + 128);

        assert_eq!(Address::parse(&text), Some(address));
        // Prefix optional, surrounding whitespace tolerated — the paste
        // survives a text message.
        assert_eq!(Address::parse(&format!("  {}  ", &text["vouch:".len()..])), Some(address));

        // A bare LogId routes but cannot read: not an address.
        assert_eq!(Address::parse(&alice.log_id().to_string()), None);
        assert_eq!(Address::parse(""), None);
        assert_eq!(Address::parse(&format!("{}zz", &text[..text.len() - 2])), None);
    }

    #[test]
    fn publish_proofs_convince_the_relay_and_nobody_else() {
        let alice = Identity::from_seed([1; 32]);
        let mallory = Identity::from_seed([6; 32]);
        let challenge = publish_challenge().unwrap();

        // The key-holder's proof verifies against her log...
        let proof = publish_proof(&alice, &challenge.public, &challenge.nonce);
        assert!(verify_publish_proof(alice.log_id(), &challenge, &proof));
        // ...someone else's key can't produce it...
        let forged = publish_proof(&mallory, &challenge.public, &challenge.nonce);
        assert!(!verify_publish_proof(alice.log_id(), &challenge, &forged));
        // ...and a proof doesn't survive to a different challenge (replay).
        let later = publish_challenge().unwrap();
        assert!(!verify_publish_proof(alice.log_id(), &later, &proof));
    }

    #[test]
    fn attestations_verify_for_strangers_and_bind_the_exact_words() {
        let alice = Identity::from_seed([1; 32]);
        let words = Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(1_750_000_000_000)),
            ("subject", Value::text("Joe's Pizza")),
            ("body", Value::text("best slice in town")),
        ]);
        let claim_id = ClaimHash([7; 32]);

        let draft = alice.attest(claim_id, &words);
        let Value::Map(attest_body) = draft.body_value() else {
            panic!("attest draft must be a map");
        };

        // A stranger — no content key anywhere in sight — verifies Alice
        // signed exactly these words for exactly this claim.
        assert!(verify_attest(alice.log_id(), claim_id, &words, &attest_body));

        // Different words, different claim, or different author: nothing.
        let edited = Value::map([("subject", Value::text("Joe's Pizza, mostly"))]);
        assert!(!verify_attest(alice.log_id(), claim_id, &edited, &attest_body));
        assert!(!verify_attest(alice.log_id(), ClaimHash([8; 32]), &words, &attest_body));
        let bob = Identity::from_seed([2; 32]);
        assert!(!verify_attest(bob.log_id(), claim_id, &words, &attest_body));
    }

    #[test]
    fn a_forged_tag_is_invisible_in_the_decrypted_view() {
        use crate::store::ClaimStore;
        let alice = Identity::from_seed([1; 32]);
        let mut writer = crate::Writer::from_seed([1; 32]);
        let draft = seal_draft(
            &alice.content_key(),
            &Draft::new("rec").at(1).text("subject", "Joe's"),
        )
        .unwrap();
        let mut event = writer.claim(draft.body_value()).unwrap();

        // A key-holder CAN forge a whole claim (that's deniability), but
        // bytes whose tag doesn't check — e.g. relay tampering — are not
        // speech and never surface.
        event.tag[0] ^= 1;
        let mut store = ClaimStore::new();
        store.ingest(event).unwrap(); // structurally fine: stores blind
        let keys = keys_for(&alice, &[]);
        assert!(decrypted_view(&store, &keys).is_empty());
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
