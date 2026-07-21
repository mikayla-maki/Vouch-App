//! Fault injection at every message boundary.
//!
//! The session's whole crash-safety claim is: abandoning it between any
//! request and the next — timeout, disconnect, process death — costs
//! nothing but a retry, because cursors trail ingest and peers hold no
//! conversation state. So: run a rich scenario, kill the session after
//! every possible prefix of its messages, resume with a fresh session each
//! time, and demand bit-identical convergence and a clean fsck. The same
//! exhaustive-prefix trick the storage layer uses for write crashes,
//! lifted to the network.

use vouch_core::sync::{
    Error, InstanceId, MemorySyncState, PeerCursor, SyncReport, SyncSession, SyncState, drive,
    respond,
};
use vouch_core::{ClaimRef, Database, LogId, Event, Value, Writer};

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

/// The scenario: enough going on that one session exercises every phase —
/// pull (two logs' worth), push, blob transfer in both directions, a
/// redaction tombstone, and a stripped body that needs reconciliation.
///
/// Returns (client, client_state, relay, relay_instance) freshly built —
/// deterministic, so every abort point starts from identical worlds.
fn build_world() -> (Database, MemorySyncState, Database, InstanceId) {
    // The friend's log lives on the relay: recs, one redacted, one whose
    // body the relay never got (stripped pipe). Media is deliberately
    // absent: sessions are claims-only (blob transfer is pull-only at the
    // actor layer), so it contributes nothing to the fault surface here.
    let mut friend = Database::new();
    let friend_log = friend.add_writer(Writer::from_seed([21; 32]));
    rec(&mut friend, &friend_log, 1, "soup place, no photo today");
    let regret = rec(&mut friend, &friend_log, 2, "regrettable take");
    friend
        .claim(
            &friend_log,
            Value::map([
                ("type", Value::text("redact")),
                ("at", Value::Int(3)),
                (
                    "redacts",
                    Value::ClaimRef(ClaimRef {
                        log_id: friend_log,
                        hash: regret.id(),
                    }),
                ),
            ]),
        )
        .unwrap();
    let stripped = rec(&mut friend, &friend_log, 4, "body withheld by a lossy pipe");
    for i in 0..4 {
        rec(&mut friend, &friend_log, 10 + i, &format!("rec {i}"));
    }

    let mut relay = Database::new();
    for event in friend.claims().events() {
        if event.id() == stripped.id() {
            relay.ingest(event.without_body()).unwrap();
        } else {
            relay.ingest(event).unwrap();
        }
    }

    // The client owns its own log to push, and already holds the
    // stripped claim's full body (heard from the friend directly) — so
    // reconciliation must flow toward the relay too.
    let mut client = Database::new();
    let client_log = client.add_writer(Writer::from_seed([22; 32]));
    rec(&mut client, &client_log, 5, "one of mine");
    rec(&mut client, &client_log, 6, "another of mine");
    client.ingest(stripped).unwrap();

    (client, MemorySyncState::new(), relay, InstanceId([1; 16]))
}

fn plan() -> (Vec<LogId>, Vec<LogId>) {
    let friend_log = Writer::from_seed([21; 32]).id();
    let client_log = Writer::from_seed([22; 32]).id();
    (vec![friend_log, client_log], vec![client_log])
}

/// Drive a session but abort (drop it) after `budget` exchanges.
fn drive_at_most(
    client: &mut Database,
    state: &mut MemorySyncState,
    relay: &mut Database,
    instance: InstanceId,
    budget: usize,
) -> usize {
    let (pull, push) = plan();
    let mut session = SyncSession::new("relay", 1000, pull, push);
    let mut used = 0;
    while let Some(request) = session.next_request(client) {
        if used == budget {
            return used; // the "crash": session dropped mid-flight
        }
        used += 1;
        let response = respond(relay, instance, 1000, request).unwrap();
        session.feed(client, state, response).unwrap();
    }
    used
}

#[test]
fn a_session_killed_at_every_message_boundary_converges_on_retry() {
    // How many messages does an unmolested run take?
    let (mut client, mut state, mut relay, instance) = build_world();
    let total = drive_at_most(&mut client, &mut state, &mut relay, instance, usize::MAX);
    assert!(total >= 8, "scenario too small to be interesting: {total}");
    let expected_client = client.claims().state_vector();
    let expected_relay = relay.claims().state_vector();

    for kill_at in 0..total {
        let (mut client, mut state, mut relay, instance) = build_world();
        drive_at_most(&mut client, &mut state, &mut relay, instance, kill_at);
        // The retry: one fresh session finishes the job from wherever the
        // corpse left the cursors.
        drive_at_most(&mut client, &mut state, &mut relay, instance, usize::MAX);

        assert_eq!(
            client.claims().state_vector(),
            expected_client,
            "client diverged when killed after message {kill_at}"
        );
        assert_eq!(
            relay.claims().state_vector(),
            expected_relay,
            "relay diverged when killed after message {kill_at}"
        );
        assert!(client.claims().verify_integrity().is_empty());
        assert!(relay.claims().verify_integrity().is_empty());
    }
}

#[test]
fn a_transport_retry_of_the_same_request_is_idempotent() {
    // next_request() is a peek: a transport that times out and re-sends the
    // same request (the response to the first attempt was lost) must not
    // confuse the session — it feeds the second response to the same
    // outstanding request.
    let (mut client, mut state, mut relay, instance) = build_world();
    let (pull, push) = plan();
    let mut session = SyncSession::new("relay", 1000, pull, push);
    while let Some(request) = session.next_request(&client) {
        let retried = session
            .next_request(&client)
            .expect("peek must not consume");
        assert_eq!(request, retried);
        // First response lost in flight; the peer answered both times.
        let _lost = respond(&mut relay, instance, 1000, request.clone()).unwrap();
        let response = respond(&mut relay, instance, 1000, retried).unwrap();
        session.feed(&mut client, &mut state, response).unwrap();
    }
    let (mut once_client, mut once_state, mut once_relay, once_instance) = build_world();
    drive_at_most(
        &mut once_client,
        &mut once_state,
        &mut once_relay,
        once_instance,
        usize::MAX,
    );
    assert_eq!(
        client.claims().state_vector(),
        once_client.claims().state_vector()
    );
    assert_eq!(
        relay.claims().state_vector(),
        once_relay.claims().state_vector()
    );
}

#[test]
fn cursors_never_run_ahead_of_ingested_data() {
    // The monotonicity bargain, checked at every abort point: whatever the
    // pull cursor claims we have of the friend's log, we actually hold at
    // least that many of its claims. (Holding MORE is fine — embeds and
    // direct ingest don't owe the cursor anything.)
    let (friend_log, _) = {
        let (pull, push) = plan();
        (pull[0], push)
    };
    let (mut probe_client, mut probe_state, mut probe_relay, instance) = build_world();
    let total = drive_at_most(
        &mut probe_client,
        &mut probe_state,
        &mut probe_relay,
        instance,
        usize::MAX,
    );

    for kill_at in 0..total {
        let (mut client, mut state, mut relay, instance) = build_world();
        drive_at_most(&mut client, &mut state, &mut relay, instance, kill_at);
        let cursor = state.cursor("relay", &friend_log).unwrap();
        assert!(
            client.claims().log_len(&friend_log) >= cursor.pull,
            "cursor ran ahead of data when killed after message {kill_at}: \
             cursor.pull={} held={}",
            cursor.pull,
            client.claims().log_len(&friend_log)
        );
        assert!(cursor.pull <= relay.claims().log_len(&friend_log));
    }
}

#[test]
fn a_mid_session_transport_error_aborts_cleanly() {
    let (mut client, mut state, mut relay, instance) = build_world();
    let (pull, push) = plan();
    let mut calls = 0;
    let session = SyncSession::new("relay", 1000, pull.clone(), push.clone());
    let result = drive(&mut client, &mut state, session, |req| {
        calls += 1;
        if calls == 3 {
            return Err(Error::Protocol("connection reset by peer".into()));
        }
        respond(&mut relay, instance, 1000, req).map_err(Error::from)
    });
    assert!(result.is_err());

    // Recovery is always the same move: a fresh session.
    let session = SyncSession::new("relay", 1000, pull, push);
    let report: SyncReport = drive(&mut client, &mut state, session, |req| {
        respond(&mut relay, instance, 1000, req).map_err(Error::from)
    })
    .unwrap();
    assert!(client.claims().verify_integrity().is_empty());
    assert_eq!(report.rejected_events, 0);
}

#[test]
fn duplicated_and_replayed_responses_cannot_double_apply() {
    // A confused transport delivers each EVENTS response twice in a row —
    // the session must reject the unsolicited second copy (no outstanding
    // request), and state must not double-advance.
    let (mut client, mut state, mut relay, instance) = build_world();
    let (pull, push) = plan();
    let mut session = SyncSession::new("relay", 1000, pull, push);
    while let Some(request) = session.next_request(&client) {
        let response = respond(&mut relay, instance, 1000, request).unwrap();
        let dup = response.clone();
        session.feed(&mut client, &mut state, response).unwrap();
        if session.next_request(&client).is_none() {
            // Session finished: the replayed frame must be refused.
            assert!(matches!(
                session.feed(&mut client, &mut state, dup),
                Err(Error::Protocol(_))
            ));
            break;
        }
        // Mid-session, a duplicate frame is either refused (wrong shape for
        // the new outstanding request) or harmless by idempotence; both are
        // fine — what's forbidden is silent double-application, which the
        // convergence check below would catch.
        let _ = session.feed(&mut client, &mut state, dup);
        // feed() may have consumed the duplicate as the answer to the next
        // request; rebuild the loop's invariant by continuing — the final
        // assertion is the arbiter.
        if session.next_request(&client).is_none() {
            break;
        }
    }
    // However the duplicates interleaved, a final clean session must land
    // both sides exactly where a fault-free run does.
    let report = {
        let (pull, push) = plan();
        let session = SyncSession::new("relay", 1000, pull, push);
        drive(&mut client, &mut state, session, |req| {
            respond(&mut relay, instance, 1000, req).map_err(Error::from)
        })
        .unwrap()
    };
    assert_eq!(report.rejected_events, 0);

    let (mut once_client, mut once_state, mut once_relay, once_instance) = build_world();
    drive_at_most(
        &mut once_client,
        &mut once_state,
        &mut once_relay,
        once_instance,
        usize::MAX,
    );
    assert_eq!(
        client.claims().state_vector(),
        once_client.claims().state_vector()
    );
    assert_eq!(
        relay.claims().state_vector(),
        once_relay.claims().state_vector()
    );
    assert!(client.claims().verify_integrity().is_empty());
    assert!(relay.claims().verify_integrity().is_empty());
}

/// Cursor rows must reload exactly (the memory impl is the reference the
/// SQLite impl is tested against in vouch-store).
#[test]
fn cursor_rows_round_trip_through_the_state_trait() {
    let mut state = MemorySyncState::new();
    let log = Writer::from_seed([30; 32]).id();
    let cursor = PeerCursor {
        instance: Some(InstanceId([7; 16])),
        pull: 41,
        push: 12,
        settled: Some([9; 32]),
    };
    state.set_cursor("relay", &log, cursor).unwrap();
    assert_eq!(state.cursor("relay", &log).unwrap(), cursor);
    assert_eq!(state.cursor("other", &log).unwrap(), PeerCursor::default());
}
