//! Bridges one WebSocket connection to one pipe on a mailbox `Peer` —
//! the same "template for real ones" shape `vouch_core::peer`'s module
//! doc describes for `vouch-transport`'s TCP bridge, just carried over
//! WebSocket messages (already framed, so no length-prefix needed) rather
//! than a raw byte stream.
//!
//! The handshake is one message: the client's first WebSocket frame is
//! the 32-byte `LogId` of the mailbox it wants to reach. That's the
//! entire address — no separate secret, since signature verification
//! already gatekeeps who can validly publish under a log.

use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};

use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use vouch_core::sync::Request;
use vouch_core::{LogId, PipeMsg, pipe};

use crate::registry::Registry;

static NEXT_CONN_ID: AtomicU64 = AtomicU64::new(0);

type BoxError = Box<dyn Error + Send + Sync>;

pub async fn serve_connection(stream: TcpStream, registry: Registry) -> Result<(), BoxError> {
    let ws = tokio_tungstenite::accept_async(stream).await?;
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

    let peer = registry.mailbox(mailbox_log).await;
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
            if ws_write.send(Message::Binary(bytes.into())).await.is_err() {
                break;
            }
        }
    };

    tokio::join!(reader, writer);
    Ok(())
}
