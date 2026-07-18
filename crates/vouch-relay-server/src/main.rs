//! A bounded store-and-forward relay.
//!
//! One `Peer` per `LogId` mailbox (see `registry`): a durable, no-writer,
//! `ServePolicy::Everything` peer, so it accepts and serves whatever's
//! published under that log by whoever actually holds its private key.
//! Each connecting client picks a mailbox by sending its 32-byte LogId as
//! the first WebSocket message (see `bridge`) — the LogId IS the address,
//! Iroh-style; no separate secret, since signature verification already
//! gatekeeps who can publish.
//!
//! This is explicitly NOT a permanent archive: a background sweep purges
//! claims older than `VOUCH_RELAY_RETENTION_DAYS` (default 7) via
//! `Peer::gc_claims_older_than`. Permanence is a property of always-on
//! peers, not this relay — see the doc comment on
//! `ClaimStore::purge_older_than` for why cursor-driven retention could
//! never be sound here.
//!
//! Env vars: `VOUCH_RELAY_BIND` (default `0.0.0.0:9443`), `VOUCH_RELAY_DATA_DIR`
//! (required), `VOUCH_RELAY_RETENTION_DAYS` (default 7).

mod bridge;
mod registry;

use std::env;
use std::path::PathBuf;
use std::time::Duration;

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

    std::fs::create_dir_all(&data_dir).expect("create data directory");
    let registry = Registry::new(data_dir);

    println!("vouch-relay-server: listening on {bind}, retention {retention_days}d");

    tokio::spawn(gc_loop(registry.clone(), retention_days));

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .expect("bind relay address");
    loop {
        let (stream, addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("accept failed: {e}");
                continue;
            }
        };
        let registry = registry.clone();
        tokio::spawn(async move {
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
        for (log_id, peer) in registry.snapshot().await {
            match peer.gc_claims_older_than(cutoff).await {
                Ok(purged) if !purged.is_empty() => {
                    println!("gc: purged {} claim(s) from {log_id}", purged.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("gc failed for {log_id}: {e}"),
            }
            // Purged claims orphan their media; reclaim the bytes too.
            match peer.gc_blobs().await {
                Ok(dropped) if !dropped.is_empty() => {
                    println!("gc: dropped {} orphaned blob(s) from {log_id}", dropped.len());
                }
                Ok(_) => {}
                Err(e) => eprintln!("blob gc failed for {log_id}: {e}"),
            }
        }
    }
}
