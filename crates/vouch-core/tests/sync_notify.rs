//! The push path: Notify frames applied with zero round trips — and every
//! way a frame can be late, lying, or insufficient degrading safely into
//! "run a session".

use vouch_core::sync::{
    Error, InstanceId, MemorySyncState, Notify, Request, Response, SyncReport, SyncSession,
    SyncState, apply_notify, drive, notify_for, respond,
};
use vouch_core::{ClaimRef, Database, LogId, Event, Value, Writer};

struct Peer {
    db: Database,
    instance: InstanceId,
}

impl Peer {
    fn new(tag: u8) -> Peer {
        Peer {
            db: Database::new(),
            instance: InstanceId([tag; 16]),
        }
    }
}

fn sync(
    client: &mut Database,
    state: &mut MemorySyncState,
    peer: &mut Peer,
    pull: &[LogId],
    push: &[LogId],
) -> SyncReport {
    let session = SyncSession::new("relay", 1000, pull.to_vec(), push.to_vec());
    drive(client, state, session, |req| {
        respond(&mut peer.db, peer.instance, 1000, req).map_err(Error::from)
    })
    .unwrap()
}

/// Count the messages a follow-up session needs — the proof of how much
/// the push already accomplished. A fully settled log re-syncs in exactly
/// one Status message with nothing to report.
fn session_cost(
    client: &mut Database,
    state: &mut MemorySyncState,
    peer: &mut Peer,
    log: LogId,
) -> (usize, SyncReport) {
    let mut messages = 0;
    let session = SyncSession::new("relay", 2000, vec![log], vec![]);
    let report = drive(client, state, session, |req| {
        messages += 1;
        respond(&mut peer.db, peer.instance, 2000, req).map_err(Error::from)
    })
    .unwrap();
    (messages, report)
}

fn rec(db: &mut Database, log: &LogId, at: i64, text: &str) -> Event {
    db.claim(
        log,
        Value::map([
            ("type", Value::text("rec")),
            ("at", Value::Int(at)),
            ("body", Value::text(text)),
        ]),
    )
    .unwrap()
}

fn redact(db: &mut Database, log: &LogId, at: i64, target: &Event) -> Event {
    db.claim(
        log,
        Value::map([
            ("type", Value::text("redact")),
            ("at", Value::Int(at)),
            (
                "redacts",
                Value::ClaimRef(ClaimRef {
                    log_id: *log,
                    hash: target.id(),
                }),
            ),
        ]),
    )
    .unwrap()
}

/// Author publishes to the relay and the relay builds the fan-out frame —
/// the relay shim's exact move after a Publish lands.
fn publish_and_notify(
    author: &mut Database,
    relay: &mut Peer,
    log: &LogId,
    event: Event,
) -> Notify {
    author.ingest(event.clone()).ok();
    relay.db.ingest(event.clone()).unwrap();
    notify_for(&relay.db, relay.instance, log, vec![event])
}

/// A reader fully synced with a relay carrying `n` claims.
fn settled_reader(relay: &mut Peer, log: LogId, expect: usize) -> (Database, MemorySyncState) {
    let mut reader = Database::new();
    let mut state = MemorySyncState::new();
    let report = sync(&mut reader, &mut state, relay, &[log], &[]);
    assert_eq!(report.pulled, expect);
    (reader, state)
}

#[test]
fn a_pushed_claim_applies_instantly_with_zero_round_trips() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([1; 32]));
    rec(&mut author, &log, 1, "old news");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);

    // The author posts; the relay fans out; the reader applies — no pipe
    // back toward the relay exists in this test at all.
    let fresh = rec(&mut author, &log, 2, "hot off the press");
    let frame = publish_and_notify(&mut author, &mut relay, &log, fresh.clone());
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();

    assert!(report.settled);
    assert_eq!(report.pulled, 1);
    assert!(reader.claims().contains(&fresh.id()));
    assert_eq!(
        reader.claims().fingerprint(&log),
        relay.db.claims().fingerprint(&log)
    );

    // The cursor fast-forwarded: a follow-up session has nothing to do
    // and nothing to ask beyond one Status.
    let (messages, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(messages, 1);
    assert_eq!(report, SyncReport::default());
}

#[test]
fn an_empty_frame_is_a_heartbeat_that_confirms_or_denies_settledness() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([2; 32]));
    rec(&mut author, &log, 1, "the state of things");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);

    // Heartbeat while in agreement: settled, nothing pulled.
    let beat = notify_for(&relay.db, relay.instance, &log, vec![]);
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, beat).unwrap();
    assert!(report.settled);
    assert_eq!(report.pulled, 0);

    // The relay moves on without us (a claim arrived from elsewhere); the
    // next heartbeat says "you're behind" — without carrying the claim.
    let elsewhere = rec(&mut author, &log, 2, "you missed this");
    relay.db.ingest(elsewhere).unwrap();
    let beat = notify_for(&relay.db, relay.instance, &log, vec![]);
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1600, beat).unwrap();
    assert!(!report.settled);

    // The doorbell rings true: one session catches up.
    let (_, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(report.pulled, 1);
    assert_eq!(
        reader.claims().fingerprint(&log),
        relay.db.claims().fingerprint(&log)
    );
}

#[test]
fn a_reader_holding_extras_stays_settled_homomorphically() {
    // The relay only ever holds part of the log (the reader can't publish
    // there); after one reconciliation the reader caches the relay's
    // fingerprint as the known benign difference. Pushes must keep that
    // cache fresh WITHOUT a session — by XOR-advancing it.
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([3; 32]));
    let direct = rec(&mut author, &log, 1, "handed to the reader directly");
    let relayed = rec(&mut author, &log, 2, "went through the relay");

    let mut relay = Peer::new(1);
    relay.db.ingest(relayed).unwrap();

    let mut reader = Database::new();
    reader.ingest(direct).unwrap();
    let mut reader_state = MemorySyncState::new();
    // First sync reconciles once and learns the difference is benign.
    let report = sync(&mut reader, &mut reader_state, &mut relay, &[log], &[]);
    assert_eq!(report.reconciled, 1);

    // Push three claims in a row; every one must land settled, no session.
    for at in 10..13 {
        let fresh = rec(&mut author, &log, at, "new post");
        let frame = publish_and_notify(&mut author, &mut relay, &log, fresh.clone());
        let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();
        assert!(report.settled, "push at={at} should ride the settled cache");
        assert!(reader.claims().contains(&fresh.id()));
    }

    // And the cache really does match reality: the next session is one
    // Status message, no reconciliation.
    let (messages, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(messages, 1);
    assert_eq!(report.reconciled, 0);
}

#[test]
fn a_pushed_redaction_takes_effect_at_push_speed() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([4; 32]));
    let regret = rec(&mut author, &log, 1, "delete this later");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);
    assert!(reader.claims().contains(&regret.id()));

    let tombstone = redact(&mut author, &log, 2, &regret);
    let frame = publish_and_notify(&mut author, &mut relay, &log, tombstone);
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();

    // The takedown landed instantly — and the delta model covered the
    // redaction entry AND the target's body-bit flip, so we're still
    // settled with zero round trips.
    assert_eq!(report.redactions_applied, 1);
    assert!(!reader.claims().contains(&regret.id()));
    assert!(reader.claims().redaction(&regret.id()).is_some());
    assert!(report.settled);
    let (messages, _) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(messages, 1);
}

#[test]
fn a_missed_push_degrades_the_next_one_into_a_doorbell() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([5; 32]));
    rec(&mut author, &log, 1, "base");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);

    // Frame one is lost in flight; frame two arrives.
    let missed = rec(&mut author, &log, 2, "lost frame");
    let _lost = publish_and_notify(&mut author, &mut relay, &log, missed.clone());
    let arrived = rec(&mut author, &log, 3, "delivered frame");
    let frame = publish_and_notify(&mut author, &mut relay, &log, arrived.clone());

    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();
    // The delivered claim is held (free content), but the fingerprint
    // exposes the gap — doorbell.
    assert!(reader.claims().contains(&arrived.id()));
    assert!(!report.settled);

    // The session the doorbell asks for fetches exactly the missed claim.
    let (_, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(report.pulled, 1);
    assert!(reader.claims().contains(&missed.id()));
    assert_eq!(
        reader.claims().state_vector(),
        relay.db.claims().state_vector()
    );
}

#[test]
fn a_lying_frame_cannot_poison_anything() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([6; 32]));
    rec(&mut author, &log, 1, "true state");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);
    let before = reader.claims().state_vector();
    let cursor_before = reader_state.cursor("relay", &log).unwrap();

    // A hostile push: an event whose body was swapped after signing, a
    // perfectly valid claim from a log we never asked about, and forged
    // coordinates (absurd count, garbage fingerprint).
    let mut tampered = rec(&mut author, &log, 9, "original words");
    tampered.body_bytes = Some(
        rec(&mut author, &log, 9, "replaced words")
            .body_bytes
            .clone()
            .unwrap(),
    );
    let mut smuggler = Database::new();
    let other_log = smuggler.add_writer(Writer::from_seed([7; 32]));
    let smuggled = rec(&mut smuggler, &other_log, 1, "unsolicited");
    let frame = Notify {
        log,
        events: vec![tampered, smuggled],
        count: 1_000_000,
        fingerprint: [0xAA; 32],
        instance: relay.instance,
    };
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();

    assert_eq!(report.rejected_events, 1);
    assert_eq!(report.off_plan, 1);
    assert!(!report.settled);
    // Nothing stored, and crucially: the forged count did NOT move the
    // cursor — fast-forward happens only on fingerprint match.
    assert_eq!(reader.claims().state_vector(), before);
    assert_eq!(reader.claims().log_len(&other_log), 0);
    assert_eq!(
        reader_state.cursor("relay", &log).unwrap().pull,
        cursor_before.pull
    );
    assert!(reader.claims().verify_integrity().is_empty());
}

#[test]
fn pushed_media_lands_as_content_plus_a_want() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([8; 32]));
    rec(&mut author, &log, 1, "base");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);

    let photo = b"sunset, probably".to_vec();
    let blob_ref = author.attach(photo.clone(), "image/jpeg").unwrap();
    let event = author
        .claim(
            &log,
            Value::map([
                ("type", Value::text("rec")),
                ("at", Value::Int(2)),
                ("photo", Value::BlobRef(blob_ref.clone())),
            ]),
        )
        .unwrap();
    relay.db.ingest(event.clone()).unwrap();
    relay.db.ingest_blob(blob_ref.hash, photo.clone()).unwrap();
    let frame = notify_for(&relay.db, relay.instance, &log, vec![event]);

    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, frame).unwrap();
    // The claim is settled instantly; the bytes are an explicit want the
    // caller acts on when it cares (UI demand, or an eager pipe's policy).
    assert!(report.settled);
    assert_eq!(report.missing_blobs.len(), 1);
    assert_eq!(report.missing_blobs[0].hash, blob_ref.hash);

    // Sync itself never moves the bytes: an idle session stays idle.
    let (messages, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(messages, 1);
    assert_eq!(report, SyncReport::default());
    // One demand-driven pull closes the want.
    let answer = respond(
        &mut relay.db,
        relay.instance,
        1500,
        Request::GetBlob {
            hash: blob_ref.hash,
        },
    )
    .unwrap();
    let Response::Blob { bytes: Some(bytes) } = answer else {
        panic!("relay holds the bytes");
    };
    assert!(reader.ingest_blob(blob_ref.hash, bytes).unwrap());
    assert_eq!(reader.blobs().get(&blob_ref.hash).unwrap(), photo);
}

#[test]
fn a_frame_from_a_reborn_relay_adopts_the_new_instance_when_sets_agree() {
    let mut author = Database::new();
    let log = author.add_writer(Writer::from_seed([9; 32]));
    rec(&mut author, &log, 1, "persistent content");
    let mut relay = Peer::new(1);
    let mut author_state = MemorySyncState::new();
    sync(&mut author, &mut author_state, &mut relay, &[], &[log]);
    let (mut reader, mut reader_state) = settled_reader(&mut relay, log, 1);

    // The relay reboots with its data intact but a fresh instance, and the
    // first thing the reader hears is a heartbeat. Same set → adopt the
    // incarnation, fully settled, zero round trips — no re-download at all.
    let relay = Peer {
        db: relay.db,
        instance: InstanceId([99; 16]),
    };
    let beat = notify_for(&relay.db, relay.instance, &log, vec![]);
    let report = apply_notify(&mut reader, &mut reader_state, "relay", 1500, beat).unwrap();
    assert!(report.settled);
    let cursor = reader_state.cursor("relay", &log).unwrap();
    assert_eq!(cursor.instance, Some(InstanceId([99; 16])));
    assert_eq!(cursor.pull, 1);

    let mut relay = relay;
    let (messages, report) = session_cost(&mut reader, &mut reader_state, &mut relay, log);
    assert_eq!(messages, 1);
    assert_eq!(report, SyncReport::default());
}
