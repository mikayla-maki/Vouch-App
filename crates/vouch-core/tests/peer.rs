//! Whole peers, wired end to end: actors talking through channel pipes,
//! driven on a single-threaded executor until quiescent. Every assertion
//! below holds with no real I/O, no clock, and no scheduler timing —
//! the pipes are channels, so the network is just message passing the
//! test can run to a fixpoint.

use futures::executor::LocalPool;
use futures::task::SpawnExt;

use vouch_core::sync::{InstanceId, MemorySyncState};
use vouch_core::{
    Database, Draft, LogId, Peer, PeerActor, PeerEvent, PipeConfig, ServePolicy, Value, Writer,
    pipe,
};

fn make_peer(seed: Option<u8>, instance: u8, serve: ServePolicy) -> (Peer, PeerActor) {
    Peer::new(
        Database::new(),
        Box::new(MemorySyncState::new()),
        InstanceId([instance; 16]),
        seed.map(|s| Writer::from_seed([s; 32])),
        serve,
        || 1000,
    )
}

/// Connect two peers with an in-process duct; each names the other.
async fn link(a: &Peer, a_calls_b: &str, b: &Peer, b_calls_a: &str) -> (PipeIdPair, PipeIdPair) {
    let (a_end, b_end) = pipe(256);
    let on_a = a.connect(a_calls_b, a_end).await.unwrap();
    let on_b = b.connect(b_calls_a, b_end).await.unwrap();
    (on_a, on_b)
}

type PipeIdPair = vouch_core::PipeId;

fn drain(rx: &mut futures::channel::mpsc::Receiver<PeerEvent>) -> Vec<PeerEvent> {
    let mut out = Vec::new();
    while let Ok(ev) = rx.try_recv() {
        out.push(ev);
    }
    out
}

fn rec(text: &str, at: i64) -> Draft {
    Draft::new("rec").at(at).text("body", text)
}

#[test]
fn a_follow_catches_up_then_frames_keep_it_live() {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();

    let (mut bob_feed, photo_hash) = pool.run_until(async {
        // History exists before bob ever shows up — including media.
        alice.claim(rec("one", 1)).await.unwrap();
        let with_photo = alice
            .claim(rec("two", 2).attach("photo", b"a soup photo".to_vec(), "image/jpeg"))
            .await
            .unwrap();
        let photo_hash = alice
            .query(move |db| db.claims().get(&with_photo.id()).unwrap().blobs[0].1.hash)
            .await
            .unwrap();

        let bob_feed = bob.firehose().await.unwrap();
        let (on_alice, _on_bob) = link(&alice, "bob", &bob, "alice").await;
        let _ = on_alice; // alice's side needs no follow; serving is automatic
        let (_, on_bob) = (on_alice, _on_bob);
        bob.follow(alice_log, on_bob).await.unwrap();
        (bob_feed, photo_hash)
    });
    pool.run_until_stalled();

    // Catch-up: bob holds the history and bodies — but NOT the photo.
    // Media is non-syncing by default; the claim carries the want.
    let (len, fp_bob, has_blob) = pool
        .run_until(bob.query(move |db| {
            (
                db.claims().log_len(&alice_log),
                db.claims().fingerprint(&alice_log),
                db.blobs().get(&photo_hash).is_some(),
            )
        }))
        .unwrap();
    assert_eq!(len, 2);
    assert!(!has_blob, "media must not sync until demanded");
    let fp_alice = pool
        .run_until(alice.query(move |db| db.claims().fingerprint(&alice_log)))
        .unwrap();
    assert_eq!(fp_bob, fp_alice);

    // The UI scrolls the photo into view: one demand, bytes arrive.
    pool.run_until(bob.fetch_blob(photo_hash)).unwrap();
    pool.run_until_stalled();
    let has_blob = pool
        .run_until(bob.query(move |db| db.blobs().get(&photo_hash).is_some()))
        .unwrap();
    assert!(has_blob, "demand-driven fetch healed the want");

    // Live: alice mints; the frame lands at bob with nobody polling.
    pool.run_until(async {
        alice.claim(rec("three", 3)).await.unwrap();
    });
    pool.run_until_stalled();
    let len = pool
        .run_until(bob.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    assert_eq!(len, 3);

    // The firehose narrated all of it.
    let items = drain(&mut bob_feed);
    assert!(items.iter().any(|e| e.log == alice_log));
}

#[test]
fn only_authored_leaves_the_house_but_quotes_travel() {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (carol, carol_actor) = make_peer(Some(3), 3, ServePolicy::Owned);
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(carol_actor.run()).unwrap();
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let carol_log = carol.id().unwrap();
    let alice_log = alice.id().unwrap();

    // Alice follows carol and receives her rec.
    let carol_rec = pool.run_until(async {
        let event = carol.claim(rec("hidden gem taqueria", 1)).await.unwrap();
        let (_, on_alice) = link(&carol, "alice", &alice, "carol").await;
        alice.follow(carol_log, on_alice).await.unwrap();
        event
    });
    pool.run_until_stalled();
    let held = pool
        .run_until(alice.query(move |db| db.claims().log_len(&carol_log)))
        .unwrap();
    assert_eq!(held, 1);

    // Bob tries to get carol's log THROUGH alice: nothing. Alice serves
    // only her own log — what she reads is not servable.
    let on_bob = pool.run_until(async {
        let (_, on_bob) = link(&alice, "bob", &bob, "alice").await;
        bob.follow(carol_log, on_bob).await.unwrap();
        on_bob
    });
    pool.run_until_stalled();
    let leaked = pool
        .run_until(bob.query(move |db| db.claims().log_len(&carol_log)))
        .unwrap();
    assert_eq!(leaked, 0, "consumption must not be servable");

    // But a QUOTE travels: alice re-vouches carol's claim, bob follows
    // alice — and carol's claim arrives INSIDE alice's, signature and all.
    // It rides as content of alice's speech: bob's store gains a row in
    // alice's log only, and reads the quote by recursion.
    let carol_id = carol_rec.id();
    pool.run_until(async {
        bob.follow(alice_log, on_bob).await.unwrap();
        alice
            .claim(
                Draft::new("vouch")
                    .at(2)
                    .embed("original", carol_rec.clone()),
            )
            .await
            .unwrap();
    });
    pool.run_until_stalled();
    let (alice_held, carol_held, quoted) = pool
        .run_until(bob.query(move |db| {
            let quote = db.claims().log(&alice_log);
            (
                db.claims().log_len(&alice_log),
                db.claims().log_len(&carol_log),
                quote.first().map(|c| c.embeds()),
            )
        }))
        .unwrap();
    assert_eq!(alice_held, 1);
    assert_eq!(carol_held, 0, "a quote is content, not a row");
    let quoted = quoted.expect("alice's vouch arrived");
    assert_eq!(
        quoted.first().map(|(_, c)| c.header.id()),
        Some(carol_id),
        "carol's claim reads out of the quote, verified"
    );
}

#[test]
fn publishing_is_following_your_own_log_and_relays_refan() {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    // The relay: no pen, no follows, serves everything published to it.
    let (relay, relay_actor) = make_peer(None, 9, ServePolicy::Everything);
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(relay_actor.run()).unwrap();
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();

    pool.run_until(async {
        // Alice publishes by following her own log at the relay; bob
        // subscribes to alice at the same relay. Alice and bob never meet.
        let (_, alice_pipe) = link(&relay, "alice", &alice, "relay").await;
        alice.follow(alice_log, alice_pipe).await.unwrap();
        let (_, bob_pipe) = link(&relay, "bob", &bob, "relay").await;
        bob.follow(alice_log, bob_pipe).await.unwrap();
    });
    pool.run_until_stalled();

    pool.run_until(async {
        alice.claim(rec("breakfast burrito spot", 1)).await.unwrap();
    });
    pool.run_until_stalled();

    // claim → session publish to relay → relay re-fans the frame → bob.
    let at_relay = pool
        .run_until(relay.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    let at_bob = pool
        .run_until(bob.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    assert_eq!(at_relay, 1);
    assert_eq!(at_bob, 1, "the relay re-fanned the publish to its watcher");

    // Multi-device: a second writer from the same mnemonic, same relay.
    let (phone, phone_actor) = make_peer(Some(1), 4, ServePolicy::Owned);
    spawner.spawn(phone_actor.run()).unwrap();
    pool.run_until(async {
        let (_, phone_pipe) = link(&relay, "phone", &phone, "relay").await;
        phone.follow(alice_log, phone_pipe).await.unwrap();
    });
    pool.run_until_stalled();
    // The phone caught up on history, then mints; everyone converges.
    pool.run_until(async {
        phone.claim(rec("from the phone", 2)).await.unwrap();
    });
    pool.run_until_stalled();
    let (a, b, p, r) = pool.run_until(async {
        (
            alice
                .query(move |db| db.claims().fingerprint(&alice_log))
                .await
                .unwrap(),
            bob.query(move |db| db.claims().fingerprint(&alice_log))
                .await
                .unwrap(),
            phone
                .query(move |db| db.claims().fingerprint(&alice_log))
                .await
                .unwrap(),
            relay
                .query(move |db| db.claims().fingerprint(&alice_log))
                .await
                .unwrap(),
        )
    });
    assert_eq!(a, r);
    assert_eq!(b, r);
    assert_eq!(p, r);
    let len = pool
        .run_until(bob.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    assert_eq!(len, 2);
}

#[test]
fn the_taps_split_everything_versus_only_my_voice() {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(Some(2), 2, ServePolicy::Owned);
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();
    let bob_log = bob.id().unwrap();

    let (mut firehose, mut authored) = pool.run_until(async {
        let firehose = alice.firehose().await.unwrap();
        let authored = alice.authored().await.unwrap();
        let (_, on_alice) = link(&bob, "alice", &alice, "bob").await;
        alice.follow(bob_log, on_alice).await.unwrap();
        (firehose, authored)
    });

    pool.run_until(async {
        bob.claim(rec("bob's pick", 1)).await.unwrap();
        alice.claim(rec("alice's pick", 2)).await.unwrap();
    });
    pool.run_until_stalled();

    let everything = drain(&mut firehose);
    let mine = drain(&mut authored);
    let fire_logs: Vec<LogId> = everything.iter().map(|e| e.log).collect();
    assert!(fire_logs.contains(&alice_log), "firehose sees my mints");
    assert!(fire_logs.contains(&bob_log), "firehose sees what I follow");
    assert!(!mine.is_empty());
    assert!(
        mine.iter().all(|e| e.log == alice_log),
        "authored is my voice only"
    );
}

#[test]
fn drafts_mint_atomically_and_a_penless_peer_cannot_speak() {
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (reader, reader_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(reader_actor.run()).unwrap();

    pool.run_until(async {
        let event = alice
            .claim(
                Draft::new("rec")
                    .at(5)
                    .text("subject", "Joe's Pizza")
                    .int("rating", 5)
                    .attach("photo", b"crusty".to_vec(), "image/jpeg"),
            )
            .await
            .unwrap();
        let id = event.id();
        let ok = alice
            .query(move |db| {
                let claim = db.claims().get(&id).unwrap();
                let Some(Value::Map(body)) = claim.body else {
                    return false;
                };
                let Some(Value::BlobRef(blob)) = body.get("photo") else {
                    return false;
                };
                // Attachment stored AND pinned, in one mint.
                db.blobs().get(&blob.hash).as_deref() == Some(b"crusty".as_slice())
                    && body.get("rating") == Some(&Value::Int(5))
            })
            .await
            .unwrap();
        assert!(ok);

        let err = reader.claim(rec("not allowed", 1)).await;
        assert!(err.is_err(), "a peer without a pen cannot claim");
    });
}

#[test]
fn media_reaches_the_relay_eagerly_and_readers_on_demand() {
    // The relay (ServePolicy::Everything) keeps itself stocked: when a
    // publish lands claims pinning media it lacks, it pulls the bytes
    // back up the same duct — eager by role. Readers stay lazy and
    // demand bytes when their UI cares.
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();
    let (relay, relay_actor) = make_peer(None, 9, ServePolicy::Everything);
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(relay_actor.run()).unwrap();
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();

    pool.run_until(async {
        let (_, alice_pipe) = link(&relay, "alice", &alice, "relay").await;
        alice.follow(alice_log, alice_pipe).await.unwrap();
        let (_, bob_pipe) = link(&relay, "bob", &bob, "relay").await;
        bob.follow(alice_log, bob_pipe).await.unwrap();
    });
    pool.run_until_stalled();

    let event = pool.run_until(async {
        alice
            .claim(rec("photo post", 1).attach("photo", b"the photo".to_vec(), "image/jpeg"))
            .await
            .unwrap()
    });
    pool.run_until_stalled();

    let id = event.id();
    let hash = pool
        .run_until(alice.query(move |db| db.claims().get(&id).unwrap().blobs[0].1.hash))
        .unwrap();
    let (relay_claims, relay_blob, relay_wants) = pool
        .run_until(relay.query(move |db| {
            (
                db.claims().log_len(&alice_log),
                db.blobs().contains(&hash),
                db.missing_blobs().len(),
            )
        }))
        .unwrap();
    assert_eq!(relay_claims, 1);
    assert!(
        relay_blob,
        "the relay pulled the media back from its publisher"
    );
    assert_eq!(relay_wants, 0);

    // Bob got the claim (re-fanned frame) but not the bytes — lazy.
    let (bob_claims, bob_blob) = pool
        .run_until(
            bob.query(move |db| (db.claims().log_len(&alice_log), db.blobs().contains(&hash))),
        )
        .unwrap();
    assert_eq!(bob_claims, 1);
    assert!(!bob_blob, "readers don't sync media until they ask");

    // Bob's UI asks; the relay answers.
    pool.run_until(bob.fetch_blob(hash)).unwrap();
    pool.run_until_stalled();
    let bob_bytes = pool
        .run_until(bob.query(move |db| db.blobs().get(&hash)))
        .unwrap();
    assert_eq!(bob_bytes.as_deref(), Some(b"the photo".as_slice()));
}

#[test]
fn an_eager_p2p_pipe_takes_the_photos_while_it_can() {
    // P2P posture: your friend's phone won't always be reachable, so an
    // eager pipe grabs media the moment its claims arrive — fed by the
    // PutBlob fast-track (the holder answers the GetBlob it knows is
    // coming) and backstopped by the pull.
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();

    pool.run_until(async {
        let (a_end, b_end) = pipe(256);
        alice.connect("bob", a_end).await.unwrap();
        let on_bob = bob
            .connect_with("alice", b_end, PipeConfig { eager_media: true })
            .await
            .unwrap();
        bob.follow(alice_log, on_bob).await.unwrap();
    });
    pool.run_until_stalled();

    let event = pool.run_until(async {
        alice
            .claim(rec("look at this", 1).attach("photo", b"vacation pic".to_vec(), "image/jpeg"))
            .await
            .unwrap()
    });
    pool.run_until_stalled();

    let id = event.id();
    let hash = pool
        .run_until(alice.query(move |db| db.claims().get(&id).unwrap().blobs[0].1.hash))
        .unwrap();
    let bob_bytes = pool
        .run_until(bob.query(move |db| db.blobs().get(&hash)))
        .unwrap();
    assert_eq!(
        bob_bytes.as_deref(),
        Some(b"vacation pic".as_slice()),
        "eager pipe took the media with no demand call"
    );
}

#[test]
fn a_relay_that_culled_media_restocks_from_the_authors_next_activity() {
    // The website model cuts both ways: the relay can cull bytes too.
    // The claim keeps the want alive, and the relay's eager fetch
    // restocks it from the author the next time her pipe carries claims.
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();
    let (relay, relay_actor) = make_peer(None, 9, ServePolicy::Everything);
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    spawner.spawn(relay_actor.run()).unwrap();
    spawner.spawn(alice_actor.run()).unwrap();
    let alice_log = alice.id().unwrap();

    let event = pool.run_until(async {
        let (_, alice_pipe) = link(&relay, "alice", &alice, "relay").await;
        alice.follow(alice_log, alice_pipe).await.unwrap();
        alice
            .claim(rec("banner", 1).attach("banner", b"banner image".to_vec(), "image/png"))
            .await
            .unwrap()
    });
    pool.run_until_stalled();
    let id = event.id();
    let hash = pool
        .run_until(alice.query(move |db| db.claims().get(&id).unwrap().blobs[0].1.hash))
        .unwrap();
    let stocked = pool
        .run_until(relay.query(move |db| db.blobs().contains(&hash)))
        .unwrap();
    assert!(stocked);

    // Storage pressure at the relay: cull. Claims untouched.
    assert!(pool.run_until(relay.evict_blob(hash)).unwrap());

    // Alice posts anything at all; the relay restocks everything her log
    // still pins — including the culled banner.
    pool.run_until(async {
        alice.claim(rec("unrelated post", 2)).await.unwrap();
    });
    pool.run_until_stalled();
    let restocked = pool
        .run_until(relay.query(move |db| (db.blobs().contains(&hash), db.missing_blobs().len())))
        .unwrap();
    assert_eq!(restocked, (true, 0), "the culled banner came back");
}

#[test]
fn quoted_media_routes_through_the_quoters_log() {
    // The routing dividend of "a quote pins its own media": carol's rec
    // carries a photo, alice quotes the rec, and bob follows ONLY alice.
    // At bob, the photo's referrer is ALICE's vouch — in a log he follows —
    // so his demand routes over the alice pipe; and at alice (serving only
    // her own log), the same attribution makes the photo servable. If the
    // referrer were carol's claim, bob couldn't route the demand and alice
    // would refuse to serve it: media inside quotes would be unfetchable
    // without following the quoted author.
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (carol, carol_actor) = make_peer(Some(3), 3, ServePolicy::Owned);
    let (alice, alice_actor) = make_peer(Some(1), 1, ServePolicy::Owned);
    let (bob, bob_actor) = make_peer(None, 2, ServePolicy::Owned);
    spawner.spawn(carol_actor.run()).unwrap();
    spawner.spawn(alice_actor.run()).unwrap();
    spawner.spawn(bob_actor.run()).unwrap();
    let carol_log = carol.id().unwrap();
    let alice_log = alice.id().unwrap();

    // Carol's photo rec reaches alice; alice pulls the photo to hold it.
    let (carol_rec, photo) = pool.run_until(async {
        let event = carol
            .claim(rec("mole negro", 1).attach("photo", b"a mole photo".to_vec(), "image/jpeg"))
            .await
            .unwrap();
        let id = event.id();
        let photo = carol
            .query(move |db| db.claims().get(&id).unwrap().blobs[0].1.hash)
            .await
            .unwrap();
        let (_, on_alice) = link(&carol, "alice", &alice, "carol").await;
        alice.follow(carol_log, on_alice).await.unwrap();
        (event, photo)
    });
    pool.run_until_stalled();
    pool.run_until(alice.fetch_blob(photo)).unwrap();
    pool.run_until_stalled();

    // Alice quotes the rec; bob follows only alice.
    pool.run_until(async {
        alice
            .claim(Draft::new("vouch").at(2).embed("original", carol_rec))
            .await
            .unwrap();
        let (_, on_bob) = link(&alice, "bob", &bob, "alice").await;
        bob.follow(alice_log, on_bob).await.unwrap();
    });
    pool.run_until_stalled();

    // The quote arrived as alice's speech; carol's log is unknown to bob,
    // and the photo is a want he can satisfy without ever meeting carol.
    let (carol_rows, has_photo) = pool
        .run_until(bob.query(move |db| {
            (
                db.claims().log_len(&carol_log),
                db.blobs().get(&photo).is_some(),
            )
        }))
        .unwrap();
    assert_eq!(carol_rows, 0, "bob never met carol");
    assert!(!has_photo, "media is non-syncing by default");

    pool.run_until(bob.fetch_blob(photo)).unwrap();
    pool.run_until_stalled();
    let has_photo = pool
        .run_until(bob.query(move |db| db.blobs().get(&photo).is_some()))
        .unwrap();
    assert!(
        has_photo,
        "the demand routed through the QUOTE's log and alice served it"
    );
}

#[test]
fn gc_claims_older_than_reaches_through_the_actor() {
    // The verb a relay's maintenance loop calls: purge by age, through the
    // same command channel every other operation uses — no back door into
    // the actor's own Database.
    let mut pool = LocalPool::new();
    let spawner = pool.spawner();

    let (relay, actor) = make_peer(None, 1, ServePolicy::Everything);
    spawner.spawn(actor.run()).unwrap();

    // `alice` must outlive the drive-to-quiescence below: dropping the
    // handle closes the actor's command channel, and the actor may then
    // exit before finishing its publish session to the relay.
    let (alice, alice_log) = pool.run_until(async {
        let (alice, alice_actor) = make_peer(Some(1), 2, ServePolicy::Owned);
        spawner.spawn(alice_actor.run()).unwrap();
        let alice_log = alice.id().unwrap();
        let (alice_end, relay_end) = pipe(256);
        relay.connect("alice", relay_end).await.unwrap();
        let on_alice = alice.connect("relay", alice_end).await.unwrap();
        alice.follow(alice_log, on_alice).await.unwrap();
        alice
            .claim(Draft::new("rec").at(1).text("subject", "old"))
            .await
            .unwrap();
        (alice, alice_log)
    });
    pool.run_until_stalled();
    drop(alice);

    let count = pool
        .run_until(relay.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    assert_eq!(count, 1, "the relay actually received alice's push");

    let purged = pool.run_until(relay.gc_claims_older_than(5_000)).unwrap();
    assert_eq!(purged.len(), 1);

    let count = pool
        .run_until(relay.query(move |db| db.claims().log_len(&alice_log)))
        .unwrap();
    assert_eq!(count, 0, "gc reached the actor's own Database");
}
