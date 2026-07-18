//! The app's read model: every `rec` component (base claim + accepted
//! edits + comments), folded live via the peer's firehose.
//!
//! Re-queries in full on every change rather than tracking per-claim
//! dependencies — cheap at this scale (see VOUCH_ARCHITECTURE.md's
//! "Storage & Reactivity" section). The fold itself (`vouch_core::fold`)
//! is the materializer; this is just the GPUI-reactive wrapper around it,
//! same shape as before.

use futures::StreamExt;
use gpui::{AsyncApp, Context, WeakEntity};
use vouch_core::{Peer, Recommendation, StoredClaim};

fn accept_all(_: &StoredClaim) -> bool {
    true
}

pub struct Feed {
    peer: Peer,
    recs: Vec<Recommendation>,
}

impl Feed {
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
            recs: Vec::new(),
        }
    }

    async fn reload(peer: &Peer, this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let Ok(mut recs) = peer
            .query(|db| vouch_core::rec::recommendations(db.claims(), &accept_all))
            .await
        else {
            return;
        };
        recs.sort_by_key(|r| std::cmp::Reverse(newest_at(r)));
        let _ = this.update(cx, |feed, cx| {
            feed.recs = recs;
            cx.notify();
        });
    }

    pub fn recs(&self) -> &[Recommendation] {
        &self.recs
    }

    pub fn peer(&self) -> &Peer {
        &self.peer
    }
}

/// Newest-first ordering: the latest claimed `at` across every claim
/// contributing to this recommendation (its own claim, any accepted edit,
/// any comment) — so an active thread sorts up, not just a fresh post.
fn newest_at(rec: &Recommendation) -> i64 {
    rec.fields
        .values()
        .flat_map(|f| f.frontier.iter().map(|c| c.at))
        .chain(rec.comments.iter().map(|c| c.at))
        .max()
        .unwrap_or(0)
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
        std::env::temp_dir().join(format!("vouch-feed-test-{}-{}", std::process::id(), n))
    }

    fn open_test_peer(seed: u8) -> (Peer, vouch_core::PeerActor, std::path::PathBuf) {
        let dir = temp_dir();
        std::fs::create_dir_all(&dir).unwrap();
        let writer = Writer::from_seed([seed; 32]);
        let (peer, actor) =
            vouch_store::open_peer(&dir, Some(writer), ServePolicy::Owned).unwrap();
        (peer, actor, dir)
    }

    /// The exact loop the UI depends on: write a `rec` claim, and the feed
    /// picks it up via the firehose with no explicit re-query from the
    /// caller — this is the create-then-see-it-in-the-feed contract.
    #[gpui::test]
    async fn feed_picks_up_a_claim_via_the_firehose(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(7);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), cx));
        cx.run_until_parked();
        feed.read_with(cx, |feed, _| assert!(feed.recs().is_empty()));

        peer.claim(
            Draft::new("rec")
                .at(1)
                .text("subject", "Joe's Pizza")
                .text("body", "Great!"),
        )
        .await
        .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].subject, "Joe's Pizza");
            assert_eq!(recs[0].body, "Great!");
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The feed is `rec`-only: other vocabulary (e.g. `profile`) must not
    /// leak into it, and a claim missing subject/body (malformed `rec`)
    /// must be skipped rather than rendered blank.
    #[gpui::test]
    async fn feed_ignores_non_rec_and_malformed_claims(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(9);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), cx));
        cx.run_until_parked();

        peer.claim(Draft::new("profile").text("name", "Me"))
            .await
            .unwrap();
        peer.claim(Draft::new("rec").text("subject", "No body"))
            .await
            .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| assert!(feed.recs().is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Newest-first ordering by the claimed `at` time, matching the feed
    /// list's display order.
    #[gpui::test]
    async fn feed_sorts_newest_first(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(11);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), cx));
        cx.run_until_parked();

        peer.claim(
            Draft::new("rec")
                .at(100)
                .text("subject", "Older")
                .text("body", "..."),
        )
        .await
        .unwrap();
        peer.claim(
            Draft::new("rec")
                .at(200)
                .text("subject", "Newer")
                .text("body", "..."),
        )
        .await
        .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 2);
            assert_eq!(recs[0].subject, "Newer");
            assert_eq!(recs[1].subject, "Older");
        });

        let _ = std::fs::remove_dir_all(&dir);
    }
}
