//! A socket transport for [`Peer`]: bridges a `PipeEnd` to a TCP
//! connection, exactly the shape `vouch_core::peer`'s module doc describes
//! as "the template for real ones" — the actor never touches a byte of
//! I/O, and this is the task that moves `PipeMsg`s between its `PipeEnd`
//! and the wire.
//!
//! There is no discovery yet: the tiny 32-byte `LogId` preamble ahead of
//! the `PipeMsg` stream is this crate's stand-in for it, letting a caller
//! auto-follow whoever answers without knowing who that will be in
//! advance. A real transport (or a smarter relay) will carry identity some
//! other way.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use vouch_core::{LogId, Peer, PipeId, pipe};

fn write_frame(stream: &mut TcpStream, bytes: &[u8]) -> io::Result<()> {
    stream.write_all(&(bytes.len() as u32).to_be_bytes())?;
    stream.write_all(bytes)
}

fn read_frame(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut len_bytes = [0u8; 4];
    stream.read_exact(&mut len_bytes)?;
    let mut buf = vec![0u8; u32::from_be_bytes(len_bytes) as usize];
    stream.read_exact(&mut buf)?;
    Ok(buf)
}

/// Dial `relay_addr` (a relay, or another node directly — this side can't
/// tell the difference, which is the point), hand the actor a pipe to it,
/// and spin up the two threads that carry `PipeMsg`s across the wire.
///
/// `peer` must hold a writer (its `LogId` is what gets sent in the
/// handshake preamble). If `auto_follow` is set, immediately follows
/// whatever log answers on the other end — the harness's stand-in for
/// real peer selection.
///
/// Returns as soon as the handshake completes; the bridge threads and any
/// session traffic keep running in the background for the life of the
/// process.
/// Parse a `LogId` from the 64-hex-char form `Display` produces — the
/// shape a friend's address travels in (a message, an env var, a QR code
/// someday).
pub fn parse_log_id(hex: &str) -> Option<LogId> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(LogId(bytes))
}

/// Dial one mailbox on a `vouch-relay-server` over WebSocket and follow
/// `mailbox` through it, forever: on connection loss the bridge backs
/// off, reconnects, and re-follows — a laptop waking from sleep resumes
/// syncing without a restart. The same call covers both directions of
/// the relay's contract: pointed at your *own* log's mailbox it
/// publishes (following your own log somewhere is how you publish
/// there); pointed at a friend's it subscribes. One connection per
/// mailbox — the LogId is the address, nothing else identifies it.
///
/// Each attempt registers a fresh pipe named after the mailbox's LogId;
/// cursors are keyed by that stable name, so catch-up after a reconnect
/// stays incremental. The whole bridge lives on a background thread for
/// the life of the process — connection errors are printed, never
/// returned, because there is no moment where they're final.
pub fn connect_mailbox(peer: &Peer, relay_url: &str, mailbox: LogId) {
    let peer = peer.clone();
    let url = relay_url.to_string();
    std::thread::spawn(move || {
        // Pick rustls's crypto backend explicitly, once: with both ring
        // and aws-lc-rs compiled in (feature unification does this),
        // rustls panics at the first TLS handshake rather than choose.
        static CRYPTO: std::sync::Once = std::sync::Once::new();
        CRYPTO.call_once(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build websocket runtime");

        let mut backoff = Duration::from_secs(1);
        loop {
            let (actor_end, transport_end) = pipe(64);
            let Ok(pipe_id) =
                futures::executor::block_on(peer.connect(format!("mailbox-{mailbox}"), actor_end))
            else {
                return; // peer actor gone: the app is shutting down
            };
            if futures::executor::block_on(peer.follow(mailbox, pipe_id)).is_err() {
                return;
            }

            let started = Instant::now();
            runtime.block_on(bridge_once(&url, mailbox, transport_end));
            let _ = futures::executor::block_on(peer.disconnect(pipe_id));

            // A session that held for a while earns a fresh start; rapid
            // failures back off up to a minute.
            if started.elapsed() > Duration::from_secs(60) {
                backoff = Duration::from_secs(1);
            } else {
                backoff = (backoff * 2).min(Duration::from_secs(60));
            }
            eprintln!("mailbox {mailbox}: connection ended, retrying in {backoff:?}");
            std::thread::sleep(backoff);
        }
    });
}

/// One connection's lifetime: dial, handshake, move messages until
/// either side ends. Returning (for any reason) means "reconnect".
async fn bridge_once(url: &str, mailbox: LogId, transport_end: vouch_core::PipeEnd) {
    use tokio_tungstenite::tungstenite::Message;

    let (ws, _) = match tokio_tungstenite::connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => {
            eprintln!("mailbox {mailbox}: connect to {url} failed: {e}");
            return;
        }
    };
    let (mut ws_write, mut ws_read) = ws.split();
    if ws_write
        .send(Message::Binary(mailbox.0.to_vec()))
        .await
        .is_err()
    {
        return;
    }

    let mut transport_tx = transport_end.tx;
    let mut transport_rx = transport_end.rx;

    let reader = async move {
        while let Some(msg) = ws_read.next().await {
            let Ok(Message::Binary(bytes)) = msg else { break };
            let Ok(decoded) = bincode::deserialize::<vouch_core::PipeMsg>(&bytes) else {
                continue; // garbage frame: drop it, the session retries
            };
            if transport_tx.send(decoded).await.is_err() {
                break; // actor gone
            }
        }
    };
    let writer = async move {
        while let Some(msg) = transport_rx.next().await {
            let bytes = bincode::serialize(&msg).expect("encode PipeMsg");
            if ws_write.send(Message::Binary(bytes)).await.is_err() {
                break; // relay hung up
            }
        }
    };
    futures::join!(reader, writer);
}

pub fn connect_relay(
    peer: &Peer,
    relay_addr: &str,
    auto_follow: bool,
) -> io::Result<(LogId, PipeId)> {
    let my_log = peer
        .id()
        .expect("connect_relay requires a peer that holds a writer");

    let mut socket = TcpStream::connect(relay_addr)?;
    socket.set_nodelay(true).ok();

    socket.write_all(&my_log.0)?;
    let mut remote_bytes = [0u8; 32];
    socket.read_exact(&mut remote_bytes)?;
    let remote_log = LogId(remote_bytes);

    let (actor_end, transport_end) = pipe(64);
    let pipe_id = futures::executor::block_on(peer.connect(remote_log.to_string(), actor_end))
        .map_err(io::Error::other)?;

    if auto_follow {
        futures::executor::block_on(peer.follow(remote_log, pipe_id)).map_err(io::Error::other)?;
    }

    let mut reader_socket = socket.try_clone()?;
    let mut transport_tx = transport_end.tx;
    std::thread::spawn(move || {
        loop {
            let bytes = match read_frame(&mut reader_socket) {
                Ok(bytes) => bytes,
                Err(_) => break, // relay/peer hung up
            };
            let msg: vouch_core::PipeMsg = match bincode::deserialize(&bytes) {
                Ok(msg) => msg,
                Err(_) => continue, // garbage frame: drop it, the session retries
            };
            if futures::executor::block_on(transport_tx.send(msg)).is_err() {
                break; // actor gone
            }
        }
    });

    let mut writer_socket = socket;
    let mut transport_rx = transport_end.rx;
    std::thread::spawn(move || {
        loop {
            let Some(msg) = futures::executor::block_on(transport_rx.next()) else {
                break; // actor gone
            };
            let bytes = bincode::serialize(&msg).expect("encode PipeMsg");
            if write_frame(&mut writer_socket, &bytes).is_err() {
                break; // relay/peer hung up
            }
        }
    });

    Ok((remote_log, pipe_id))
}
