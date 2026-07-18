//! Where the app's durable state lives, and this device's writer identity.

use std::path::{Path, PathBuf};

use vouch_core::Writer;

/// The OS application-support directory for this app: `claims.db`,
/// `blobs/`, and `sync.db` (via [`vouch_store::open_peer`]) live here,
/// alongside the device identity.
pub fn app_dir() -> PathBuf {
    dirs::data_dir()
        .expect("no OS application-support directory")
        .join("Vouch")
}

/// Load this device's writer identity, minting one on first launch.
///
/// The seed is stored in plaintext next to the database for now — real key
/// custody (OS keychain, mnemonic backup) is deferred; a `Writer` can only
/// be reconstructed from its seed (never persisted by vouch-core itself),
/// so this is the minimum needed for a stable identity across launches.
pub fn load_or_create_writer(dir: &Path) -> Writer {
    let key_path = dir.join("identity.key");
    if let Ok(bytes) = std::fs::read(&key_path)
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return Writer::from_seed(seed);
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).expect("OS randomness for a new identity");
    std::fs::create_dir_all(dir).expect("create app-support directory");
    std::fs::write(&key_path, seed).expect("persist device identity");
    Writer::from_seed(seed)
}
