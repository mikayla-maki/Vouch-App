//! Who you follow — deliberately NOT claims.
//!
//! Consumption is private: anything in your log publishes to your
//! mailbox, so follows-as-claims would broadcast your reading list to
//! anyone holding your address. Instead they're a local JSON file next
//! to `identity.key` (a flat array of address strings), never synced,
//! never servable. Ephemeral instances get no file at all — their
//! follows last exactly as long as they do.
//!
//! Each entry is a full capability [`Address`]: following someone and
//! being able to read them are the same act, because the pasted string
//! carries both the LogId to sync and the content key to decrypt.

use std::path::{Path, PathBuf};

use gpui::Context;
use vouch_core::Peer;
use vouch_core::e2ee::Address;

pub struct Follows {
    peer: Peer,
    mailbox_url: Option<String>,
    /// Where follows persist. Dropped to `None` for the session if the
    /// existing file couldn't be parsed — we never overwrite a file we
    /// couldn't read.
    path: Option<PathBuf>,
    list: Vec<Address>,
}

impl Follows {
    /// Load the stored follows, connect a mailbox bridge for each, and
    /// fold in any extras (the `VOUCH_FOLLOW` env var) through the same
    /// path as a UI add — connected and persisted alike.
    pub fn new(
        peer: Peer,
        mailbox_url: Option<String>,
        path: Option<PathBuf>,
        extras: Vec<Address>,
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
        list.retain(|address| Some(address.log) != peer.id());
        let cleaned = list.len() != before;

        let mut this = Self {
            peer,
            mailbox_url,
            path,
            list,
        };
        if cleaned {
            this.save();
        }
        for address in this.list.clone() {
            this.connect(&address);
        }
        let mut added = false;
        for address in extras {
            if Some(address.log) != this.peer.id()
                && !this.list.iter().any(|a| a.log == address.log)
            {
                this.list.push(address);
                this.connect(&address);
                added = true;
            }
        }
        if added {
            this.save();
        }
        this
    }

    pub fn list(&self) -> &[Address] {
        &self.list
    }

    /// Follow a new address: connect its mailbox, persist, notify.
    /// Returns false (and does nothing) for a log already followed —
    /// including your own, which you follow by construction (publishing
    /// IS following your own log at the mailbox).
    pub fn add(&mut self, address: Address, cx: &mut Context<Self>) -> bool {
        if Some(address.log) == self.peer.id() || self.list.iter().any(|a| a.log == address.log) {
            return false;
        }
        self.list.push(address);
        self.connect(&address);
        self.save();
        cx.notify();
        true
    }

    fn connect(&self, address: &Address) {
        if let Some(url) = &self.mailbox_url {
            vouch_transport::connect_mailbox(&self.peer, url, address.log);
        }
    }

    fn save(&self) {
        let Some(path) = &self.path else { return };
        let strings: Vec<String> = self.list.iter().map(|a| a.to_string()).collect();
        match serde_json::to_string_pretty(&strings) {
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
/// refuse to rewrite. Pre-capability files (bare 64-hex LogIds) land
/// here too: those entries route but can't read, so we leave the file
/// untouched for the owner to re-follow with full addresses.
fn load(path: &Path) -> Option<Vec<Address>> {
    let text = std::fs::read_to_string(path).ok()?;
    let strings = serde_json::from_str::<Vec<String>>(&text).ok()?;
    strings.iter().map(|s| Address::parse(s)).collect()
}
