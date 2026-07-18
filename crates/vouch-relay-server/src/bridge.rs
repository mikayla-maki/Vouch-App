//! Bridges one WebSocket connection to one pipe on a mailbox `Peer` —
//! the same "template for real ones" shape `vouch_core::peer`'s module
//! doc describes for `vouch-transport`'s TCP bridge, just carried over
//! WebSocket messages (already framed, so no length-prefix needed).
//!
//! The handshake is one message: the client's first WebSocket frame is
//! the 32-byte `LogId` of the mailbox it wants to reach. That's the
//! entire address — no separate secret, since signature verification
//! already gatekeeps who can validly publish under a log.
//!
//! ## Dormant until proven costly
//!
//! A connection to a mailbox that doesn't exist yet allocates NOTHING —
//! no directory, no SQLite, no actor. The bridge answers its reads
//! synthetically (the honest empty answers an empty mailbox would give)
//! and only materializes the mailbox when a *validly signed* publish for
//! that exact log arrives. Keypairs are free, so this doesn't stop a
//! determined attacker from creating mailboxes — but it makes creation
//! cost a signature the server verifies, instead of being free for
//! anyone who can send 32 bytes. The synthesized `Status` carries a
//! fresh random instance id; when the mailbox later materializes with
//! its real one, clients see an instance change and reset cursors —
//! exactly the relay-reborn path the protocol already heals.

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use vouch_core::sync::{InstanceId, Notify, Request, Response};
use vouch_core::{LogId, Peer, PipeMsg, pipe};

use crate::registry::Registry;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(0);

/// Claims are capped at 64 KiB each; a full publish batch is 256 of
/// them. 8 MiB covers the worst legitimate batch with room to spare
/// while bounding what one frame can make this server buffer.
const MAX_WS_MESSAGE: usize = 8 * 1024 * 1024;

type BoxError = Box<dyn Error + Send + Sync>;
type WsWrite = SplitSink<WebSocketStream<TcpStream>, Message>;
type WsRead = SplitStream<WebSocketStream<TcpStream>>;

pub async fn serve_connection(stream: TcpStream, registry: Registry) -> Result<(), BoxError> {
    let config = WebSocketConfig {
        max_message_size: Some(MAX_WS_MESSAGE),
        max_frame_size: Some(MAX_WS_MESSAGE),
        ..Default::default()
    };
    let ws = tokio_tungstenite::accept_async_with_config(stream, Some(config)).await?;
    let (mut ws_write, mut ws_read) = ws.split();

    let Some(first) = ws_read.next().await else {
        return Ok(()); // closed before the handshake — nothing to do
    };
    let Message::Binary(bytes) = first? else {
        return Err("expected a binary handshake frame".into());
    };
    let log_bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| "handshake frame must be exactly 32 bytes")?;
    let mailbox_log = LogId(log_bytes);

    let (peer, initial) = match registry.open_existing(mailbox_log).await {
        Some(peer) => (peer, None),
        None => match dormant_phase(&mut ws_read, &mut ws_write, mailbox_log, &registry).await? {
            Some((peer, publish)) => (peer, Some(publish)),
            None => return Ok(()), // closed while dormant: zero cost
        },
    };

    live_phase(peer, ws_read, ws_write, mailbox_log, initial).await
}

async fn send_msg(ws_write: &mut WsWrite, msg: &PipeMsg) -> Result<(), BoxError> {
    let bytes = bincode::serialize(msg)?;
    ws_write.send(Message::Binary(bytes)).await?;
    Ok(())
}

/// Answer a nonexistent mailbox honestly (the empty answers an empty
/// mailbox would give) until either the connection ends or a validly
/// signed publish for this log justifies materializing it.
async fn dormant_phase(
    ws_read: &mut WsRead,
    ws_write: &mut WsWrite,
    mailbox_log: LogId,
    registry: &Registry,
) -> Result<Option<(Peer, PipeMsg)>, BoxError> {
    let mut instance_bytes = [0u8; 16];
    getrandom::fill(&mut instance_bytes).expect("OS randomness for a dormant instance id");
    let instance = InstanceId(instance_bytes);
    let empty_fingerprint = [0u8; 32];

    while let Some(msg) = ws_read.next().await {
        let Ok(msg) = msg else { break };
        let Message::Binary(bytes) = msg else {
            continue; // ping/pong etc. — the library handles the replies
        };
        let Ok(decoded) = bincode::deserialize::<PipeMsg>(&bytes) else {
            continue; // garbage frame: drop it, the session retries
        };
        match decoded {
            PipeMsg::Request { id, request } => {
                let response = match request {
                    Request::Publish { events } => {
                        let total = events.len() as u64;
                        let valid: Vec<_> = events
                            .into_iter()
                            .filter(|e| {
                                e.header().is_ok_and(|h| h.log_id == mailbox_log)
                                    && e.verify().is_ok()
                            })
                            .collect();
                        if valid.is_empty() {
                            Response::Ack {
                                stored: 0,
                                rejected: total,
                            }
                        } else {
                            // The one event allowed to cost us disk.
                            let peer = registry.materialize(mailbox_log).await;
                            let publish = PipeMsg::Request {
                                id,
                                request: Request::Publish { events: valid },
                            };
                            return Ok(Some((peer, publish)));
                        }
                    }
                    Request::Status { .. } => Response::Status {
                        count: 0,
                        fingerprint: empty_fingerprint,
                        instance,
                    },
                    Request::Since { .. } | Request::Claims { .. } => {
                        Response::Events { events: Vec::new() }
                    }
                    Request::Hashes { .. } => Response::Hashes {
                        entries: Vec::new(),
                    },
                    Request::GetBlob { .. } => Response::Blob { bytes: None },
                    Request::PutBlob { .. } => Response::Ack {
                        stored: 0,
                        rejected: 0,
                    },
                };
                send_msg(ws_write, &PipeMsg::Response { id, response }).await?;
            }
            PipeMsg::Watch(logs) => {
                // One heartbeat per watched log, same as a live mailbox:
                // the watcher settles against emptiness or rings its own
                // doorbell — which leads to the synthesized session above.
                for log in logs {
                    let beat = Notify {
                        log,
                        events: Vec::new(),
                        count: 0,
                        fingerprint: empty_fingerprint,
                        instance,
                    };
                    send_msg(ws_write, &PipeMsg::Frame(beat)).await?;
                }
            }
            // The dormant bridge never sends requests, so responses and
            // frames from the client have nothing to land on.
            PipeMsg::Response { .. } | PipeMsg::Frame(_) => {}
        }
    }
    Ok(None)
}

async fn live_phase(
    peer: Peer,
    mut ws_read: WsRead,
    mut ws_write: WsWrite,
    mailbox_log: LogId,
    initial: Option<PipeMsg>,
) -> Result<(), BoxError> {
    let (actor_end, transport_end) = pipe(64);
    // A generic per-connection name: cursors don't persist across a
    // reconnect (a fresh name never matches a prior one), which costs a
    // full re-sync on reconnect rather than an incremental one — wasteful
    // but not incorrect, since ingest is idempotent. A future version
    // could have the client also present a stable identity to reuse.
    let conn_id = NEXT_CONN_ID.fetch_add(1, Ordering::Relaxed);
    peer.connect(format!("guest-{conn_id}"), actor_end)
        .await
        .map_err(|e| format!("connect failed: {e}"))?;

    let mut transport_tx = transport_end.tx;
    let mut transport_rx = transport_end.rx;

    // The publish that ended the dormant phase, already verified there.
    if let Some(msg) = initial {
        let _ = transport_tx.send(msg).await;
    }

    let reader = async move {
        while let Some(msg) = ws_read.next().await {
            let Ok(Message::Binary(bytes)) = msg else {
                break;
            };
            let Ok(mut decoded) = bincode::deserialize::<PipeMsg>(&bytes) else {
                continue; // garbage frame: drop it, the session retries
            };
            // This mailbox only ever holds `mailbox_log`'s claims — drop
            // anything else here, before it ever reaches the Peer, so a
            // guest can never write into someone else's log through it.
            if let PipeMsg::Request {
                request: Request::Publish { events },
                ..
            } = &mut decoded
            {
                events.retain(|e| {
                    e.header()
                        .map(|h| h.log_id == mailbox_log)
                        .unwrap_or(false)
                });
            }
            if transport_tx.send(decoded).await.is_err() {
                break;
            }
        }
    };

    let writer = async move {
        while let Some(msg) = transport_rx.next().await {
            let Ok(bytes) = bincode::serialize(&msg) else {
                continue;
            };
            if ws_write.send(Message::Binary(bytes)).await.is_err() {
                break;
            }
        }
    };

    tokio::join!(reader, writer);
    Ok(())
}
