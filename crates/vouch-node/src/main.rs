//! A headless `Peer` process for exercising real sync over a real
//! transport (a TCP connection through `vouch-relay`, or directly between
//! two nodes). This is a test/dev harness, not the app — it proves the
//! `PeerActor` + `PipeEnd` machinery converges two independent databases
//! over an actual socket, the way `main.rs`'s in-process `pipe()` proves it
//! converges two actors in a test.
//!
//! Env vars:
//! - `VOUCH_DATA_DIR` (required): where this node's identity + claims.db live.
//! - `VOUCH_RELAY_ADDR` (required): the relay (or peer) to dial, e.g. `127.0.0.1:7777`.
//! - `VOUCH_NAME` (optional): a label for this node's own log lines.
//! - `VOUCH_AUTO_FOLLOW` (optional, "1"/"true"): follow whatever log is on
//!   the other end of the pipe as soon as the handshake completes. There is
//!   no discovery yet, so this is the harness's stand-in for it.
//! - `VOUCH_SEED_CLAIM` (optional): if set, mint one `rec` claim with this
//!   text shortly after startup, so there's something to observe syncing.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vouch_core::{Draft, ServePolicy, Writer};

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn env_flag(name: &str) -> bool {
    matches!(env_var(name).as_deref(), Some("1") | Some("true"))
}

fn load_or_create_writer(dir: &Path) -> Writer {
    let key_path = dir.join("identity.key");
    if let Ok(bytes) = std::fs::read(&key_path)
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        return Writer::from_seed(seed);
    }
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).expect("OS randomness for a new identity");
    std::fs::create_dir_all(dir).expect("create data directory");
    std::fs::write(&key_path, seed).expect("persist device identity");
    Writer::from_seed(seed)
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn main() {
    let name = env_var("VOUCH_NAME").unwrap_or_else(|| "node".to_string());
    let dir = env_var("VOUCH_DATA_DIR").expect("VOUCH_DATA_DIR is required");
    let relay_addr = env_var("VOUCH_RELAY_ADDR").expect("VOUCH_RELAY_ADDR is required");
    let auto_follow = env_flag("VOUCH_AUTO_FOLLOW");
    let seed_claim = env_var("VOUCH_SEED_CLAIM");

    let dir = Path::new(&dir);
    let writer = load_or_create_writer(dir);
    let (peer, actor) =
        vouch_store::open_peer(dir, Some(writer), ServePolicy::Owned).expect("open local database");
    let my_log = peer.id().expect("this node always holds a writer");
    println!("[{name}] my log id: {my_log}");

    std::thread::spawn(move || futures::executor::block_on(actor.run()));

    println!("[{name}] connecting to {relay_addr}");
    let (remote_log, _pipe_id) =
        vouch_transport::connect_relay(&peer, &relay_addr, auto_follow).expect("connect to relay");
    println!("[{name}] remote log id: {remote_log}");
    if auto_follow {
        println!("[{name}] auto-following {remote_log}");
    }

    if let Some(text) = seed_claim {
        std::thread::sleep(Duration::from_millis(500));
        let draft = Draft::new("rec")
            .at(now_ms())
            .text("subject", text.clone())
            .text("body", format!("seeded by {name}"));
        match futures::executor::block_on(peer.claim(draft)) {
            Ok(_) => println!("[{name}] claimed: {text}"),
            Err(e) => eprintln!("[{name}] failed to claim: {e}"),
        }
    }

    loop {
        std::thread::sleep(Duration::from_secs(1));
        let counts = futures::executor::block_on(peer.query(move |db| {
            (
                db.claims().len(),
                db.claims().log_len(&my_log),
                db.claims().log_len(&remote_log),
            )
        }));
        if let Ok((total, mine, theirs)) = counts {
            println!("[{name}] total={total} mine={mine} theirs={theirs}");
        }
    }
}
