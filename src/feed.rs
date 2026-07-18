//! The app's read model: every `rec` component (base claim + accepted
//! edits + comments), folded live via the peer's firehose.
//!
//! Re-queries in full on every change rather than tracking per-claim
//! dependencies — cheap at this scale (see VOUCH_ARCHITECTURE.md's
//! "Storage & Reactivity" section). The fold itself (`vouch_core::fold`)
//! is the materializer; this is just the GPUI-reactive wrapper around it,
//! same shape as before.

use std::collections::BTreeMap;

use futures::StreamExt;
use gpui::{AsyncApp, Context, WeakEntity};
use vouch_core::e2ee::{self, Identity};
use vouch_core::fold::ClaimView;
use vouch_core::{LogId, Peer, Recommendation};

fn accept_all(_: &ClaimView) -> bool {
    true
}

pub struct Feed {
    peer: Peer,
    recs: Vec<Recommendation>,
    /// Advertised display names, from each log's newest `profile` claim —
    /// refreshed on the same firehose events as the recs. Sealed like all
    /// speech: only logs we hold a key for resolve.
    names: BTreeMap<LogId, String>,
}

impl Feed {
    pub fn new(peer: Peer, identity: Identity, cx: &mut Context<Self>) -> Self {
        cx.spawn({
            let peer = peer.clone();
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                Self::reload(&peer, &identity, &this, cx).await;
                let Ok(mut rx) = peer.firehose().await else {
                    return;
                };
                while rx.next().await.is_some() {
                    Self::reload(&peer, &identity, &this, cx).await;
                }
            }
        })
        .detach();

        Self {
            peer,
            recs: Vec::new(),
            names: BTreeMap::new(),
        }
    }

    async fn reload(peer: &Peer, identity: &Identity, this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        let identity = identity.clone();
        let Ok((mut recs, names)) = peer
            .query(move |db| {
                // Everything legible to this identity, decrypted once and
                // shared by both projections: our own key (derived) plus
                // every grant we can open.
                let keys = e2ee::collect_keys(db.claims(), &identity);
                let view = e2ee::decrypted_view(db.claims(), &keys);
                (
                    vouch_core::rec::recommendations(&view, &accept_all),
                    vouch_core::profile::names(&view),
                )
            })
            .await
        else {
            return;
        };
        recs.sort_by_key(|r| std::cmp::Reverse(newest_at(r)));
        let _ = this.update(cx, |feed, cx| {
            feed.recs = recs;
            feed.names = names;
            cx.notify();
        });
    }

    pub fn recs(&self) -> &[Recommendation] {
        &self.recs
    }

    pub fn names(&self) -> &BTreeMap<LogId, String> {
        &self.names
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
    use vouch_core::{ClaimRef, Draft, ServePolicy, Value, Writer};

    fn identity_of(seed: u8) -> Identity {
        Identity::from_seed([seed; 32])
    }

    /// Seal `draft` the way every real authoring path does.
    fn sealed(seed: u8, draft: Draft) -> Draft {
        e2ee::seal_draft(&identity_of(seed).content_key(), &draft).unwrap()
    }

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

        let feed = cx.new(|cx| Feed::new(peer.clone(), identity_of(7), cx));
        cx.run_until_parked();
        feed.read_with(cx, |feed, _| assert!(feed.recs().is_empty()));

        peer.claim(sealed(
            7,
            Draft::new("rec")
                .at(1)
                .text("subject", "Joe's Pizza")
                .text("body", "Great!"),
        ))
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

        let feed = cx.new(|cx| Feed::new(peer.clone(), identity_of(9), cx));
        cx.run_until_parked();

        peer.claim(sealed(9, Draft::new("profile").text("name", "Me")))
            .await
            .unwrap();
        peer.claim(sealed(9, Draft::new("rec").text("subject", "No body")))
            .await
            .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| assert!(feed.recs().is_empty()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Advertised names ride the same reload as recs: a `profile` claim
    /// lands and the name map updates without any explicit re-query.
    #[gpui::test]
    async fn feed_resolves_advertised_names(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(15);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), identity_of(15), cx));
        cx.run_until_parked();
        feed.read_with(cx, |feed, _| assert!(feed.names().is_empty()));

        peer.claim(sealed(15, Draft::new("profile").at(1).text("name", "Maya")))
            .await
            .unwrap();
        cx.run_until_parked();

        let me = peer.id().unwrap();
        feed.read_with(cx, |feed, _| {
            assert_eq!(feed.names().get(&me).map(String::as_str), Some("Maya"));
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Newest-first ordering by the claimed `at` time, matching the feed
    /// list's display order.
    #[gpui::test]
    async fn feed_sorts_newest_first(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(11);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), identity_of(11), cx));
        cx.run_until_parked();

        peer.claim(sealed(
            11,
            Draft::new("rec")
                .at(100)
                .text("subject", "Older")
                .text("body", "..."),
        ))
        .await
        .unwrap();
        peer.claim(sealed(
            11,
            Draft::new("rec")
                .at(200)
                .text("subject", "Newer")
                .text("body", "..."),
        ))
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

    /// The edit path the detail panel drives: the source author edits their
    /// own `rec`, and the folded `Recommendation` in the feed shows the new
    /// subject/body — one component still, not a second post. Mirrors the
    /// `Draft::new("edit").field("of", …).text(…)` shape the edit modal
    /// builds.
    #[gpui::test]
    async fn a_source_author_edit_updates_the_folded_recommendation(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(13);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = cx.new(|cx| Feed::new(peer.clone(), identity_of(13), cx));
        cx.run_until_parked();

        let author = peer.id().unwrap();
        let rec = peer
            .claim(sealed(
                13,
                Draft::new("rec")
                    .at(1)
                    .text("subject", "Joe's Pizza")
                    .text("body", "Great!"),
            ))
            .await
            .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 1);
            assert_eq!(recs[0].subject, "Joe's Pizza");
            assert_eq!(recs[0].body, "Great!");
        });

        // An `edit` from the same writer, referencing the original claim so
        // it causally dominates it — exactly what the modal assembles.
        let of = Value::Array(vec![Value::ClaimRef(ClaimRef {
            log_id: author,
            hash: rec.id(),
        })]);
        peer.claim(sealed(
            13,
            Draft::new("edit")
                .at(2)
                .field("of", of)
                .text("subject", "Joe's Pizzeria")
                .text("body", "Still great, now with garlic knots"),
        ))
        .await
        .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 1, "the edit folds in, it is not a new rec");
            assert_eq!(recs[0].subject, "Joe's Pizzeria");
            assert_eq!(recs[0].body, "Still great, now with garlic knots");
            assert_eq!(recs[0].claims.len(), 2, "rec + edit in one component");
            assert!(recs[0].comments.is_empty());
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The comment path under E2EE, end to end: a *different* writer
    /// comments on someone else's rec, sealed with HIS key — so alice can
    /// only read it because bob also granted her. Grant, comment, and rec
    /// all sync over a real pipe; the comment lands in `comments` without
    /// touching `fields`.
    #[gpui::test]
    async fn a_comment_from_another_writer_lands_without_touching_fields(cx: &mut TestAppContext) {
        let (alice, alice_actor, alice_dir) = open_test_peer(21);
        let (bob, bob_actor, bob_dir) = open_test_peer(22);
        cx.update(|cx| {
            cx.background_executor().spawn(alice_actor.run()).detach();
            cx.background_executor().spawn(bob_actor.run()).detach();
        });

        let alice_log = alice.id().unwrap();
        let bob_log = bob.id().unwrap();

        // Link the two peers; alice follows bob's log so his comment reaches
        // her (bob serves his own log automatically under ServePolicy::Owned).
        let (a_end, b_end) = vouch_core::pipe(256);
        let on_alice = alice.connect("bob", a_end).await.unwrap();
        let _on_bob = bob.connect("alice", b_end).await.unwrap();
        alice.follow(bob_log, on_alice).await.unwrap();

        let feed = cx.new(|cx| Feed::new(alice.clone(), identity_of(21), cx));
        cx.run_until_parked();

        // Alice posts a sealed rec.
        let rec = alice
            .claim(sealed(
                21,
                Draft::new("rec")
                    .at(1)
                    .text("subject", "Taco Truck")
                    .text("body", "The al pastor is unreal"),
            ))
            .await
            .unwrap();
        cx.run_until_parked();

        // Bob grants alice (so his sealed speech is legible to her), then
        // comments on her rec — reference riding inside his ciphertext.
        let grant = identity_of(22).grant_for(alice_log).unwrap();
        bob.claim(Draft::new(e2ee::GRANT_TYPE).field("sealed", Value::Bytes(grant)))
            .await
            .unwrap();
        let of = Value::Array(vec![Value::ClaimRef(ClaimRef {
            log_id: alice_log,
            hash: rec.id(),
        })]);
        bob.claim(sealed(
            22,
            Draft::new("comment")
                .at(2)
                .field("of", of)
                .text("text", "Agreed, best in town!"),
        ))
        .await
        .unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 1);
            let rec = &recs[0];
            // Fields untouched by the comment: still alice's originals.
            assert_eq!(rec.subject, "Taco Truck");
            assert_eq!(rec.body, "The al pastor is unreal");
            // The comment shows, authored by bob (a different writer).
            assert_eq!(rec.comments.len(), 1, "bob's comment synced and folded in");
            assert_eq!(rec.comments[0].text, "Agreed, best in town!");
            assert_eq!(rec.comments[0].author, bob_log);
        });

        let _ = std::fs::remove_dir_all(&alice_dir);
        let _ = std::fs::remove_dir_all(&bob_dir);
    }
}
