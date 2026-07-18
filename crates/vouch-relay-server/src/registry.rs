//! The mailbox registry: one durable `Peer` per `LogId`, created lazily on
//! first connection and kept alive for the life of the process — a known
//! simplification; there's no idle-eviction yet, only claim-level
//! retention (see `main::gc_loop`).
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

    /// Get or lazily create the mailbox for `log_id`.
    pub async fn mailbox(&self, log_id: LogId) -> Peer {
        let mut mailboxes = self.mailboxes.lock().await;
        if let Some(peer) = mailboxes.get(&log_id) {
            return peer.clone();
        }
        let dir = self.data_dir.join(log_id.to_string());
        std::fs::create_dir_all(&dir).expect("create mailbox directory");
        let (peer, actor) =
            vouch_store::open_peer(&dir, None, ServePolicy::Everything).expect("open mailbox");
        tokio::spawn(actor.run());
        mailboxes.insert(log_id, peer.clone());
        peer
    }

    /// Every mailbox currently held in memory — the maintenance loop's
    /// sweep list. A mailbox never touched since process start (nothing on
    /// disk from a prior run) simply isn't here yet; that's fine, there's
    /// nothing in it to garbage-collect either.
    pub async fn snapshot(&self) -> Vec<(LogId, Peer)> {
        self.mailboxes
            .lock()
            .await
            .iter()
            .map(|(id, peer)| (*id, peer.clone()))
            .collect()
    }
}
