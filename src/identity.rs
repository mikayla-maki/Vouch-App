//! Where the app's durable state lives, and this device's identity.

use std::path::{Path, PathBuf};

use vouch_core::Writer;
use vouch_core::e2ee::Identity;

/// The OS application-support directory for this app: `claims.db`,
/// `blobs/`, and `sync.db` (via [`vouch_store::open_peer`]) live here,
/// alongside the device identity.
pub fn app_dir() -> PathBuf {
    dirs::data_dir()
        .expect("no OS application-support directory")
        .join("Vouch")
}

/// Load this device's seed, minting one on first launch, and build both
/// halves of the identity from it: the [`Writer`] that signs and the
/// [`Identity`] that seals/opens (the content key and grants derive from
/// the same 32 bytes, so nothing extra is ever stored or synced).
///
/// The seed is stored in plaintext next to the database for now — real key
/// custody (OS keychain, mnemonic backup) is deferred.
pub fn load_or_create(dir: &Path) -> (Writer, Identity) {
    let key_path = dir.join("identity.key");
    let seed = if let Ok(bytes) = std::fs::read(&key_path)
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        seed
    } else {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("OS randomness for a new identity");
        std::fs::create_dir_all(dir).expect("create app-support directory");
        std::fs::write(&key_path, seed).expect("persist device identity");
        seed
    };
    (Writer::from_seed(seed), Identity::from_seed(seed))
}
