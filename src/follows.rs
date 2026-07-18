//! Who you follow — deliberately NOT claims.
//!
//! Consumption is private: anything in your log publishes to your
//! mailbox, so follows-as-claims would broadcast your reading list to
//! anyone holding your address. Instead they're a local JSON file next
//! to `identity.key` (a flat array of 64-hex addresses), never synced,
//! never servable. Ephemeral instances get no file at all — their
//! follows last exactly as long as they do.

use std::path::{Path, PathBuf};

use gpui::Context;
use vouch_core::e2ee::{self, Identity};
use vouch_core::{Draft, LogId, Peer, Value};

pub struct Follows {
    peer: Peer,
    identity: Identity,
    mailbox_url: Option<String>,
    /// Where follows persist. Dropped to `None` for the session if the
    /// existing file couldn't be parsed — we never overwrite a file we
    /// couldn't read.
    path: Option<PathBuf>,
    list: Vec<LogId>,
}

impl Follows {
    /// Load the stored follows, connect a mailbox bridge for each, and
    /// fold in any extras (the `VOUCH_FOLLOW` env var) through the same
    /// path as a UI add — connected, granted, and persisted alike.
    pub fn new(
        peer: Peer,
        identity: Identity,
        mailbox_url: Option<String>,
        path: Option<PathBuf>,
        extras: Vec<LogId>,
    ) -> Self {
        let (mut list, path) = match path {
            None => (Vec::new(), None),
            Some(p) if !p.exists() => (Vec::new(), Some(p)),
            Some(p) => match load(&p) {
                Some(list) => (list, Some(p)),
                None => {
                    eprintln!(
                        "follows file at {} is unreadable; following from it is off this \
                         session and the file will not be touched",
                        p.display()
                    );
                    (Vec::new(), None)
                }
            },
        };

        // You already follow yourself (that's how publishing works) —
        // a stored self-follow is junk from an older build or a hand
        // edit; drop it and persist the cleanup.
        let before = list.len();
        list.retain(|log| Some(*log) != peer.id());
        let cleaned = list.len() != before;

        let mut this = Self {
            peer,
            identity,
            mailbox_url,
            path,
            list,
        };
        if cleaned {
            this.save();
        }
        for log in this.list.clone() {
            this.connect(log);
            this.grant(log);
        }
        let mut added = false;
        for log in extras {
            if !this.list.contains(&log) {
                this.list.push(log);
                this.connect(log);
                this.grant(log);
                added = true;
            }
        }
        if added {
            this.save();
        }
        this
    }

    pub fn list(&self) -> &[LogId] {
        &self.list
    }

    /// Follow a new address: connect its mailbox, persist, notify.
    /// Returns false (and does nothing) for an address already followed —
    /// including your own, which you follow by construction (publishing
    /// IS following your own log at the mailbox).
    pub fn add(&mut self, log: LogId, cx: &mut Context<Self>) -> bool {
        if Some(log) == self.peer.id() || self.list.contains(&log) {
            return false;
        }
        self.list.push(log);
        self.connect(log);
        self.grant(log);
        self.save();
        cx.notify();
        true
    }

    fn connect(&self, log: LogId) {
        if let Some(url) = &self.mailbox_url {
            vouch_transport::connect_mailbox(&self.peer, url, log);
        }
    }

    /// Auto-grant on follow: seal our content key to them and publish the
    /// grant into our own log. Sealing is deterministic, so re-granting
    /// on every launch mints a byte-identical claim that content-address
    /// dedupes to nothing — no bookkeeping about who's already granted.
    /// (No `at` field, for the same reason: the body must be stable.)
    fn grant(&self, log: LogId) {
        let Some(sealed) = self.identity.grant_for(log) else {
            eprintln!("cannot grant {log}: not a valid key");
            return;
        };
        let peer = self.peer.clone();
        let draft = Draft::new(e2ee::GRANT_TYPE).field("sealed", Value::Bytes(sealed));
        std::thread::spawn(move || {
            if let Err(e) = futures::executor::block_on(peer.claim(draft)) {
                eprintln!("failed to publish grant: {e}");
            }
        });
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let hex: Vec<String> = self.list.iter().map(|l| l.to_string()).collect();
        match serde_json::to_string_pretty(&hex) {
            Ok(json) => {
                if let Err(e) = std::fs::write(path, json) {
                    eprintln!("failed to persist follows: {e}");
                }
            }
            Err(e) => eprintln!("failed to encode follows: {e}"),
        }
    }
}

/// `None` on any problem — a file we can't fully parse is a file we
/// refuse to rewrite.
fn load(path: &Path) -> Option<Vec<LogId>> {
    let text = std::fs::read_to_string(path).ok()?;
    let hex = serde_json::from_str::<Vec<String>>(&text).ok()?;
    hex.iter()
        .map(|h| vouch_transport::parse_log_id(h))
        .collect()
}
