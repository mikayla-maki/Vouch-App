//! The peer: the composition with a name on the network.
//!
//! A [`Peer`] is one database, one identity context, and all the protocol
//! machinery, behind five verbs. The consumer's model is exactly the user
//! model — *write claims, get claims, and only your words leave the
//! house*:
//!
//! ```ignore
//! let (peer, actor) = Peer::new(db, cursors, instance, Some(writer), ServePolicy::Owned, clock);
//! executor.spawn(actor.run());                  // touched once, forgotten
//!
//! peer.claim(Draft::new("rec").text("subject", "Joe's Pizza")).await?;
//! let pipe = peer.connect("mom-relay", end).await?;   // host plumbing: a duct to a remote
//! peer.follow(mom_log, pipe).await?;                  // private consumption, from a chosen source
//! peer.firehose().await?;                             // local-only: everything, for the UI
//! peer.authored().await?;                             // my own claims (in-process tap)
//! ```
//!
//! ## The actor
//!
//! All state lives in one task ([`PeerActor::run`]) that selects over a
//! command channel and an inbox of pipes — no locks, no `Arc<Mutex>`,
//! nothing held across an await. Pipes carry *typed protocol messages*
//! ([`PipeMsg`]); serialization and sockets live out in transport tasks,
//! so the actor itself never touches a byte of I/O and a test pipe is
//! just a pair of channels. The sans-io session machinery slots straight
//! in: a session never blocks, so sessions against different pipes
//! interleave freely, one `feed` per inbox event.
//!
//! Inside, three small tables and their joins ARE the behavior:
//!
//! - `follows: LogId → {PipeId}` — what you consume, and from where
//!   (configured by [`Peer::follow`]; the only input relationship).
//! - the writer (at most one in this composition) — your voice.
//! - `watches: PipeId → {LogId}` — other peers' follows of you,
//!   accumulated from inbound [`PipeMsg::Watch`] announcements, ephemeral,
//!   gone when the pipe closes.
//!
//! Sessions = follows ⋈ pipes (pull what you want over the wires you
//! have, pushing your own log wherever you follow it — *publishing is
//! following your own log somewhere*). Fan-out = watches ⋈ pipes (new
//! claims become [`Notify`] frames for whoever announced interest). A
//! relay is this same actor with no writer, no follows, and
//! [`ServePolicy::Everything`].
//!
//! ## Consumption is private
//!
//! With [`ServePolicy::Owned`] (the app default), the peer answers
//! requests — and accepts watches — only for logs it *writes*. What you
//! follow, what you've merged, what you read: none of it is servable, so
//! none of it can leak by syncing with you. The only sanctioned path for
//! third-party content through you is the embed — quoting is speech with
//! your name on it. The firehose tap never leaves the process.
//!
//! ## What can go wrong, and why it's fine
//!
//! Everything here decides *when*, never *what* — convergence lives in
//! the layers below, which tolerate redelivery, loss, and reordering by
//! construction. So: frames dropped under backpressure degrade to the
//! next session; a congested pipe aborts its session and retries on the
//! next event; a dead pipe just disappears (cursors are durable, watch
//! state was always ephemeral; reconnect = re-announce). A scheduling bug
//! up here can cost latency; it cannot corrupt anything.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::pin::Pin;

use futures::channel::{mpsc, oneshot};
use futures::stream::{self, SelectAll, Stream, StreamExt};

use crate::claim::SignedEvent;
use crate::database::Database;
use crate::draft::Draft;
use crate::error::Error as CoreError;
use crate::keys::LogId;
use crate::sync::{
    self, Error, InstanceId, Notify, Request, Response, SyncSession, SyncState, apply_notify,
    notify_for,
};
use crate::value::BlobHash;
use crate::writer::Writer;

/// One live connection, as the actor names it. Connection-scoped: a
/// reconnect is a new pipe with a new id (cursors don't care — they're
/// keyed by the stable peer *name* given to [`Peer::connect`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PipeId(pub u64);

/// What travels on a pipe — typed protocol messages, both directions.
/// Transports serialize these however they like; the actor never sees
/// bytes.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
pub enum PipeMsg {
    /// A protocol request, correlation-tagged (both sides may have
    /// requests in flight on one duct).
    Request { id: u64, request: Request },
    /// The answer to the `Request` with the same id.
    Response { id: u64, response: Response },
    /// A push frame (see [`Notify`]): server-initiated, advisory,
    /// droppable.
    Frame(Notify),
    /// "Of the logs you serve, I want live frames for these." Replaces
    /// the sender's previous watch set on this pipe. Answered with one
    /// heartbeat frame per watched log, so the watcher can settle (or
    /// ring its own doorbell) immediately.
    Watch(Vec<LogId>),
}

/// One end of a duct. The actor holds one end; a transport task (or, in
/// tests, another actor) holds the other.
pub struct PipeEnd {
    pub tx: mpsc::Sender<PipeMsg>,
    pub rx: mpsc::Receiver<PipeMsg>,
}

/// An in-process duct: two ends, crossed channels. The entire test
/// transport, and the template for real ones (a socket transport is a
/// task that moves `PipeMsg`s between a `PipeEnd` and the wire).
pub fn pipe(capacity: usize) -> (PipeEnd, PipeEnd) {
    let (a_tx, b_rx) = mpsc::channel(capacity);
    let (b_tx, a_rx) = mpsc::channel(capacity);
    (
        PipeEnd { tx: a_tx, rx: a_rx },
        PipeEnd { tx: b_tx, rx: b_rx },
    )
}

/// Which logs this peer answers requests (and accepts watches) for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServePolicy {
    /// Serve only logs this peer writes — the app default. Consumption
    /// stays private: following, merging, and reading leave no servable
    /// trace.
    Owned,
    /// Serve every log held — the relay posture (a relay holds only what
    /// owners published to it, so serving everything *is* serving the
    /// authorized set). Auth and quota are the transport wrapper's job.
    Everything,
}

/// Per-pipe media posture. Claims always sync; media never does by
/// default — it moves only when something asks ([`Peer::fetch_blob`],
/// usually the UI wanting to render). `eager_media` opts a pipe into
/// fetching media for claims as they arrive (the p2p posture: when your
/// friend's phone is reachable, take the photos while you can). Relays
/// ([`ServePolicy::Everything`]) fetch eagerly regardless of config —
/// holding the media is their job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PipeConfig {
    pub eager_media: bool,
}

/// A change that landed in the database — the tap item. `events` carries
/// the artifacts when they're cheaply at hand (mints, push frames);
/// after a bulk session pull it may be empty, meaning "this log changed,
/// re-query what you care about".
#[derive(Debug, Clone)]
pub struct PeerEvent {
    pub log: LogId,
    pub events: Vec<SignedEvent>,
}

enum Command {
    Connect {
        name: String,
        end: PipeEnd,
        config: PipeConfig,
        reply: oneshot::Sender<PipeId>,
    },
    Disconnect {
        pipe: PipeId,
    },
    Follow {
        log: LogId,
        pipe: PipeId,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    Unfollow {
        log: LogId,
        reply: oneshot::Sender<()>,
    },
    Claim {
        draft: Draft,
        reply: oneshot::Sender<Result<SignedEvent, Error>>,
    },
    SyncNow {
        reply: oneshot::Sender<()>,
    },
    FetchBlob {
        hash: BlobHash,
        reply: oneshot::Sender<Result<(), Error>>,
    },
    EvictBlob {
        hash: BlobHash,
        reply: oneshot::Sender<Result<bool, Error>>,
    },
    Firehose {
        reply: oneshot::Sender<mpsc::Receiver<PeerEvent>>,
    },
    Authored {
        reply: oneshot::Sender<mpsc::Receiver<PeerEvent>>,
    },
    Query(Box<dyn FnOnce(&Database) + Send>),
}

/// The handle consumers hold: cheap to clone (a sender underneath), and
/// the whole API surface. Every method is one message to the actor.
#[derive(Clone)]
pub struct Peer {
    commands: mpsc::Sender<Command>,
    me: Option<LogId>,
}

fn gone<T>(_: T) -> Error {
    Error::State("peer actor stopped".into())
}

impl Peer {
    /// Compose a peer: one database, its cursor store, the incarnation of
    /// its arrival order, at most one pen, a serving posture, and a clock
    /// (the actor's only contact with time — inject the system clock from
    /// the host, a counter from a test).
    pub fn new(
        mut db: Database,
        state: Box<dyn SyncState>,
        instance: InstanceId,
        writer: Option<Writer>,
        serve: ServePolicy,
        clock: impl Fn() -> i64 + Send + 'static,
    ) -> (Peer, PeerActor) {
        let me = writer.map(|w| db.add_writer(w));
        let (tx, rx) = mpsc::channel(64);
        let peer = Peer { commands: tx, me };
        let actor = PeerActor {
            commands: rx,
            inbox: SelectAll::new(),
            core: Core {
                db,
                state,
                instance,
                me,
                serve,
                clock: Box::new(clock),
                pipes: HashMap::new(),
                follows: BTreeMap::new(),
                next_pipe_id: 0,
                firehose: Vec::new(),
                authored: Vec::new(),
            },
        };
        (peer, actor)
    }

    /// The log this peer writes, if it holds a pen.
    pub fn id(&self) -> Option<LogId> {
        self.me
    }

    async fn send(&self, command: Command) -> Result<(), Error> {
        let mut tx = self.commands.clone();
        futures::SinkExt::send(&mut tx, command).await.map_err(gone)
    }

    /// Hand the actor a duct to a remote peer. `name` is your stable,
    /// private label for who's on the other end (a relay URL, a friend's
    /// log id) — cursors are keyed by it, so reconnecting under the same
    /// name resumes where you left off. Media is lazy on this pipe; see
    /// [`connect_with`](Self::connect_with) for the eager knob.
    pub async fn connect(&self, name: impl Into<String>, end: PipeEnd) -> Result<PipeId, Error> {
        self.connect_with(name, end, PipeConfig::default()).await
    }

    /// [`connect`](Self::connect) with options (e.g. eager media for p2p
    /// pipes).
    pub async fn connect_with(
        &self,
        name: impl Into<String>,
        end: PipeEnd,
        config: PipeConfig,
    ) -> Result<PipeId, Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Connect {
            name: name.into(),
            end,
            config,
            reply,
        })
        .await?;
        rx.await.map_err(gone)
    }

    /// Demand a blob (the UI wants to render it): one `GetBlob` to a pipe
    /// that follows the log whose claim pins it. The result lands in the
    /// database (watch the firehose, or re-query); a miss leaves the want
    /// standing for the next demand. Errors only when no connected pipe
    /// follows any referrer's log.
    pub async fn fetch_blob(&self, hash: BlobHash) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::FetchBlob { hash, reply }).await?;
        rx.await.map_err(gone)?
    }

    /// Cache eviction under storage pressure: drop the bytes, keep every
    /// claim. Re-fetchable forever via [`fetch_blob`](Self::fetch_blob).
    pub async fn evict_blob(&self, hash: BlobHash) -> Result<bool, Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::EvictBlob { hash, reply }).await?;
        rx.await.map_err(gone)?
    }

    pub async fn disconnect(&self, pipe: PipeId) -> Result<(), Error> {
        self.send(Command::Disconnect { pipe }).await
    }

    /// Care about a log, via a source: catch up now, stay live through
    /// frames, heal forever after. Following your *own* log somewhere is
    /// how you publish there — the push direction emerges from holding
    /// the pen, not from a separate verb.
    pub async fn follow(&self, log: LogId, pipe: PipeId) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Follow { log, pipe, reply }).await?;
        rx.await.map_err(gone)?
    }

    pub async fn unfollow(&self, log: LogId) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Unfollow { log, reply }).await?;
        rx.await.map_err(gone)
    }

    /// Speak: mint the draft (attachments stored, body signed, ingested),
    /// fan it to watchers, queue it for every pipe where you follow your
    /// own log. Returns the signed artifact.
    pub async fn claim(&self, draft: Draft) -> Result<SignedEvent, Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Claim { draft, reply }).await?;
        rx.await.map_err(gone)?
    }

    /// Kick sessions against every pipe now (the app's timer calls this;
    /// frames make it rarely matter).
    pub async fn sync_now(&self) -> Result<(), Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::SyncNow { reply }).await?;
        rx.await.map_err(gone)
    }

    /// Everything that lands in the database, as it lands — the UI's
    /// invalidation stream. Local-only by construction: this tap is not
    /// part of any protocol surface.
    pub async fn firehose(&self) -> Result<mpsc::Receiver<PeerEvent>, Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Firehose { reply }).await?;
        rx.await.map_err(gone)
    }

    /// Claims from this peer's own writer only — the one stream with any
    /// business leaving the process (exporters, bridge transports).
    pub async fn authored(&self) -> Result<mpsc::Receiver<PeerEvent>, Error> {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Authored { reply }).await?;
        rx.await.map_err(gone)
    }

    /// Read the database inside the actor: snapshots for the UI, asserts
    /// for tests. The closure runs on the actor's turn — keep it quick.
    pub async fn query<R, F>(&self, f: F) -> Result<R, Error>
    where
        R: Send + 'static,
        F: FnOnce(&Database) -> R + Send + 'static,
    {
        let (reply, rx) = oneshot::channel();
        self.send(Command::Query(Box::new(move |db| {
            let _ = reply.send(f(db));
        })))
        .await?;
        rx.await.map_err(gone)
    }
}

enum PipeEvent {
    Msg(PipeMsg),
    Closed,
}

type PipeStream = Pin<Box<dyn Stream<Item = (PipeId, PipeEvent)> + Send>>;

struct Pipe {
    /// The stable peer name cursors are keyed by.
    name: String,
    tx: mpsc::Sender<PipeMsg>,
    config: PipeConfig,
    /// Out-of-session blob fetches in flight: request id → the hash the
    /// answer must verify against.
    pending_blobs: HashMap<u64, BlobHash>,
    /// The in-flight session and its pull set (kept for tap invalidation
    /// when it completes), if any. One session per pipe at a time.
    session: Option<(SyncSession, Vec<LogId>)>,
    /// Correlation id of our outstanding request.
    pending: Option<u64>,
    next_request_id: u64,
    /// Something changed; run a(nother) session when free.
    dirty: bool,
    /// What the other end watches of what we serve.
    watches: BTreeSet<LogId>,
}

/// The spawned half: owns the database and runs the loop. Construct with
/// [`Peer::new`], hand `run()` to any executor — it is `Send`, carries no
/// runtime of its own, and reads no clock but the injected one.
pub struct PeerActor {
    commands: mpsc::Receiver<Command>,
    inbox: SelectAll<PipeStream>,
    core: Core,
}

struct Core {
    db: Database,
    state: Box<dyn SyncState>,
    instance: InstanceId,
    me: Option<LogId>,
    serve: ServePolicy,
    clock: Box<dyn Fn() -> i64 + Send>,
    pipes: HashMap<PipeId, Pipe>,
    follows: BTreeMap<LogId, BTreeSet<PipeId>>,
    next_pipe_id: u64,
    firehose: Vec<mpsc::Sender<PeerEvent>>,
    authored: Vec<mpsc::Sender<PeerEvent>>,
}

enum Turn {
    Command(Option<Command>),
    Pipe(Option<(PipeId, PipeEvent)>),
}

impl PeerActor {
    /// The loop. Runs until every [`Peer`] handle is dropped.
    pub async fn run(mut self) {
        loop {
            let turn = futures::select! {
                command = self.commands.next() => Turn::Command(command),
                event = self.inbox.next() => Turn::Pipe(event),
            };
            match turn {
                Turn::Command(Some(command)) => self.core.command(command, &mut self.inbox),
                Turn::Command(None) => break,
                Turn::Pipe(Some((id, event))) => self.core.pipe_event(id, event),
                Turn::Pipe(None) => {}
            }
            self.core.pump();
        }
    }
}

impl Core {
    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn serves(&self, log: &LogId) -> bool {
        match self.serve {
            ServePolicy::Owned => Some(*log) == self.me,
            ServePolicy::Everything => true,
        }
    }

    /// Logs followed via this pipe — a session's pull set.
    fn followed_on(&self, pipe: PipeId) -> Vec<LogId> {
        self.follows
            .iter()
            .filter(|(_, pipes)| pipes.contains(&pipe))
            .map(|(log, _)| *log)
            .collect()
    }

    // ── Commands ──────────────────────────────────────────────────────

    fn command(&mut self, command: Command, inbox: &mut SelectAll<PipeStream>) {
        match command {
            Command::Connect {
                name,
                end,
                config,
                reply,
            } => {
                let id = PipeId(self.next_pipe_id);
                self.next_pipe_id += 1;
                self.pipes.insert(
                    id,
                    Pipe {
                        name,
                        tx: end.tx,
                        config,
                        pending_blobs: HashMap::new(),
                        session: None,
                        pending: None,
                        next_request_id: 0,
                        dirty: false,
                        watches: BTreeSet::new(),
                    },
                );
                inbox.push(Box::pin(
                    end.rx
                        .map(move |msg| (id, PipeEvent::Msg(msg)))
                        .chain(stream::once(async move { (id, PipeEvent::Closed) })),
                ));
                let _ = reply.send(id);
            }
            Command::Disconnect { pipe } => self.drop_pipe(pipe),
            Command::Follow { log, pipe, reply } => {
                if !self.pipes.contains_key(&pipe) {
                    let _ = reply.send(Err(Error::Protocol("no such pipe".into())));
                    return;
                }
                self.follows.entry(log).or_default().insert(pipe);
                self.announce_watch(pipe);
                if let Some(p) = self.pipes.get_mut(&pipe) {
                    p.dirty = true;
                }
                let _ = reply.send(Ok(()));
            }
            Command::Unfollow { log, reply } => {
                if let Some(pipes) = self.follows.remove(&log) {
                    for pipe in pipes {
                        self.announce_watch(pipe);
                    }
                }
                let _ = reply.send(());
            }
            Command::Claim { draft, reply } => {
                let _ = reply.send(self.mint(draft));
            }
            Command::SyncNow { reply } => {
                for pipe in self.pipes.values_mut() {
                    pipe.dirty = true;
                }
                let _ = reply.send(());
            }
            Command::Firehose { reply } => {
                let (tx, rx) = mpsc::channel(256);
                self.firehose.push(tx);
                let _ = reply.send(rx);
            }
            Command::Authored { reply } => {
                let (tx, rx) = mpsc::channel(256);
                self.authored.push(tx);
                let _ = reply.send(rx);
            }
            Command::FetchBlob { hash, reply } => {
                let _ = reply.send(self.demand_blob(hash));
            }
            Command::EvictBlob { hash, reply } => {
                let _ = reply.send(self.db.evict_blob(&hash).map_err(Error::Core));
            }
            Command::Query(f) => f(&self.db),
        }
    }

    fn mint(&mut self, draft: Draft) -> Result<SignedEvent, Error> {
        let me = self
            .me
            .ok_or_else(|| Error::State("read-only peer: no writer configured".into()))?;
        let supplies: Vec<Vec<u8>> = draft.attachments.iter().map(|a| a.bytes.clone()).collect();
        let event = self.db.compose(&me, draft).map_err(Error::Core)?;
        self.landed(me, vec![event.clone()], None);
        // The PutBlob fast-track, same idea as the Notify frame: anyone
        // watching our log just received the claim (FIFO duct), so they
        // want its media RIGHT NOW — answer the GetBlob they were about
        // to send. Advisory and droppable; the pull path is the truth.
        if !supplies.is_empty() {
            let watching: Vec<PipeId> = self
                .pipes
                .iter()
                .filter(|(_, p)| p.watches.contains(&me))
                .map(|(id, _)| *id)
                .collect();
            for id in watching {
                if let Some(p) = self.pipes.get_mut(&id) {
                    for bytes in &supplies {
                        let _ = p.tx.try_send(PipeMsg::Request {
                            id: {
                                let rid = p.next_request_id;
                                p.next_request_id += 1;
                                rid
                            },
                            request: Request::PutBlob {
                                bytes: bytes.clone(),
                            },
                        });
                    }
                }
            }
        }
        // Frames cover the fast path; the cursor-driven push half of the
        // next session is the reliable one. Every pipe where we follow our
        // own log is now behind.
        let publish_pipes = self.follows.get(&me).cloned().unwrap_or_default();
        for pipe in publish_pipes {
            if let Some(p) = self.pipes.get_mut(&pipe) {
                p.dirty = true;
            }
        }
        Ok(event)
    }

    /// New artifacts landed for `log`: tell the taps, frame the watchers
    /// (except whoever brought them — they obviously have them).
    fn landed(&mut self, log: LogId, events: Vec<SignedEvent>, source: Option<PipeId>) {
        let item = PeerEvent {
            log,
            events: events.clone(),
        };
        emit(&mut self.firehose, &item);
        if Some(log) == self.me {
            emit(&mut self.authored, &item);
        }
        let watchers: Vec<PipeId> = self
            .pipes
            .iter()
            .filter(|(id, p)| Some(**id) != source && p.watches.contains(&log))
            .map(|(id, _)| *id)
            .collect();
        if watchers.is_empty() {
            return;
        }
        let frame = notify_for(&self.db, self.instance, &log, events);
        for id in watchers {
            if let Some(p) = self.pipes.get_mut(&id) {
                // Frames are advisory: a full pipe just misses one; the
                // watcher's next session (or heartbeat) catches it up.
                let _ = p.tx.try_send(PipeMsg::Frame(frame.clone()));
            }
        }
    }

    // ── Pipe events ───────────────────────────────────────────────────

    fn pipe_event(&mut self, id: PipeId, event: PipeEvent) {
        match event {
            PipeEvent::Closed => self.drop_pipe(id),
            PipeEvent::Msg(PipeMsg::Request { id: rid, request }) => {
                let (response, ingested) = self.respond_scoped(request);
                if let Some(p) = self.pipes.get_mut(&id) {
                    let _ = p.tx.try_send(PipeMsg::Response { id: rid, response });
                }
                let logs: Vec<LogId> = ingested.iter().map(|(log, _)| *log).collect();
                for (log, events) in ingested {
                    self.landed(log, events, Some(id));
                }
                // A relay (or eager pipe) keeps itself stocked: claims
                // just landed, pull their media from whoever brought them
                // — including bytes an earlier cull dropped.
                self.eager_fetch(id, &logs);
            }
            PipeEvent::Msg(PipeMsg::Response { id: rid, response }) => {
                let blob_fetch = self
                    .pipes
                    .get_mut(&id)
                    .and_then(|p| p.pending_blobs.remove(&rid));
                match blob_fetch {
                    Some(expected) => self.blob_response(id, expected, response),
                    None => self.session_response(id, rid, response),
                }
            }
            PipeEvent::Msg(PipeMsg::Frame(frame)) => self.apply_frame(id, frame),
            PipeEvent::Msg(PipeMsg::Watch(logs)) => {
                let watched: Vec<LogId> = logs.into_iter().filter(|l| self.serves(l)).collect();
                if let Some(p) = self.pipes.get_mut(&id) {
                    p.watches = watched.iter().copied().collect();
                }
                // Answer with one heartbeat per watched log: the watcher
                // settles instantly or learns to ring its doorbell.
                for log in watched {
                    let beat = notify_for(&self.db, self.instance, &log, Vec::new());
                    if let Some(p) = self.pipes.get_mut(&id) {
                        let _ = p.tx.try_send(PipeMsg::Frame(beat));
                    }
                }
            }
        }
    }

    fn drop_pipe(&mut self, id: PipeId) {
        self.pipes.remove(&id);
        self.follows.retain(|_, pipes| {
            pipes.remove(&id);
            !pipes.is_empty()
        });
        // The inbox stream ends on its own (its sender is gone); cursors
        // are durable under the peer NAME, so a reconnect resumes.
    }

    // ── Media (pull-only, demand-driven) ──────────────────────────────

    /// UI demand: route one `GetBlob` to a pipe that follows a log whose
    /// claim pins this hash.
    fn demand_blob(&mut self, hash: BlobHash) -> Result<(), Error> {
        let referrer_logs: BTreeSet<LogId> = self
            .db
            .claims()
            .blob_referrers(&hash)
            .iter()
            .filter_map(|cid| self.db.claims().get(cid))
            .map(|c| c.header.log_id)
            .collect();
        let pipe = referrer_logs
            .iter()
            .filter_map(|log| self.follows.get(log))
            .flatten()
            .next()
            .copied()
            .ok_or_else(|| Error::State("no connected pipe follows this blob's log".into()))?;
        self.request_blob(pipe, hash);
        Ok(())
    }

    /// Eager pipes (and relays, always) pull media for claims as they
    /// land: every blob the given logs' live bodies pin and we lack.
    fn eager_fetch(&mut self, pipe: PipeId, logs: &[LogId]) {
        let eager = matches!(self.serve, ServePolicy::Everything)
            || self.pipes.get(&pipe).is_some_and(|p| p.config.eager_media);
        if !eager {
            return;
        }
        for log in logs {
            for hash in sync::respond::log_wants(&self.db, log) {
                self.request_blob(pipe, hash);
            }
        }
    }

    /// One out-of-session `GetBlob`, correlated so the answer can be
    /// verified against the hash we asked for. At most one in flight per
    /// hash per pipe.
    fn request_blob(&mut self, pipe: PipeId, hash: BlobHash) {
        let Some(p) = self.pipes.get_mut(&pipe) else {
            return;
        };
        if p.pending_blobs.values().any(|h| *h == hash) {
            return;
        }
        let rid = p.next_request_id;
        p.next_request_id += 1;
        if p.tx
            .try_send(PipeMsg::Request {
                id: rid,
                request: Request::GetBlob { hash },
            })
            .is_ok()
        {
            p.pending_blobs.insert(rid, hash);
        }
    }

    /// The answer to one of our blob fetches. A miss or a lie costs
    /// nothing: the want stands (verification is `ingest_blob`'s), and
    /// the next demand asks again.
    fn blob_response(&mut self, id: PipeId, expected: BlobHash, response: Response) {
        let Response::Blob { bytes: Some(bytes) } = response else {
            return;
        };
        if matches!(self.db.ingest_blob(expected, bytes), Ok(true)) {
            // Bytes landed: invalidate the taps and heartbeat watchers of
            // the referrer logs (a downstream fetcher may have missed
            // moments ago) — same move as an accepted PutBlob.
            let logs: BTreeSet<LogId> = self
                .db
                .claims()
                .blob_referrers(&expected)
                .iter()
                .filter_map(|cid| self.db.claims().get(cid))
                .map(|c| c.header.log_id)
                .collect();
            for log in logs {
                self.landed(log, Vec::new(), Some(id));
            }
        }
    }

    fn announce_watch(&mut self, pipe: PipeId) {
        let watched = self.followed_on(pipe);
        if let Some(p) = self.pipes.get_mut(&pipe) {
            let _ = p.tx.try_send(PipeMsg::Watch(watched));
        }
    }

    fn apply_frame(&mut self, id: PipeId, frame: Notify) {
        let Some(pipe) = self.pipes.get(&id) else {
            return;
        };
        let name = pipe.name.clone();
        let log = frame.log;
        let carried: Vec<SignedEvent> = frame
            .events
            .iter()
            .filter(|e| e.header().ok().map(|h| h.log_id) == Some(log))
            .cloned()
            .collect();
        let now = self.now();
        match apply_notify(&mut self.db, &mut *self.state, &name, now, frame) {
            Ok(report) => {
                if report.pulled > 0 || report.healed > 0 || report.redactions_applied > 0 {
                    self.landed(log, carried, Some(id));
                }
                if !report.settled
                    && let Some(p) = self.pipes.get_mut(&id)
                {
                    p.dirty = true;
                }
                self.eager_fetch(id, &[log]);
            }
            Err(_) => {
                // Local storage trouble: leave the pipe quiet rather than
                // hot-loop; the next external event retries.
            }
        }
    }

    // ── Serving (the consumption-is-private boundary) ─────────────────

    /// Answer a request within the serve scope. Returns the response plus
    /// whatever newly landed (so the caller can fan it out — that's the
    /// relay re-serving what an owner just published).
    fn respond_scoped(&mut self, request: Request) -> (Response, Vec<(LogId, Vec<SignedEvent>)>) {
        let now = self.now();
        match request {
            Request::Status { log } if !self.serves(&log) => (
                // Indistinguishable from "I have nothing": zero count and
                // the empty-set fingerprint are exactly what an empty log
                // answers, so scope leaks nothing.
                Response::Status {
                    count: 0,
                    fingerprint: [0u8; 32],
                    instance: self.instance,
                },
                Vec::new(),
            ),
            Request::Since { log, .. } if !self.serves(&log) => {
                (Response::Events { events: Vec::new() }, Vec::new())
            }
            Request::Hashes { log } if !self.serves(&log) => (
                Response::Hashes {
                    entries: Vec::new(),
                },
                Vec::new(),
            ),
            Request::Claims { ids } => {
                let events = ids
                    .iter()
                    .filter_map(|cid| self.db.claims().get(cid))
                    .filter(|c| self.serves(&c.header.log_id))
                    .map(|c| c.signed)
                    .collect();
                (Response::Events { events }, Vec::new())
            }
            Request::GetBlob { hash } => {
                let referenced_by_served = self
                    .db
                    .claims()
                    .blob_referrers(&hash)
                    .iter()
                    .filter_map(|cid| self.db.claims().get(cid))
                    .any(|c| self.serves(&c.header.log_id));
                let bytes = if referenced_by_served {
                    self.db.blobs().get(&hash)
                } else {
                    None
                };
                (Response::Blob { bytes }, Vec::new())
            }
            Request::PutBlob { bytes } => {
                let hash = BlobHash(*blake3::hash(&bytes).as_bytes());
                let wanted = self.db.missing_blobs().iter().any(|b| b.hash == hash);
                let stored = if wanted {
                    self.db.ingest_blob(hash, bytes).unwrap_or(false)
                } else {
                    false
                };
                // A landed blob is news to whoever watches its referrer
                // logs: their claims arrived bodied but media-less moments
                // ago, and their first fetch may have missed. An empty
                // heartbeat per referrer log (`landed` with no events)
                // makes them re-check their want-list — that's how media
                // chases its claim through a relay.
                let woken = if stored {
                    let mut logs: BTreeSet<LogId> = BTreeSet::new();
                    for cid in self.db.claims().blob_referrers(&hash) {
                        if let Some(claim) = self.db.claims().get(&cid) {
                            logs.insert(claim.header.log_id);
                        }
                    }
                    logs.into_iter().map(|log| (log, Vec::new())).collect()
                } else {
                    Vec::new()
                };
                (
                    Response::Ack {
                        stored: stored as u64,
                        rejected: 0,
                    },
                    woken,
                )
            }
            Request::Publish { events } => self.accept_publish(events, now),
            // In-scope reads: the stateless responder answers from the
            // database directly.
            other => (
                sync::respond(&mut self.db, self.instance, now, other)
                    .unwrap_or(Response::Events { events: Vec::new() }),
                Vec::new(),
            ),
        }
    }

    /// Accept pushed events for logs we host, follow, or own — decline
    /// the rest silently (not misbehavior, just not our business).
    fn accept_publish(
        &mut self,
        events: Vec<SignedEvent>,
        now: i64,
    ) -> (Response, Vec<(LogId, Vec<SignedEvent>)>) {
        let mut stored = 0u64;
        let mut rejected = 0u64;
        let mut landed: BTreeMap<LogId, Vec<SignedEvent>> = BTreeMap::new();
        for event in events {
            let Ok(header) = event.header() else {
                rejected += 1;
                continue;
            };
            let log = header.log_id;
            let accept =
                self.serves(&log) || self.follows.contains_key(&log) || Some(log) == self.me;
            if !accept {
                continue;
            }
            match self.db.ingest_at(event.clone(), now) {
                Ok(report) => {
                    let new = report.newly_stored.is_some() as usize + report.bodies_attached;
                    stored += new as u64;
                    if new > 0 || report.redactions_applied > 0 {
                        landed.entry(log).or_default().push(event);
                    }
                }
                Err(CoreError::Storage(_)) => break,
                Err(_) => rejected += 1,
            }
        }
        (
            Response::Ack { stored, rejected },
            landed.into_iter().collect(),
        )
    }

    // ── Sessions ──────────────────────────────────────────────────────

    /// Start sessions wherever there's appetite and a free pipe.
    fn pump(&mut self) {
        let ids: Vec<PipeId> = self
            .pipes
            .iter()
            .filter(|(_, p)| p.dirty && p.session.is_none())
            .map(|(id, _)| *id)
            .collect();
        for id in ids {
            let pull = self.followed_on(id);
            let push: Vec<LogId> = pull
                .iter()
                .copied()
                .filter(|log| Some(*log) == self.me)
                .collect();
            let now = self.now();
            let Some(pipe) = self.pipes.get_mut(&id) else {
                continue;
            };
            pipe.dirty = false;
            if pull.is_empty() {
                continue;
            }
            let session = SyncSession::new(pipe.name.clone(), now, pull.clone(), push);
            pipe.session = Some((session, pull));
            self.advance_session(id);
        }
    }

    fn session_response(&mut self, id: PipeId, rid: u64, response: Response) {
        let Some(pipe) = self.pipes.get_mut(&id) else {
            return;
        };
        if pipe.pending != Some(rid) || pipe.session.is_none() {
            return; // stale or unsolicited — drop it
        }
        pipe.pending = None;
        let Some((mut session, pull)) = pipe.session.take() else {
            return;
        };
        match session.feed(&mut self.db, &mut *self.state, response) {
            Ok(()) => {
                if let Some(p) = self.pipes.get_mut(&id) {
                    p.session = Some((session, pull));
                }
                self.advance_session(id);
            }
            Err(_) => {
                // Protocol garbage or local storage trouble: abandon the
                // session (cursors never ran ahead of ingest, so a fresh
                // one finishes the job whenever something next stirs).
            }
        }
    }

    /// Send the session's next request, or finish it.
    fn advance_session(&mut self, id: PipeId) {
        let Some(pipe) = self.pipes.get_mut(&id) else {
            return;
        };
        let Some((mut session, pull)) = pipe.session.take() else {
            return;
        };
        match session.next_request(&self.db) {
            Some(request) => {
                let Some(pipe) = self.pipes.get_mut(&id) else {
                    return;
                };
                let rid = pipe.next_request_id;
                pipe.next_request_id += 1;
                match pipe.tx.try_send(PipeMsg::Request { id: rid, request }) {
                    Ok(()) => {
                        pipe.pending = Some(rid);
                        pipe.session = Some((session, pull));
                    }
                    Err(_) => {
                        // Congested or closing: drop the session, retry on
                        // the next event. Nothing is lost — cursors trail
                        // ingest.
                        pipe.dirty = true;
                    }
                }
            }
            None => {
                let report = session.finish();
                if report.pulled > 0 || report.healed > 0 || report.reconciled > 0 {
                    // Bulk pulls don't carry per-event lists; tell the
                    // taps which logs moved and let readers re-query.
                    for log in &pull {
                        let item = PeerEvent {
                            log: *log,
                            events: Vec::new(),
                        };
                        emit(&mut self.firehose, &item);
                    }
                    // Eager pipes take the media while the pipe is warm.
                    self.eager_fetch(id, &pull);
                }
            }
        }
    }
}

/// Best-effort tap fan-out: a closed tap is pruned, a full one misses
/// this item (readers re-query; the tap is an invalidation stream, not a
/// ledger).
fn emit(taps: &mut Vec<mpsc::Sender<PeerEvent>>, item: &PeerEvent) {
    taps.retain_mut(|tap| match tap.try_send(item.clone()) {
        Ok(()) => true,
        Err(e) if e.is_full() => true,
        Err(_) => false,
    });
}
