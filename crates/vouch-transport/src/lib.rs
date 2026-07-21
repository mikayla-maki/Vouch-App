//! The mailbox transport for [`Peer`]: bridges a `PipeEnd` to a
//! `vouch-relay-server` over WebSocket, exactly the shape
//! `vouch_core::peer`'s module doc describes as "the template for real
//! ones" — the actor never touches a byte of I/O, and this is the task
//! that moves `PipeMsg`s between its `PipeEnd` and the wire.

use std::time::{Duration, Instant};

use futures::{SinkExt, StreamExt};
use vouch_core::e2ee::{self, Identity};
use vouch_core::{LogId, Peer, pipe};

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
/// the relay's contract: pointed at your *own* log's mailbox with
/// `publish: Some(identity)` it authenticates the deniable
/// key-possession handshake and publishes (following your own log
/// somewhere is how you publish there); pointed at a friend's with
/// `publish: None` it subscribes read-only. One connection per mailbox —
/// the LogId is the address, nothing else identifies it.
///
/// The handshake never signs anything: the proof is a MAC under a
/// DH-agreed secret (see [`e2ee::publish_proof`]), so the relay is
/// convinced in the moment and holds evidence of nothing afterward.
///
/// Each attempt registers a fresh pipe named after the mailbox's LogId;
/// cursors are keyed by that stable name, so catch-up after a reconnect
/// stays incremental. The whole bridge lives on a background thread for
/// the life of the process — connection errors are printed, never
/// returned, because there is no moment where they're final.
pub fn connect_mailbox(peer: &Peer, relay_url: &str, mailbox: LogId, publish: Option<Identity>) {
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
            runtime.block_on(bridge_once(&url, mailbox, publish.clone(), transport_end));
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

/// One connection's lifetime: dial, handshake (+ publish auth when this
/// is our own mailbox), move messages until either side ends. Returning
/// (for any reason) means "reconnect".
async fn bridge_once(
    url: &str,
    mailbox: LogId,
    publish: Option<Identity>,
    transport_end: vouch_core::PipeEnd,
) {
    use tokio_tungstenite::tungstenite::Message;

    let (ws, _) = match tokio_tungstenite::connect_async(url).await {
        Ok(ok) => ok,
        Err(e) => {
            eprintln!("mailbox {mailbox}: connect to {url} failed: {e}");
            return;
        }
    };
    let (mut ws_write, mut ws_read) = ws.split();
    let mut hello = mailbox.0.to_vec();
    if publish.is_some() {
        hello.push(1); // publish intent: the server will challenge us
    }
    if ws_write.send(Message::Binary(hello)).await.is_err() {
        return;
    }
    if let Some(identity) = &publish {
        // The server's challenge: its ephemeral X25519 public + a nonce.
        let challenge = loop {
            match ws_read.next().await {
                Some(Ok(Message::Binary(bytes))) => break bytes,
                Some(Ok(_)) => continue, // ping/pong
                _ => return,
            }
        };
        if challenge.len() != 48 {
            eprintln!("mailbox {mailbox}: malformed publish challenge");
            return;
        }
        let eph_pub: [u8; 32] = challenge[..32].try_into().expect("length checked");
        let nonce: [u8; 16] = challenge[32..].try_into().expect("length checked");
        let proof = e2ee::publish_proof(identity, &eph_pub, &nonce);
        if ws_write.send(Message::Binary(proof.to_vec())).await.is_err() {
            return;
        }
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

