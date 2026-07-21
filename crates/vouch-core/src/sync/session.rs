//! One sync session: a sans-io state machine over a [`Database`].
//!
//! The driver loop is the entire I/O contract:
//!
//! ```ignore
//! let mut session = SyncSession::new("relay", now, pull, push);
//! while let Some(request) = session.next_request(&mut db) {
//!     let response = transport.exchange(request)?; // the only I/O
//!     session.feed(&mut db, &mut cursors, response)?;
//! }
//! let report = session.finish();
//! ```
//!
//! `next_request()` is an idempotent peek (same request until fed — a transport
//! may retry it freely); `feed()` does all the mutation. Abandoning a
//! session at any message boundary is always safe: cursors advance only
//! after the data they describe has ingested, peers hold no conversation
//! state, and ingest is idempotent — so the recovery story for every
//! crash, timeout, and disconnect is the same: start a new session.
//!
//! ## The shape of one session
//!
//! Per log, in order:
//!
//! 1. **Status** — count, fingerprint, instance. An unfamiliar instance
//!    resets our cursors (the peer's arrival order was reborn).
//! 2. **Pull** — `Since` batches from our pull cursor until we've seen
//!    `count`.
//! 3. **Push** — `Publish` from our push cursor. Claims only: sessions
//!    never carry blob bytes. Media is pull-only (`GetBlob`, issued by
//!    whoever wants it, whenever it wants it — see the peer actor's
//!    media policy), so there is no transfer to negotiate and no
//!    ordering to get wrong.
//! 4. **Settle** — `Status` again. Fingerprint match: done, remember it.
//!    Mismatch we've reconciled before (`settled`): done — that's the
//!    known benign difference, e.g. claims a relay won't take from
//!    non-owners. Anything else → reconcile: exchange `Hashes`, fetch
//!    what they have that we lack (including bodies for claims we hold
//!    stripped), publish what we hold that they lack, and record where
//!    we landed.
//!
//!
//! [`Database`]: vouch_core::Database

use std::collections::{BTreeMap, BTreeSet};

use crate::{ClaimHash, Database, Error as CoreError, LogId, Event};

use super::error::Error;
use super::protocol::{BATCH, InstanceId, Request, Response};
use super::state::{PeerCursor, SyncState};

/// What one session accomplished. Counts of *progress*, plus counts of
/// *peer misbehavior* (rejected/off-plan artifacts) — non-zero misbehavior
/// never aborts a session, but a caller may well stop talking to that
/// pipe.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncReport {
    /// Claims newly stored from this peer (including embedded ones).
    pub pulled: usize,
    /// Bodies attached to claims we previously held stripped.
    pub healed: usize,
    /// Events the peer reported as new when we published.
    pub pushed: u64,
    /// Logs that needed full set reconciliation (the slow path).
    pub reconciled: usize,
    /// Logs whose cursors were reset because the peer's instance changed.
    pub cursor_resets: usize,
    /// Peer-served events that failed verification (dropped).
    pub rejected_events: usize,
    /// Peer-served events outside what we asked for (dropped — a pipe
    /// cannot smuggle logs you didn't subscribe to).
    pub off_plan: usize,
    /// Our published events the peer claims failed verification.
    pub push_rejected: u64,
}

struct LogPlan {
    log: LogId,
    pull: bool,
    push: bool,
}

/// Where the push pipeline returns to when it drains.
#[derive(Clone, Copy, PartialEq)]
enum AfterPush {
    /// The cursor-driven push inside the normal flow → settle next.
    Settle,
    /// The reconciliation publish → record where we landed and move on.
    Resettle,
}

/// Each phase has exactly one outstanding request shape; `next_request()` builds
/// it, `feed()` consumes the answer and picks the next phase.
enum Phase {
    /// Not yet opened: the first `next_request` call enters the first log
    /// (construction takes no `Database`, so the work that reads one is
    /// deferred to the first call that has one).
    Start,
    /// First contact for the current log.
    Status,
    /// `Since` batches against our pull cursor.
    Pull,
    /// `Publish` the next chunk of `push_batch`.
    Publish,
    /// Post-catch-up `Status`: the fingerprint check.
    Settle,
    /// Reconciliation step one: `Hashes`.
    Hashes,
    /// Reconciliation fetch: `Claims` chunks of `want_ids`.
    Claims,
    /// Final `Status` after reconciling: record the landing point.
    Resettle,
    Done,
}

/// A pure state bundle: it borrows nothing, so it can live inside whatever
/// owns the [`Database`] (the peer actor stores one per pipe). Every call
/// is handed the database (and cursor store) it should act on — pass the
/// same ones for the session's whole life.
pub struct SyncSession {
    peer: String,
    now: i64,
    plans: Vec<LogPlan>,
    idx: usize,
    phase: Phase,
    report: SyncReport,

    // Per-log scratch, reset by enter_log().
    cursor: PeerCursor,
    peer_count: u64,
    /// Extra pull rounds granted at settle time (the peer kept receiving
    /// while we synced). Bounded so a firehose can't hold a session open
    /// forever — the next session picks up from the cursor.
    settle_rounds: u32,
    /// No rejected artifacts seen for this log — only then may a
    /// reconciliation outcome be cached as `settled` (caching over
    /// garbage would mask real missing data as "known benign").
    clean: bool,
    push_batch: Vec<Event>,
    push_sent: usize,
    /// Whether this push batch is the cursor-ordered `serve_since` prefix
    /// (only then may acks advance the push cursor — reconciliation
    /// publishes are arbitrary ids, not arrival positions).
    push_is_cursor_prefix: bool,
    after_push: AfterPush,
    want_ids: Vec<ClaimHash>,
    want_sent: usize,
    extra_batch: Vec<Event>,
}

impl SyncSession {
    /// A session against one peer: `pull` is the subscription list, `push`
    /// the logs we offer (usually our own; between friends, any log —
    /// gossip is legitimate, and a relay that disagrees enforces it with
    /// auth, not protocol). `now` is the caller's clock for `received_at`.
    pub fn new(
        peer: impl Into<String>,
        now: i64,
        pull: Vec<LogId>,
        push: Vec<LogId>,
    ) -> SyncSession {
        let mut plans: Vec<LogPlan> = Vec::new();
        for log in pull {
            if !plans.iter().any(|p| p.log == log) {
                plans.push(LogPlan {
                    log,
                    pull: true,
                    push: false,
                });
            }
        }
        for log in push {
            match plans.iter_mut().find(|p| p.log == log) {
                Some(p) => p.push = true,
                None => plans.push(LogPlan {
                    log,
                    pull: false,
                    push: true,
                }),
            }
        }
        SyncSession {
            peer: peer.into(),
            now,
            plans,
            idx: 0,
            phase: Phase::Start,
            report: SyncReport::default(),
            cursor: PeerCursor::default(),
            peer_count: 0,
            settle_rounds: 0,
            clean: true,
            push_batch: Vec::new(),
            push_sent: 0,
            push_is_cursor_prefix: false,
            after_push: AfterPush::Settle,
            want_ids: Vec::new(),
            want_sent: 0,
            extra_batch: Vec::new(),
        }
    }

    fn plan(&self) -> &LogPlan {
        &self.plans[self.idx]
    }

    fn log(&self) -> LogId {
        self.plans[self.idx].log
    }

    fn save_cursor(&mut self, state: &mut dyn SyncState) -> Result<(), Error> {
        let log = self.log();
        state.set_cursor(&self.peer, &log, self.cursor)
    }

    /// Reset scratch and open the log at `self.idx`, or move to the blob
    /// phase when the plan is exhausted.
    fn enter_log(&mut self, _db: &Database) {
        if self.idx >= self.plans.len() {
            self.phase = Phase::Done;
            return;
        }
        self.cursor = PeerCursor::default();
        self.peer_count = 0;
        self.settle_rounds = 0;
        self.clean = true;
        self.push_batch.clear();
        self.push_sent = 0;
        self.push_is_cursor_prefix = false;
        self.want_ids.clear();
        self.want_sent = 0;
        self.extra_batch.clear();
        self.phase = Phase::Status;
    }

    fn finish_log(&mut self, db: &Database) {
        self.idx += 1;
        self.enter_log(db);
    }

    /// Stage a batch for the push pipeline, then onward to `after`.
    fn begin_push(&mut self, batch: Vec<Event>, cursor_prefix: bool, after: AfterPush) {
        self.push_batch = batch;
        self.push_sent = 0;
        self.push_is_cursor_prefix = cursor_prefix;
        self.after_push = after;
        if !self.push_batch.is_empty() {
            self.phase = Phase::Publish;
        } else {
            self.after_push();
        }
    }

    /// The cursor-driven push of the normal flow (step 3).
    fn begin_primary_push(&mut self, db: &Database) {
        let batch = if self.plan().push {
            db.claims().serve_since(&self.log(), self.cursor.push)
        } else {
            Vec::new()
        };
        self.begin_push(batch, true, AfterPush::Settle);
    }

    fn after_push(&mut self) {
        self.phase = match self.after_push {
            AfterPush::Settle => Phase::Settle,
            AfterPush::Resettle => Phase::Resettle,
        };
    }

    /// The settle decision, from whichever `Status` answer is freshest:
    /// match → remember and move on; the difference we already reconciled
    /// to → move on; anything else → reconcile.
    fn settle_with(
        &mut self,
        db: &Database,
        state: &mut dyn SyncState,
        count: u64,
        fingerprint: [u8; 32],
        instance: InstanceId,
    ) -> Result<(), Error> {
        if self.cursor.instance != Some(instance) {
            // The peer's arrival order was reborn mid-session. Reset and
            // take this log from the top (bounded by settle_rounds).
            self.report.cursor_resets += 1;
            self.cursor = PeerCursor {
                instance: Some(instance),
                ..PeerCursor::default()
            };
            self.save_cursor(state)?;
            self.peer_count = count;
            self.settle_rounds += 1;
            if self.settle_rounds > 2 {
                self.finish_log(db);
            } else if self.plan().pull && count > 0 {
                self.phase = Phase::Pull;
            } else {
                self.begin_primary_push(db);
            }
            return Ok(());
        }
        if self.plan().pull && count > self.cursor.pull && self.settle_rounds < 2 {
            // More arrived while we worked; one more lap. The cap keeps a
            // firehose from pinning a session open — the next session's
            // cursor picks up wherever this one left off.
            self.settle_rounds += 1;
            self.peer_count = count;
            self.phase = Phase::Pull;
            return Ok(());
        }
        let ours = db.claims().fingerprint(&self.log());
        if fingerprint == ours {
            self.cursor.settled = Some(fingerprint);
            self.save_cursor(state)?;
            self.finish_log(db);
        } else if self.cursor.settled == Some(fingerprint) {
            // The known benign difference: we reconciled to exactly this
            // peer state before. Nothing new to learn.
            self.finish_log(db);
        } else {
            self.report.reconciled += 1;
            self.phase = Phase::Hashes;
        }
        Ok(())
    }

    /// Ingest one peer-served event. Verification failures are the peer's
    /// misbehavior (counted, skipped); storage failures are ours (fatal).
    fn ingest(&mut self, db: &mut Database, event: Event) -> Result<(), Error> {
        match db.ingest_at(event, self.now) {
            Ok(report) => {
                self.report.pulled += report.newly_stored.is_some() as usize;
                self.report.healed += report.bodies_attached;
                Ok(())
            }
            Err(CoreError::Storage(e)) => {
                self.phase = Phase::Done;
                Err(Error::Core(CoreError::Storage(e)))
            }
            Err(_) => {
                self.report.rejected_events += 1;
                self.clean = false;
                Ok(())
            }
        }
    }

    /// The outstanding request, or `None` when the session is complete.
    /// Idempotent until the matching `feed()`: a driver may rebuild and
    /// retry the same request after a transport failure.
    pub fn next_request(&mut self, db: &Database) -> Option<Request> {
        if matches!(self.phase, Phase::Start) {
            self.enter_log(db);
        }
        Some(match self.phase {
            Phase::Start => unreachable!("Start is resolved above"),
            Phase::Status | Phase::Settle | Phase::Resettle => Request::Status { log: self.log() },
            Phase::Pull => Request::Since {
                log: self.log(),
                have: self.cursor.pull,
                max: BATCH,
            },
            Phase::Publish => {
                let end = (self.push_sent + BATCH as usize).min(self.push_batch.len());
                Request::Publish {
                    events: self.push_batch[self.push_sent..end].to_vec(),
                }
            }
            Phase::Hashes => Request::Hashes { log: self.log() },
            Phase::Claims => {
                let end = (self.want_sent + BATCH as usize).min(self.want_ids.len());
                Request::Claims {
                    ids: self.want_ids[self.want_sent..end].to_vec(),
                }
            }
            Phase::Done => return None,
        })
    }

    /// Consume the answer to the request `next_request()` last returned.
    pub fn feed(
        &mut self,
        db: &mut Database,
        state: &mut dyn SyncState,
        response: Response,
    ) -> Result<(), Error> {
        match self.phase {
            Phase::Start => Err(Error::Protocol(
                "response fed to a session with no outstanding request".into(),
            )),
            Phase::Status => {
                let (count, fingerprint, instance) = expect_status(response)?;
                self.cursor = state.cursor(&self.peer, &self.log())?;
                if self.cursor.instance != Some(instance) {
                    if self.cursor.instance.is_some() {
                        self.report.cursor_resets += 1;
                    }
                    self.cursor = PeerCursor {
                        instance: Some(instance),
                        ..PeerCursor::default()
                    };
                    self.save_cursor(state)?;
                }
                self.peer_count = count;
                if self.plan().pull && self.cursor.pull < count {
                    self.phase = Phase::Pull;
                    return Ok(());
                }
                self.begin_primary_push(db);
                if matches!(self.phase, Phase::Settle) {
                    // Nothing to pull, nothing to push: this Status answer
                    // already IS the settle — don't ask the same question
                    // twice. An idle sync is one message per log.
                    return self.settle_with(db, state, count, fingerprint, instance);
                }
                Ok(())
            }
            Phase::Pull => {
                let events = expect_events(response)?;
                if events.is_empty() {
                    // The peer claimed more than it serves. Don't spin —
                    // move on; the settle fingerprint decides if anything
                    // real is missing.
                    self.begin_primary_push(db);
                    return Ok(());
                }
                let received = events.len() as u64;
                let log = self.log();
                for event in events {
                    // A pipe answers for the log we asked about; valid
                    // events from other logs are still smuggling
                    // (subscription is the reader's choice).
                    if event.header().ok().map(|h| h.log_id) != Some(log) {
                        self.report.off_plan += 1;
                        self.clean = false;
                        continue;
                    }
                    self.ingest(db, event)?;
                }
                // The peer's arrival positions were consumed whether or not
                // each artifact was worth keeping.
                self.cursor.pull += received;
                self.save_cursor(state)?;
                if self.cursor.pull >= self.peer_count {
                    self.begin_primary_push(db);
                }
                Ok(())
            }
            Phase::Publish => {
                let (stored, rejected) = expect_ack(response)?;
                self.report.pushed += stored;
                self.report.push_rejected += rejected;
                if rejected > 0 {
                    self.clean = false;
                }
                let chunk = (BATCH as usize).min(self.push_batch.len() - self.push_sent);
                self.push_sent += chunk;
                if self.push_is_cursor_prefix {
                    self.cursor.push += chunk as u64;
                    self.save_cursor(state)?;
                }
                if self.push_sent >= self.push_batch.len() {
                    self.after_push();
                }
                Ok(())
            }
            Phase::Settle => {
                let (count, fingerprint, instance) = expect_status(response)?;
                self.settle_with(db, state, count, fingerprint, instance)
            }
            Phase::Hashes => {
                let theirs = expect_hashes(response)?;
                let log = self.log();
                let ours: BTreeMap<ClaimHash, bool> =
                    db.claims().log_hashes(&log).into_iter().collect();
                let theirs: BTreeMap<ClaimHash, bool> = theirs.into_iter().collect();
                self.want_ids.clear();
                if self.plan().pull {
                    for (id, has_body) in &theirs {
                        let want = match ours.get(id) {
                            // Unknown to us: want it (content or tombstone).
                            None => true,
                            // We hold it stripped, they hold the body, and
                            // no redaction forbids it: want the heal.
                            Some(false) => *has_body && db.claims().redaction(id).is_none(),
                            Some(true) => false,
                        };
                        if want {
                            self.want_ids.push(*id);
                        }
                    }
                }
                self.want_sent = 0;
                self.extra_batch.clear();
                if self.plan().push {
                    for (id, has_body) in &ours {
                        let offer = match theirs.get(id) {
                            // Unknown to them: offer it.
                            None => true,
                            // They hold it stripped and we have the body:
                            // offer the heal. (If they redacted it, their
                            // ingest drops the body — their state's
                            // business, and correct.)
                            Some(false) => *has_body,
                            Some(true) => false,
                        };
                        if offer && let Some(claim) = db.claims().get(id) {
                            self.extra_batch.push(claim.event);
                        }
                    }
                }
                if !self.want_ids.is_empty() {
                    self.phase = Phase::Claims;
                } else if !self.extra_batch.is_empty() {
                    let batch = std::mem::take(&mut self.extra_batch);
                    self.begin_push(batch, false, AfterPush::Resettle);
                } else {
                    self.phase = Phase::Resettle;
                }
                Ok(())
            }
            Phase::Claims => {
                let events = expect_events(response)?;
                let end = (self.want_sent + BATCH as usize).min(self.want_ids.len());
                let requested: BTreeSet<ClaimHash> =
                    self.want_ids[self.want_sent..end].iter().copied().collect();
                for event in events {
                    if !requested.contains(&event.id()) {
                        self.report.off_plan += 1;
                        self.clean = false;
                        continue;
                    }
                    self.ingest(db, event)?;
                }
                self.want_sent = end;
                if self.want_sent < self.want_ids.len() {
                    // Stay in Claims for the next chunk.
                } else if !self.extra_batch.is_empty() {
                    let batch = std::mem::take(&mut self.extra_batch);
                    self.begin_push(batch, false, AfterPush::Resettle);
                } else {
                    self.phase = Phase::Resettle;
                }
                Ok(())
            }
            Phase::Resettle => {
                let (_count, fingerprint, instance) = expect_status(response)?;
                // We've fetched everything they had and pushed everything
                // we may: whatever difference remains is the benign kind.
                // Remember the peer's fingerprint so the next session's
                // settle doesn't re-reconcile a difference we already
                // understand — unless this log saw garbage, in which case
                // assume nothing.
                if self.clean && self.cursor.instance == Some(instance) {
                    self.cursor.settled = Some(fingerprint);
                    self.save_cursor(state)?;
                }
                self.finish_log(db);
                Ok(())
            }
            Phase::Done => Err(Error::Protocol("response fed to a finished session".into())),
        }
    }

    /// The running tally (final once `next_request()` returns `None`).
    pub fn report(&self) -> &SyncReport {
        &self.report
    }

    pub fn finish(self) -> SyncReport {
        self.report
    }
}

fn expect_status(r: Response) -> Result<(u64, [u8; 32], InstanceId), Error> {
    match r {
        Response::Status {
            count,
            fingerprint,
            instance,
        } => Ok((count, fingerprint, instance)),
        other => Err(unexpected("Status", &other)),
    }
}

fn expect_events(r: Response) -> Result<Vec<Event>, Error> {
    match r {
        Response::Events { events } => Ok(events),
        other => Err(unexpected("Events", &other)),
    }
}

fn expect_hashes(r: Response) -> Result<Vec<(ClaimHash, bool)>, Error> {
    match r {
        Response::Hashes { entries } => Ok(entries),
        other => Err(unexpected("Hashes", &other)),
    }
}

fn expect_ack(r: Response) -> Result<(u64, u64), Error> {
    match r {
        Response::Ack { stored, rejected } => Ok((stored, rejected)),
        other => Err(unexpected("Ack", &other)),
    }
}

fn unexpected(wanted: &str, got: &Response) -> Error {
    let got = match got {
        Response::Status { .. } => "Status",
        Response::Events { .. } => "Events",
        Response::Hashes { .. } => "Hashes",
        Response::Ack { .. } => "Ack",
        Response::Blob { .. } => "Blob",
    };
    Error::Protocol(format!("expected {wanted} response, got {got}"))
}
