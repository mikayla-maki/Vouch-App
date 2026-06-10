//! Content-addressed blob storage: the media side-car.
//!
//! Blobs are bulk bytes (images, any media) that claim bodies pin by hash
//! via [`BlobRef`] values. They ride a different rail from claims on
//! purpose:
//!
//! - **Blobs are cache, not convergent state.** Claims sync eagerly and the
//!   per-log fingerprint covers them; blob presence is local, like
//!   provenance. A store can be fully synced on claims while missing
//!   bytes — the UI shows a placeholder and the want stands forever.
//! - **Anyone can serve a blob.** Bytes either hash to the pinned value or
//!   they don't, so every pipe is equally trustworthy and arrival order is
//!   irrelevant. Missing bytes heal exactly like stripped bodies do.
//! - **Deletion is GC, not policy.** A blob referenced by zero live bodies
//!   (every referencing claim redacted) is garbage: collecting it is how
//!   cooperative deletion extends to media.

use std::collections::HashMap;

use crate::error::Error;
use crate::store::ClaimStore;
use crate::value::BlobHash;

/// An in-memory content-addressed blob store.
#[derive(Default)]
pub struct BlobStore {
    blobs: HashMap<BlobHash, Vec<u8>>,
}

impl BlobStore {
    pub fn new() -> BlobStore {
        BlobStore::default()
    }

    /// Store local bytes (authoring a claim with media). Returns the hash
    /// to pin in the claim body. Idempotent: same bytes, same hash, stored
    /// once.
    pub fn put(&mut self, bytes: Vec<u8>) -> BlobHash {
        let hash = BlobHash(*blake3::hash(&bytes).as_bytes());
        self.blobs.entry(hash).or_insert(bytes);
        hash
    }

    /// Store bytes fetched from a pipe, verifying them against the hash the
    /// claim pinned. Returns whether the bytes were new. Mismatched bytes
    /// are refused and the want stands — a bad pipe can't poison the cache.
    pub fn insert_verified(&mut self, expected: BlobHash, bytes: Vec<u8>) -> Result<bool, Error> {
        if *blake3::hash(&bytes).as_bytes() != expected.0 {
            return Err(Error::BlobHashMismatch);
        }
        Ok(self.blobs.insert(expected, bytes).is_none())
    }

    pub fn get(&self, hash: &BlobHash) -> Option<&[u8]> {
        self.blobs.get(hash).map(Vec::as_slice)
    }

    pub fn contains(&self, hash: &BlobHash) -> bool {
        self.blobs.contains_key(hash)
    }

    /// Number of blobs held.
    pub fn len(&self) -> usize {
        self.blobs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.blobs.is_empty()
    }

    /// Drop every blob no live body references, returning what was removed.
    /// This is cooperative deletion for media: when the claims that carried
    /// an image are redacted, the next sweep forgets the bytes too.
    pub fn gc(&mut self, claims: &ClaimStore) -> Vec<BlobHash> {
        let mut removed: Vec<BlobHash> = self
            .blobs
            .keys()
            .filter(|h| claims.blob_referrers(h).next().is_none())
            .copied()
            .collect();
        removed.sort();
        for hash in &removed {
            self.blobs.remove(hash);
        }
        removed
    }
}
