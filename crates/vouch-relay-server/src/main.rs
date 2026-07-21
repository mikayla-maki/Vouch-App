//! A bounded store-and-forward relay.
//!
//! One `Peer` per `LogId` mailbox (see `registry`): a durable, no-writer,
//! `ServePolicy::Everything` peer, so it accepts and serves whatever's
//! published under that log by whoever actually holds its private key.
//! Each connecting client picks a mailbox by sending the 32-byte LogId as
//! the first WebSocket message (see `bridge`) — the LogId IS the address,
//! Iroh-style. Publishing requires a deniable key-possession handshake
//! (no signatures anywhere; the relay's own records prove nothing). A
//! mailbox costs disk only after an authenticated publish; bare
//! connections are answered dormant.
//!
//! This is explicitly NOT a permanent archive: a background sweep purges
//! claims older than `VOUCH_RELAY_RETENTION_DAYS` (default 7) and their
//! orphaned media. Permanence is a property of always-on peers, not this
//! relay — see the doc comment on `ClaimStore::purge_older_than` for why
//! cursor-driven retention could never be sound here.
//!
//! Env vars: `VOUCH_RELAY_BIND` (default `0.0.0.0:9443`),
//! `VOUCH_RELAY_DATA_DIR` (required), `VOUCH_RELAY_RETENTION_DAYS`
//! (default 7), `VOUCH_RELAY_MAX_CONNS` (default 128).

mod bridge;
mod registry;

use std::collections::HashSet;
use std::env;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use registry::Registry;

fn env_var(name: &str) -> Option<String> {
    env::var(name).ok()
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[tokio::main]
async fn main() {
    let bind = env_var("VOUCH_RELAY_BIND").unwrap_or_else(|| "0.0.0.0:9443".to_string());
    let data_dir: PathBuf = env_var("VOUCH_RELAY_DATA_DIR")
        .expect("VOUCH_RELAY_DATA_DIR is required")
        .into();
    let retention_days: i64 = env_var("VOUCH_RELAY_RETENTION_DAYS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(7);
    let max_conns: usize = env_var("VOUCH_RELAY_MAX_CONNS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(128);

    std::fs::create_dir_all(&data_dir).expect("create data directory");
    let registry = Registry::new(data_dir);

    println!(
        "vouch-relay-server: listening on {bind}, retention {retention_days}d, max {max_conns} connections"
    );

    tokio::spawn(gc_loop(registry.clone(), retention_days));

    let listener = TcpListener::bind(&bind).await.expect("bind relay address");
    accept_loop(listener, registry, max_conns).await;
}

async fn accept_loop(listener: TcpListener, registry: Registry, max_conns: usize) {
    let limiter = Arc::new(Semaphore::new(max_conns));
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        // Over capacity: drop before the WebSocket handshake even starts,
        // so a connection flood costs an accept and nothing more.
        let Ok(permit) = limiter.clone().try_acquire_owned() else {
            eprintln!("connection from {addr} dropped: at capacity");
            continue;
        };
        let registry = registry.clone();
        tokio::spawn(async move {
            let _permit = permit;
            if let Err(e) = bridge::serve_connection(stream, registry).await {
                eprintln!("connection from {addr} ended: {e}");
            }
        });
    }
}

async fn gc_loop(registry: Registry, retention_days: i64) {
    let retention_ms = retention_days * 24 * 60 * 60 * 1000;
    let mut ticker = tokio::time::interval(Duration::from_secs(60 * 60));
    loop {
        ticker.tick().await;
        let cutoff = now_ms() - retention_ms;

        // Live mailboxes: through the actor's own verbs, serialized with
        // whatever client traffic they're carrying.
        let live = registry.live_mailboxes().await;
        let live_ids: HashSet<_> = live.iter().map(|(id, _)| *id).collect();
        for (log_id, peer) in live {
            match peer.gc_claims_older_than(cutoff).await {
                Ok(purged) if !purged.is_empty() => {
                    println!("gc: purged {} claim(s) from {log_id}", purged.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("gc failed for {log_id}: {e}"),
            }
            match peer.gc_blobs().await {
                Ok(dropped) if !dropped.is_empty() => {
                    println!("gc: dropped {} orphaned blob(s) from {log_id}", dropped.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("blob gc failed for {log_id}: {e}"),
            }
        }

        // Dormant on-disk mailboxes from earlier lifetimes: open the
        // Database directly, sweep, and drop it — old mailboxes keep
        // draining without being pinned into memory. The rare race with a
        // concurrent materialize surfaces as SQLITE_BUSY here: skipped
        // this hour, retried next.
        for log_id in registry.on_disk() {
            if live_ids.contains(&log_id) {
                continue;
            }
            let dir = registry.dir_of(&log_id);
            let swept = tokio::task::spawn_blocking(move || -> Result<(usize, usize), String> {
                let mut db = vouch_store::open(&dir).map_err(|e| e.to_string())?;
                let claims = db.gc_claims_older_than(cutoff).map_err(|e| e.to_string())?;
                let blobs = db.gc_blobs().map_err(|e| e.to_string())?;
                Ok((claims.len(), blobs.len()))
            })
            .await;
            match swept {
                Ok(Ok((claims, blobs))) if claims > 0 || blobs > 0 => {
                    println!("gc: purged {claims} claim(s), {blobs} blob(s) from dormant {log_id}");
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => eprintln!("gc failed for dormant {log_id}: {e}"),
                Err(e) => eprintln!("gc task failed for {log_id}: {e}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::{SinkExt, StreamExt};
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::TcpStream;
    use tokio_tungstenite::tungstenite::Message;
    use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
    use vouch_core::e2ee::{self, Identity};
    use vouch_core::sync::{Request, Response};
    use vouch_core::{LogId, PipeMsg, Value, Writer};

    type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

    fn temp_dir() -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("vouch-relay-test-{}-{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    async fn start_server(max_conns: usize) -> (std::net::SocketAddr, PathBuf) {
        let dir = temp_dir();
        let registry = Registry::new(dir.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(accept_loop(listener, registry, max_conns));
        (addr, dir)
    }

    async fn connect(addr: std::net::SocketAddr, mailbox: LogId) -> Ws {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        ws.send(Message::Binary(mailbox.0.to_vec())).await.unwrap();
        ws
    }

    /// Connect with publish intent and answer the challenge with
    /// `identity`'s key. Returns the authenticated session; panics if the
    /// server closes it (callers proving the failure path do it by hand).
    async fn connect_publish(addr: std::net::SocketAddr, mailbox: LogId, identity: &Identity) -> Ws {
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        let mut hello = mailbox.0.to_vec();
        hello.push(1);
        ws.send(Message::Binary(hello)).await.unwrap();
        let challenge = loop {
            match ws.next().await.expect("server closed during handshake").unwrap() {
                Message::Binary(bytes) => break bytes,
                _ => continue,
            }
        };
        let eph_pub: [u8; 32] = challenge[..32].try_into().unwrap();
        let nonce: [u8; 16] = challenge[32..48].try_into().unwrap();
        let proof = e2ee::publish_proof(identity, &eph_pub, &nonce);
        ws.send(Message::Binary(proof.to_vec())).await.unwrap();
        ws
    }

    /// Send one request and read to its response, skipping frames.
    async fn roundtrip(ws: &mut Ws, id: u64, request: Request) -> Response {
        let msg = PipeMsg::Request { id, request };
        ws.send(Message::Binary(bincode::serialize(&msg).unwrap()))
            .await
            .unwrap();
        loop {
            let msg = ws.next().await.expect("connection ended").unwrap();
            let Message::Binary(bytes) = msg else { continue };
            if let Ok(PipeMsg::Response { id: rid, response }) = bincode::deserialize(&bytes)
                && rid == id
            {
                return response;
            }
        }
    }

    fn mailbox_dirs(dir: &PathBuf) -> usize {
        std::fs::read_dir(dir).unwrap().count()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_of_a_nonexistent_mailbox_costs_no_disk() {
        let (addr, dir) = start_server(16).await;
        let mailbox = LogId([7; 32]);
        let mut ws = connect(addr, mailbox).await;

        let response = roundtrip(&mut ws, 0, Request::Status { log: mailbox }).await;
        let Response::Status { count, .. } = response else {
            panic!("expected a synthesized Status, got {response:?}");
        };
        assert_eq!(count, 0);
        assert_eq!(mailbox_dirs(&dir), 0, "a bare reader must allocate nothing");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_valid_publish_materializes_and_survives_for_the_next_reader() {
        let mut writer = Writer::from_seed([1; 32]);
        let identity = Identity::from_seed([1; 32]);
        let mailbox = writer.id();
        let event = writer
            .claim(Value::map([
                ("type", Value::text("rec")),
                ("subject", Value::text("Joe's Pizza")),
            ]))
            .unwrap();

        let (addr, dir) = start_server(16).await;
        let mut ws = connect_publish(addr, mailbox, &identity).await;
        let response = roundtrip(
            &mut ws,
            0,
            Request::Publish {
                events: vec![event],
            },
        )
        .await;
        let Response::Ack { stored, .. } = response else {
            panic!("expected an Ack, got {response:?}");
        };
        assert_eq!(stored, 1);
        assert_eq!(mailbox_dirs(&dir), 1, "the publish materialized a mailbox");

        // A separate connection reads it back through the real mailbox.
        let mut ws2 = connect(addr, mailbox).await;
        let response = roundtrip(
            &mut ws2,
            0,
            Request::Since {
                log: mailbox,
                have: 0,
                max: 10,
            },
        )
        .await;
        let Response::Events { events } = response else {
            panic!("expected Events, got {response:?}");
        };
        assert_eq!(events.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_wrong_key_cannot_authenticate_as_publisher() {
        // Mallory answers the victim-mailbox challenge with her own key:
        // the server closes the connection outright.
        let mallory = Identity::from_seed([6; 32]);
        let victim = Writer::from_seed([2; 32]).id();

        let (addr, dir) = start_server(16).await;
        let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{addr}"))
            .await
            .unwrap();
        let mut hello = victim.0.to_vec();
        hello.push(1);
        ws.send(Message::Binary(hello)).await.unwrap();
        let challenge = loop {
            match ws.next().await.expect("server closed during handshake").unwrap() {
                Message::Binary(bytes) => break bytes,
                _ => continue,
            }
        };
        let eph_pub: [u8; 32] = challenge[..32].try_into().unwrap();
        let nonce: [u8; 16] = challenge[32..48].try_into().unwrap();
        let proof = e2ee::publish_proof(&mallory, &eph_pub, &nonce);
        ws.send(Message::Binary(proof.to_vec())).await.unwrap();

        let closed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await;
        assert!(closed.is_ok(), "a failed proof must end the connection");
        assert_eq!(mailbox_dirs(&dir), 0, "a failed handshake allocates nothing");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reader_session_cannot_publish_at_all() {
        // A well-formed event, but the session never authenticated: every
        // event is rejected and no disk is allocated. (Without publish
        // auth this would be the old forgery hole — the relay can't check
        // MAC tags, so the session is the only gate.)
        let mut writer = Writer::from_seed([2; 32]);
        let mailbox = writer.id();
        let event = writer
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();

        let (addr, dir) = start_server(16).await;
        let mut ws = connect(addr, mailbox).await;
        let response = roundtrip(
            &mut ws,
            0,
            Request::Publish {
                events: vec![event],
            },
        )
        .await;
        let Response::Ack { stored, rejected } = response else {
            panic!("expected an Ack, got {response:?}");
        };
        assert_eq!(stored, 0);
        assert_eq!(rejected, 1);
        assert_eq!(mailbox_dirs(&dir), 0, "a rejected publish allocates nothing");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_publisher_cannot_smuggle_another_logs_events() {
        // Alice authenticates for HER mailbox but publishes an event
        // belonging to a different log: address mismatch, rejected.
        let alice = Identity::from_seed([1; 32]);
        let mailbox = Writer::from_seed([1; 32]).id();
        let mut other = Writer::from_seed([3; 32]);
        let stray = other
            .claim(Value::map([("type", Value::text("rec"))]))
            .unwrap();

        let (addr, dir) = start_server(16).await;
        let mut ws = connect_publish(addr, mailbox, &alice).await;
        let response = roundtrip(
            &mut ws,
            0,
            Request::Publish {
                events: vec![stray],
            },
        )
        .await;
        let Response::Ack { stored, rejected } = response else {
            panic!("expected an Ack, got {response:?}");
        };
        assert_eq!(stored, 0);
        assert_eq!(rejected, 1);
        assert_eq!(mailbox_dirs(&dir), 0, "a rejected publish allocates nothing");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oversized_message_ends_the_connection() {
        let (addr, _dir) = start_server(16).await;
        let mut ws = connect(addr, LogId([7; 32])).await;

        // Past the 8 MiB cap: the server must drop us, not buffer it.
        let _ = ws.send(Message::Binary(vec![0u8; 9 * 1024 * 1024])).await;
        let ended = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            loop {
                match ws.next().await {
                    None | Some(Err(_)) => break,
                    Some(Ok(Message::Close(_))) => break,
                    Some(Ok(_)) => {}
                }
            }
        })
        .await;
        assert!(ended.is_ok(), "the connection should have been closed");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn connections_past_the_cap_are_dropped() {
        let (addr, _dir) = start_server(1).await;
        let mailbox = LogId([7; 32]);
        let mut ws1 = connect(addr, mailbox).await;
        // Prove the first connection is fully established and served.
        let _ = roundtrip(&mut ws1, 0, Request::Status { log: mailbox }).await;

        // The second never completes a WebSocket handshake.
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio_tungstenite::connect_async(format!("ws://{addr}")),
        )
        .await;
        match second {
            Ok(Err(_)) => {}                       // rejected — expected
            Err(_) => {}                           // or hung until dropped
            Ok(Ok(_)) => panic!("a connection past the cap was accepted"),
        }
    }
}
