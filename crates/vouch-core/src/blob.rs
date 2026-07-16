//! Content-addressed blob storage: the media side-car.
//!
//! Blobs are bulk bytes (images, any media) that claim bodies pin by hash
//! via [`BlobRef`](crate::BlobRef) values. They ride a different rail from
//! claims on purpose:
//!
//! - **Blobs are cache, not convergent state.** Claims sync eagerly and the
//!   per-log fingerprint covers them; blob presence is local, like arrival
//!   order. A store can be fully synced on claims while missing
//!   bytes — the UI shows a placeholder and the want stands forever.
//! - **Anyone can serve a blob.** Bytes either hash to the pinned value or
//!   they don't, so every pipe is equally trustworthy and arrival order is
//!   irrelevant. Missing bytes heal exactly like stripped bodies do.
//! - **Deletion is GC, not policy.** A blob referenced by zero live bodies
//!   (every referencing claim redacted) is garbage: collecting it is how
//!   cooperative deletion extends to media.
//!
//! Same cut as claims: [`BlobStorage`] is the dumb backend trait (memory
//! here, files in vouch-store) and [`BlobStore`] is the concrete logic over
//! it. Hashing and verify-on-arrival live ONLY in `BlobStore` — they are
//! not trait methods, not even provided ones, so no backend can override
//! or skip them. Backends store bytes; the engine decides what counts.

use std::collections::HashMap;

use crate::error::Error;
use crate::store::ClaimStore;
use crate::value::BlobHash;

/// Dumb content-addressed byte storage. Implementations store and
/// retrieve; they never hash, verify, or decide. See [`BlobStore`] for the
/// logic that drives them.
pub trait BlobStorage: Send {
    /// Store bytes under a hash the engine has already verified.
    fn insert(&mut self, hash: BlobHash, bytes: Vec<u8>) -> Result<(), Error>;

    /// The bytes, if held. Owned, because a backend may read them from
    /// disk (and may evict corrupt storage on the way — returning `None`
    /// for bytes that no longer match `hash` is correct and self-healing).
    fn get(&self, hash: &BlobHash) -> Option<Vec<u8>>;

    fn contains(&self, hash: &BlobHash) -> bool;

    /// Forget bytes. Returns whether anything was held.
    fn remove(&mut self, hash: &BlobHash) -> Result<bool, Error>;

    /// Every hash held, in any order.
    fn hashes(&self) -> Vec<BlobHash>;
}

/// The blob store: verification and GC logic over a [`BlobStorage`]
/// backend. This is the only place that computes or checks blob hashes, by
/// construction — backends can't get verification wrong because they never
/// see it.
pub struct BlobStore {
    storage: Box<dyn BlobStorage>,
}

impl Default for BlobStore {
    fn default() -> BlobStore {
        BlobStore::new()
    }
}

impl BlobStore {
    /// An in-memory store (tests, simulations, relays).
    pub fn new() -> BlobStore {
        BlobStore::with_storage(Box::new(MemoryBlobStorage::new()))
    }

    /// A store over an injected backend (the app injects files here).
    pub fn with_storage(storage: Box<dyn BlobStorage>) -> BlobStore {
        BlobStore { storage }
    }

    /// Store local bytes (authoring a claim with media). Returns the hash
    /// to pin in the claim body. Idempotent: same bytes, same hash, stored
    /// once.
    pub fn put(&mut self, bytes: Vec<u8>) -> Result<BlobHash, Error> {
        let hash = BlobHash(*blake3::hash(&bytes).as_bytes());
        if !self.storage.contains(&hash) {
            self.storage.insert(hash, bytes)?;
        }
        Ok(hash)
    }

    /// Store bytes fetched from a pipe, verifying them against the hash the
    /// claim pinned. Returns whether the bytes were new. Mismatched bytes
    /// are refused and the want stands — a bad pipe can't poison the cache.
    ///
    /// Verified bytes are always written through, even if the hash is
    /// already present: a backend may hold a *corrupt* copy under that hash
    /// (disk rot), and re-writing the known-good bytes heals it in this one
    /// fetch rather than waiting for a read to evict it first. Writes are
    /// idempotent for content-addressed storage, and we only reach here for
    /// blobs the want-list asked for, so a redundant write is rare.
    pub fn insert_verified(&mut self, expected: BlobHash, bytes: Vec<u8>) -> Result<bool, Error> {
        if *blake3::hash(&bytes).as_bytes() != expected.0 {
            return Err(Error::BlobHashMismatch);
        }
        let was_present = self.storage.contains(&expected);
        self.storage.insert(expected, bytes)?;
        Ok(!was_present)
    }

    pub fn get(&self, hash: &BlobHash) -> Option<Vec<u8>> {
        self.storage.get(hash)
    }

    pub fn contains(&self, hash: &BlobHash) -> bool {
        self.storage.contains(hash)
    }

    /// Cache eviction: drop the bytes, keep every claim. The pinning
    /// claims still reference the hash, so it reappears on the want-list
    /// (`missing_blobs`) and heals from any pipe that serves the referring
    /// log — the website model: media is *re-queryable*, never hoarded.
    /// Storage-pressure policy (what to evict, when) belongs to the app;
    /// this is just the safe primitive. Returns whether bytes were held.
    ///
    /// Contrast [`gc`](Self::gc), which drops only *unreferenced* bytes
    /// (cooperative deletion); eviction drops *referenced* bytes on
    /// purpose, trading local disk for a future re-fetch.
    pub fn evict(&mut self, hash: &BlobHash) -> Result<bool, Error> {
        self.storage.remove(hash)
    }

    /// Drop every blob no live body references, returning what was
    /// removed. This is cooperative deletion for media: when the claims
    /// that carried an image are redacted, the next sweep forgets the
    /// bytes too.
    pub fn gc(&mut self, claims: &ClaimStore) -> Result<Vec<BlobHash>, Error> {
        let mut removed: Vec<BlobHash> = self
            .storage
            .hashes()
            .into_iter()
            .filter(|h| claims.blob_referrers(h).is_empty())
            .collect();
        removed.sort();
        for hash in &removed {
            self.storage.remove(hash)?;
        }
        Ok(removed)
    }
}

/// The in-memory backend: tests, simulations, and ephemeral stores.
#[derive(Default)]
pub struct MemoryBlobStorage {
    blobs: HashMap<BlobHash, Vec<u8>>,
}

impl MemoryBlobStorage {
    pub fn new() -> MemoryBlobStorage {
        MemoryBlobStorage::default()
    }
}

impl BlobStorage for MemoryBlobStorage {
    fn insert(&mut self, hash: BlobHash, bytes: Vec<u8>) -> Result<(), Error> {
        self.blobs.insert(hash, bytes);
        Ok(())
    }

    fn get(&self, hash: &BlobHash) -> Option<Vec<u8>> {
        self.blobs.get(hash).cloned()
    }

    fn contains(&self, hash: &BlobHash) -> bool {
        self.blobs.contains_key(hash)
    }

    fn remove(&mut self, hash: &BlobHash) -> Result<bool, Error> {
        Ok(self.blobs.remove(hash).is_some())
    }

    fn hashes(&self) -> Vec<BlobHash> {
        self.blobs.keys().copied().collect()
    }
}
