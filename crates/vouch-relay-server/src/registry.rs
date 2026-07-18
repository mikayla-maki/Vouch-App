//! The mailbox registry: one durable `Peer` per `LogId`, created lazily —
//! and only ever *created* after the bridge has seen a validly signed
//! publish for that log. A bare connection (a scanner, a typo, a reader
//! of an address nobody ever published under) must never be able to cost
//! this server disk; see `bridge::dormant_phase` for how such
//! connections are answered without a mailbox.
//!
//! Each mailbox is a `Peer` with no writer of its own and
//! `ServePolicy::Everything`: it holds and serves whatever's published
//! under that log by whoever actually has its private key. Splitting one
//! Peer per log (rather than one shared Peer/database for every
//! connection) keeps each mailbox's storage and actor independent — no
//! single SQLite file or single actor task serializing every client.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Mutex;

use vouch_core::{LogId, Peer, ServePolicy};

#[derive(Clone)]
pub struct Registry {
    data_dir: PathBuf,
    mailboxes: Arc<Mutex<HashMap<LogId, Peer>>>,
}

impl Registry {
    pub fn new(data_dir: PathBuf) -> Registry {
        Registry {
            data_dir,
            mailboxes: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub fn dir_of(&self, log_id: &LogId) -> PathBuf {
        // LogId's Display is pure lowercase hex — no separators, no
        // traversal, safe as a directory name by construction.
        self.data_dir.join(log_id.to_string())
    }

    /// The mailbox for `log_id`, only if it already exists — in memory
    /// from this process, or on disk from any earlier one. `None` means
    /// the connection stays dormant at the bridge.
    pub async fn open_existing(&self, log_id: LogId) -> Option<Peer> {
        let mut mailboxes = self.mailboxes.lock().await;
        if let Some(peer) = mailboxes.get(&log_id) {
            return Some(peer.clone());
        }
        if !self.dir_of(&log_id).exists() {
            return None;
        }
        Some(self.open_locked(&mut mailboxes, log_id))
    }

    /// Create-or-open. Called from exactly one place: the bridge, after
    /// verifying a signed publish for `log_id` — the single event allowed
    /// to allocate disk on this server.
    pub async fn materialize(&self, log_id: LogId) -> Peer {
        let mut mailboxes = self.mailboxes.lock().await;
        if let Some(peer) = mailboxes.get(&log_id) {
            return peer.clone();
        }
        self.open_locked(&mut mailboxes, log_id)
    }

    fn open_locked(&self, mailboxes: &mut HashMap<LogId, Peer>, log_id: LogId) -> Peer {
        let dir = self.dir_of(&log_id);
        std::fs::create_dir_all(&dir).expect("create mailbox directory");
        let (peer, actor) =
            vouch_store::open_peer(&dir, None, ServePolicy::Everything).expect("open mailbox");
        tokio::spawn(actor.run());
        mailboxes.insert(log_id, peer.clone());
        peer
    }

    /// Every mailbox live in memory — swept through the Peer's own GC
    /// verbs so the sweep serializes with client traffic.
    pub async fn live_mailboxes(&self) -> Vec<(LogId, Peer)> {
        self.mailboxes
            .lock()
            .await
            .iter()
            .map(|(id, peer)| (*id, peer.clone()))
            .collect()
    }

    /// Every mailbox directory on disk — including ones from earlier
    /// process lifetimes that nothing has connected to since. The GC
    /// sweep opens dormant ones directly (and drops them after), so old
    /// mailboxes keep draining without being pinned into memory forever.
    pub fn on_disk(&self) -> Vec<LogId> {
        let Ok(entries) = std::fs::read_dir(&self.data_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(|e| e.ok())
            .filter_map(|e| parse_log_id_hex(&e.file_name().to_string_lossy()))
            .collect()
    }
}

/// Parse the 64-hex-char directory-name form of a LogId.
fn parse_log_id_hex(hex: &str) -> Option<LogId> {
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(LogId(bytes))
}
