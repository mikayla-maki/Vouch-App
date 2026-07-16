//! The push path: apply a server-initiated [`Notify`] frame.
//!
//! Push is layered on coordinates the session machinery already knows how
//! to judge. The frame carries new events plus the sender's
//! `(count, fingerprint, instance)`; applying it is "ingest, then settle".
//! Three outcomes, in declining order of luck:
//!
//! - **Settled, fingerprints equal**: we now provably hold the sender's
//!   exact set, so the pull cursor fast-forwards to `count` — a *stronger*
//!   justification than the session's normal advance (set equality, not
//!   just positions consumed). Zero round trips.
//! - **Settled via the cache**: we hold *more* than the sender (the known
//!   benign difference — e.g. a relay that won't take our third-party
//!   claims). The remembered `settled` fingerprint is advanced
//!   *homomorphically*: the fingerprint is an XOR fold, so the sender's
//!   new fingerprint is computable from the old one plus the pushed claim
//!   — no round trip even though our sets still differ. The cursor
//!   fast-forwards here too: a cache match proves the sender's set is one
//!   we reconciled to and then tracked, i.e. a subset of ours.
//! - **Not settled**: the frame degrades into a doorbell with a free claim
//!   attached. Hold what was pushed (ingest already verified it), report
//!   `settled: false`, and let the app run an ordinary session.
//!
//! Nothing here can be made unsafe by a lying sender: events verify at
//! ingest, coordinates are advisory (the cursor only fast-forwards on
//! fingerprint match, and the next session's settle catches real
//! divergence), and a wrong homomorphic guess just fails the equality
//! check and costs one session.

use crate::{
    BlobRef, Database, Error as CoreError, LogId, SignedEvent, Value, cbor, fingerprint_claim,
    fingerprint_redaction, redact_target,
};

use super::error::Error;
use super::protocol::{InstanceId, Notify};
use super::state::{PeerCursor, SyncState};

/// What applying one frame accomplished.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct NotifyReport {
    /// Claims newly stored (including embedded ones).
    pub pulled: usize,
    /// Bodies attached to claims we previously held stripped.
    pub healed: usize,
    /// Redactions that took effect — takedowns land at push speed.
    pub redactions_applied: usize,
    /// Events that failed verification (dropped; sender misbehavior).
    pub rejected_events: usize,
    /// Events outside the frame's own log (dropped; a push channel cannot
    /// smuggle logs any more than a pull can).
    pub off_plan: usize,
    /// Blobs the pushed bodies pin that we don't hold — fetch via
    /// `GetBlob` on any pipe, or let the next session's blob phase heal
    /// them.
    pub missing_blobs: Vec<BlobRef>,
    /// True: nothing to ask the sender; we are as caught up as we can be.
    /// False: the sets disagree beyond what we can explain — run a
    /// session.
    pub settled: bool,
}

/// Build the fan-out frame for one log — the sender half. A relay calls
/// this after a `Publish` lands (with the events it just stored); with no
/// events it is a heartbeat.
pub fn notify_for(
    db: &Database,
    instance: InstanceId,
    log: &LogId,
    events: Vec<SignedEvent>,
) -> Notify {
    Notify {
        log: *log,
        events,
        count: db.claims().log_len(log),
        fingerprint: db.claims().fingerprint(log),
        instance,
    }
}

/// Apply one push frame — the receiver half. See the module docs for the
/// outcome ladder; only local storage failure is an error.
pub fn apply_notify(
    db: &mut Database,
    state: &mut dyn SyncState,
    peer: &str,
    now: i64,
    notify: Notify,
) -> Result<NotifyReport, Error> {
    let mut report = NotifyReport::default();
    let mut cursor = state.cursor(peer, &notify.log)?;
    if cursor.instance != Some(notify.instance) {
        // The sender's arrival order was reborn; its counts mean nothing
        // to our old row. Start the row over — if the fingerprint matches
        // below we'll adopt the new incarnation fully settled anyway.
        cursor = PeerCursor {
            instance: Some(notify.instance),
            ..PeerCursor::default()
        };
    }

    for event in notify.events {
        if event.header().ok().map(|h| h.log_id) != Some(notify.log) {
            report.off_plan += 1;
            continue;
        }
        // Model the sender's fingerprint delta BEFORE ingest (the redact
        // branch reads our pre-ingest state), apply it to the settled
        // cache only after the event proves ingestible.
        let delta = sender_delta(db, &notify.log, &event);
        match db.ingest_at(event, now) {
            Ok(r) => {
                report.pulled += r.newly_stored.is_some() as usize;
                report.healed += r.bodies_attached;
                report.redactions_applied += r.redactions_applied;
                if let Some(s) = &mut cursor.settled {
                    xor_into(s, delta);
                }
            }
            Err(CoreError::Storage(e)) => return Err(Error::Core(CoreError::Storage(e))),
            Err(_) => report.rejected_events += 1,
        }
    }

    let ours = db.claims().fingerprint(&notify.log);
    if notify.fingerprint == ours || cursor.settled == Some(notify.fingerprint) {
        // Settled, by equality or by the cache. Either way the cursor may
        // fast-forward to the sender's count: equality means we hold their
        // exact set; a cache match means their set is one we reconciled to
        // and then tracked claim-by-claim through pushes — every claim it
        // counts, we hold (and every body we'd want; a reconcile that saw
        // garbage never caches). `max` guards against frames arriving out
        // of order on a sloppy channel.
        cursor.pull = cursor.pull.max(notify.count);
        cursor.settled = Some(notify.fingerprint);
        report.settled = true;
    }
    // Mismatch: save the row anyway (an instance reset must stick), but
    // never touch the cursor counts — holding more than the cursor says
    // is always allowed; claiming more is never.
    state.set_cursor(peer, &notify.log, cursor)?;

    report.missing_blobs = db.missing_blobs();
    Ok(report)
}

/// Best-effort model of what ingesting `event` does to the SENDER's
/// fingerprint for `log`, computed from the event alone plus our own state
/// as a proxy for theirs. Exact for the common cases (a fresh content
/// claim; a first redaction of a held body); when an assumption is off —
/// the sender already held the claim, a redaction tiebreak went the other
/// way — the settled cache simply stops matching and the next settle runs
/// a session. A wrong guess costs a round trip, never correctness.
fn sender_delta(db: &Database, log: &LogId, event: &SignedEvent) -> [u8; 32] {
    let id = event.id();
    let body: Option<Value> = event
        .body_bytes
        .as_ref()
        .and_then(|b| cbor::from_bytes(b).ok());
    let mut delta = fingerprint_claim(&id, body.is_some());
    if let Some(target) = redact_target(*log, body.as_ref()) {
        // The redaction entry lands on the redactor's log...
        xor_into(&mut delta, fingerprint_redaction(&target, &id));
        // ...and the target's body bit flips, if the sender held the body.
        // We can't see their shelves; assume they mirror ours (pre-ingest).
        if db.claims().contains(&target) {
            xor_into(&mut delta, fingerprint_claim(&target, true));
            xor_into(&mut delta, fingerprint_claim(&target, false));
        }
    }
    delta
}

fn xor_into(acc: &mut [u8; 32], digest: [u8; 32]) {
    for (a, b) in acc.iter_mut().zip(&digest) {
        *a ^= b;
    }
}
