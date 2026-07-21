//! The raw-claims read model: every claim the local database holds, of any
//! type, folded live via the peer's firehose.
//!
//! This is the debug counterpart to [`crate::feed::Feed`]. Where `Feed`
//! materializes only `rec` components through `vouch_core::rec`, this lists
//! the store's whole timeline verbatim — every landed claim, whatever its
//! vocabulary — so a developer can see what actually converged, not just what
//! the app's normal UI surfaces. Same reactive shape as `Feed`: query in full
//! on every firehose event, sort, notify.

use futures::StreamExt;
use gpui::{AsyncApp, Context, WeakEntity};
use vouch_core::{Peer, StoredClaim, Value};

pub struct DebugFeed {
    peer: Peer,
    claims: Vec<StoredClaim>,
}

impl DebugFeed {
    pub fn new(peer: Peer, cx: &mut Context<Self>) -> Self {
        cx.spawn({
            let peer = peer.clone();
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                Self::reload(&peer, &this, cx).await;
                let Ok(mut rx) = peer.firehose().await else {
                    return;
                };
                while rx.next().await.is_some() {
                    Self::reload(&peer, &this, cx).await;
                }
            }
        })
        .detach();

        Self {
            peer,
            claims: Vec::new(),
        }
    }

    async fn reload(peer: &Peer, this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let Ok(mut claims) = peer.query(|db| db.claims().timeline()).await else {
            return;
        };
        // `timeline()` sorts ascending by `(at, log_id, id)`; the debug view
        // wants newest-first, so re-sort by claimed time descending with a
        // stable id tie-break (deterministic across stores, and across runs).
        claims.sort_by(|a, b| {
            claim_at(b)
                .cmp(&claim_at(a))
                .then_with(|| a.event.id().cmp(&b.event.id()))
        });
        let _ = this.update(cx, |feed, cx| {
            feed.claims = claims;
            cx.notify();
        });
    }

    pub fn claims(&self) -> &[StoredClaim] {
        &self.claims
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }
}

/// Display time for a claim: the author-claimed top-level `at` (Unix ms) when
/// present, else this store's local receive time. Cosmetic — ordering only.
pub fn claim_at(c: &StoredClaim) -> i64 {
    if let Some(Value::Map(m)) = &c.body
        && let Some(Value::Int(t)) = m.get("at")
    {
        return *t;
    }
    c.received_at
}

/// A claim's vocabulary tag: the top-level `type` string, read the same way
/// `ClaimStore::by_type` and the engine's recognizers do. `None` for a
/// bodiless tombstone or a body with no `type` field.
pub fn claim_type(c: &StoredClaim) -> Option<&str> {
    match &c.body {
        Some(Value::Map(m)) => match m.get("type") {
            Some(Value::Text(t)) => Some(t.as_str()),
            _ => None,
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext, TestAppContext};
    use std::sync::atomic::{AtomicU64, Ordering};
    use vouch_core::{Draft, ServePolicy, Writer};

    fn temp_dir() -> std::path::PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("vouch-debug-test-{}-{}", std::process::id(), n))
    }

    fn open_test_peer(seed: u8) -> (Peer, vouch_core::PeerActor, std::path::PathBuf) {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let writer = Writer::from_seed([seed; 32]);
        let (peer, actor) =
            vouch_store::open_peer(&dir, Some(writer), ServePolicy::Owned).unwrap();
        (peer, actor, dir)
    }

    /// The debug view's core contract: a claim of ANY type lands and shows up
    /// via the firehose with no explicit re-query — including vocabulary the
    /// normal `rec` feed deliberately ignores.
    #[gpui::test]
    async fn debug_feed_shows_any_claim_type(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(21);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| DebugFeed::new(peer.clone(), cx));
        cx.run_until_parked();
        feed.read_with(cx, |feed, _| assert!(feed.claims().is_empty()));

        // A `profile` claim — never rendered by the rec feed — must appear here.
        peer.claim(Draft::new("profile").at(1).text("name", "Me"))
            .await
            .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let claims = feed.claims();
            assert_eq!(claims.len(), 1);
            assert_eq!(claim_type(&claims[0]), Some("profile"));
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Every type lands: rec, profile, and a comment referencing the rec all
    /// coexist in the raw timeline (nothing is folded away or filtered).
    #[gpui::test]
    async fn debug_feed_lists_every_type_newest_first(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(23);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| DebugFeed::new(peer.clone(), cx));
        cx.run_until_parked();

        peer.claim(Draft::new("profile").at(100).text("name", "Me"))
            .await
            .unwrap();
        peer.claim(
            Draft::new("rec")
                .at(200)
                .text("subject", "Joe's Pizza")
                .text("body", "Great!"),
        )
        .await
        .unwrap();
        peer.claim(Draft::new("note").at(300).text("body", "scratch"))
            .await
            .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let claims = feed.claims();
            assert_eq!(claims.len(), 3);
            // Newest-first by claimed `at`.
            assert_eq!(claim_type(&claims[0]), Some("note"));
            assert_eq!(claim_type(&claims[1]), Some("rec"));
            assert_eq!(claim_type(&claims[2]), Some("profile"));
        });

        let _ = std::fs::remove_dir_all(&dir);
    }
}
