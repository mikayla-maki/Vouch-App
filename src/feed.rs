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
use gpui::{AsyncApp, Context, Entity, WeakEntity};
use vouch_core::e2ee::{self, Identity};
use vouch_core::fold::ClaimView;
use vouch_core::{LogId, Peer, Recommendation};

use crate::follows::Follows;

fn accept_all(_: &ClaimView) -> bool {
    true
}

pub struct Feed {
    peer: Peer,
    /// Our own identity: its derived key is always in the view.
    identity: Identity,
    /// Every followed address carries the key that makes its log
    /// legible — the follows list IS the keyring.
    follows: Entity<Follows>,
    recs: Vec<Recommendation>,
    /// Advertised display names, from each log's newest `profile` claim —
    /// refreshed on the same firehose events as the recs. Sealed like all
    /// speech: only logs we hold a key for resolve.
    names: BTreeMap<LogId, String>,
}

impl Feed {
    pub fn new(
        peer: Peer,
        identity: Identity,
        follows: Entity<Follows>,
        cx: &mut Context<Self>,
    ) -> Self {
        // A new follow is a new key: ciphertext already in the store may
        // decrypt now, so re-fold even though no claim arrived.
        cx.observe(&follows, |this, _, cx| {
            let peer = this.peer.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                Self::reload(&peer, &this, cx).await;
            })
            .detach();
        })
        .detach();

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
            identity,
            follows,
            recs: Vec::new(),
            names: BTreeMap::new(),
        }
    }

    async fn reload(peer: &Peer, this: &WeakEntity<Self>, cx: &mut AsyncApp) {
        // Snapshot the keyring (own key + followed addresses) before
        // querying: keys live in entities, the fold runs on the peer.
        let Ok(keys) = this.update(cx, |feed, cx| {
            e2ee::keys_for(&feed.identity, feed.follows.read(cx).list())
        }) else {
            return;
        };
        let Ok((mut recs, names)) = peer
            .query(move |db| {
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

    /// A feed over `peer`, following the given addresses — the exact
    /// wiring `VouchApp` does.
    fn feed_with(
        peer: &Peer,
        seed: u8,
        follows: Vec<e2ee::Address>,
        cx: &mut TestAppContext,
    ) -> Entity<Feed> {
        let follows = cx.new(|_| Follows::new(peer.clone(), None, None, follows));
        cx.new(|cx| Feed::new(peer.clone(), identity_of(seed), follows, cx))
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

        let feed = feed_with(&peer, 7, vec![], cx);
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

        let feed = feed_with(&peer, 9, vec![], cx);
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

        let feed = feed_with(&peer, 15, vec![], cx);
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

        let feed = feed_with(&peer, 11, vec![], cx);
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

        let feed = feed_with(&peer, 13, vec![], cx);
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
    /// comments on someone else's rec, sealed with HIS key — legible to
    /// alice because she follows his address (the paste IS the grant).
    /// Comment and rec sync over a real pipe; the comment lands in
    /// `comments` without touching `fields`.
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

        // Alice pasted bob's address, so she holds his content key.
        let feed = feed_with(&alice, 21, vec![identity_of(22).address()], cx);
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

        // Bob comments on her rec — the reference rides inside his
        // ciphertext, sealed with his own key.
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

    /// Going on the record, through the reactive layer: the feed shows the
    /// rec unmarked (deniable is the default), then flips to on-the-record
    /// when the attest claim lands — the exact minting path the detail
    /// panel's button drives.
    #[gpui::test]
    async fn an_attest_claim_flips_the_rec_to_on_the_record(cx: &mut TestAppContext) {
        let (peer, actor, dir) = open_test_peer(17);
        cx.update(|cx| cx.background_executor().spawn(actor.run()).detach());

        let feed = feed_with(&peer, 17, vec![], cx);
        cx.run_until_parked();

        let rec_draft = Draft::new("rec")
            .at(1)
            .text("subject", "Delfina")
            .text("body", "Overrated, honestly");
        let rec_words = rec_draft.body_value();
        let rec = peer.claim(sealed(17, rec_draft)).await.unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            assert_eq!(feed.recs().len(), 1);
            assert!(!feed.recs()[0].on_the_record(), "deniable by default");
        });

        // The button's path: attest the exact plaintext, sealed like all
        // speech.
        let identity = identity_of(17);
        let attest = identity.attest(rec.id(), &rec_words).at(2);
        peer.claim(sealed(17, attest)).await.unwrap();
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            assert_eq!(feed.recs().len(), 1, "an attest is not a feed item");
            assert!(feed.recs()[0].on_the_record());
        });

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The capability moment: bob's sealed claims are already synced into
    /// alice's store but illegible — until she pastes his address. No new
    /// claim arrives; the follow itself re-folds the feed and the backlog
    /// decrypts.
    #[gpui::test]
    async fn adding_a_follow_decrypts_the_already_synced_backlog(cx: &mut TestAppContext) {
        let (alice, alice_actor, alice_dir) = open_test_peer(23);
        let (bob, bob_actor, bob_dir) = open_test_peer(24);
        cx.update(|cx| {
            cx.background_executor().spawn(alice_actor.run()).detach();
            cx.background_executor().spawn(bob_actor.run()).detach();
        });

        let bob_log = bob.id().unwrap();
        let (a_end, b_end) = vouch_core::pipe(256);
        let on_alice = alice.connect("bob", a_end).await.unwrap();
        let _on_bob = bob.connect("alice", b_end).await.unwrap();
        alice.follow(bob_log, on_alice).await.unwrap();

        let follows = cx.new(|_| Follows::new(alice.clone(), None, None, vec![]));
        let feed =
            cx.new(|cx| Feed::new(alice.clone(), identity_of(23), follows.clone(), cx));
        cx.run_until_parked();

        bob.claim(sealed(
            24,
            Draft::new("rec")
                .at(1)
                .text("subject", "Hidden ramen bar")
                .text("body", "Ask for the off-menu tsukemen"),
        ))
        .await
        .unwrap();
        cx.run_until_parked();

        // Synced, but sealed: without bob's key the feed shows nothing.
        feed.read_with(cx, |feed, _| assert!(feed.recs().is_empty()));

        // Pasting his address is the grant — nothing else happens on the
        // wire, yet the feed re-folds legible.
        follows.update(cx, |follows, cx| {
            assert!(follows.add(identity_of(24).address(), cx));
        });
        cx.run_until_parked();

        feed.read_with(cx, |feed, _| {
            let recs = feed.recs();
            assert_eq!(recs.len(), 1, "the backlog decrypted on follow");
            assert_eq!(recs[0].subject, "Hidden ramen bar");
        });

        let _ = std::fs::remove_dir_all(&alice_dir);
        let _ = std::fs::remove_dir_all(&bob_dir);
    }
}
