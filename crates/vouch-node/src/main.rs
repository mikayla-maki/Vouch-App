//! A headless `Peer` process for exercising real sync over a real
//! transport. This is a test/dev harness, not the app — it proves the
//! `PeerActor` + `PipeEnd` machinery converges independent databases over
//! actual sockets, the way vouch-core's tests prove it over in-process
//! pipes.
//!
//! Talks to a `vouch-relay-server` (`VOUCH_MAILBOX_URL`, ws:// or wss://):
//! connects to its OWN log's mailbox there (publishing is following your
//! own log somewhere), and to one mailbox per `VOUCH_FOLLOW` entry
//! (comma-separated 64-hex LogIds) to subscribe. Store-and-forward:
//! neither side needs to be online at the same moment, within the relay's
//! retention window.
//!
//! Env vars:
//! - `VOUCH_DATA_DIR` (required): where this node's identity + claims.db live.
//! - `VOUCH_MAILBOX_URL` (required): relay server to publish through.
//! - `VOUCH_FOLLOW` (optional): comma-separated hex LogIds to follow.
//! - `VOUCH_NAME` (optional): a label for this node's own log lines.
//! - `VOUCH_SEED_CLAIM` (optional): if set, mint one `rec` claim with this
//!   text shortly after startup, so there's something to observe syncing.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use vouch_core::e2ee::{self, Identity};
use vouch_core::{Draft, ServePolicy, Writer};

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn load_or_create(dir: &Path) -> (Writer, Identity) {
    let key_path = dir.join("identity.key");
    let seed = if let Ok(bytes) = std::fs::read(&key_path)
        && let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice())
    {
        seed
    } else {
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).expect("OS randomness for a new identity");
        std::fs::create_dir_all(dir).expect("create data directory");
        std::fs::write(&key_path, seed).expect("persist device identity");
        seed
    };
    (Writer::from_seed(seed), Identity::from_seed(seed))
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
    let url = env_var("VOUCH_MAILBOX_URL").expect("VOUCH_MAILBOX_URL is required");
    let seed_claim = env_var("VOUCH_SEED_CLAIM");

    let dir = Path::new(&dir);
    let (writer, identity) = load_or_create(dir);
    let (peer, actor) =
        vouch_store::open_peer(dir, Some(writer), ServePolicy::Owned).expect("open local database");
    let my_log = peer.id().expect("this node always holds a writer");
    println!("[{name}] my log id: {my_log}");

    std::thread::spawn(move || futures::executor::block_on(actor.run()));

    println!("[{name}] publishing to own mailbox at {url}");
    vouch_transport::connect_mailbox(&peer, &url, my_log);

    let mut watched: Vec<vouch_core::LogId> = Vec::new();
    for hex in env_var("VOUCH_FOLLOW").unwrap_or_default().split(',') {
        if hex.trim().is_empty() {
            continue;
        }
        let log = vouch_transport::parse_log_id(hex)
            .unwrap_or_else(|| panic!("VOUCH_FOLLOW entry is not a 64-hex LogId: {hex}"));
        println!("[{name}] following {log} via its mailbox");
        vouch_transport::connect_mailbox(&peer, &url, log);
        watched.push(log);
    }

    if let Some(text) = seed_claim {
        std::thread::sleep(Duration::from_millis(500));
        // Sealed like all speech — there is no plaintext content path.
        let draft = Draft::new("rec")
            .at(now_ms())
            .text("subject", text.clone())
            .text("body", format!("seeded by {name}"));
        let draft = e2ee::seal_draft(&identity.content_key(), &draft).expect("seal seed claim");
        match futures::executor::block_on(peer.claim(draft)) {
            Ok(_) => println!("[{name}] claimed: {text}"),
            Err(e) => eprintln!("[{name}] failed to claim: {e}"),
        }
    }

    loop {
        std::thread::sleep(Duration::from_secs(1));
        let watched = watched.clone();
        let counts = futures::executor::block_on(peer.query(move |db| {
            let theirs: u64 = watched.iter().map(|log| db.claims().log_len(log)).sum();
            (db.claims().len(), db.claims().log_len(&my_log), theirs)
        }));
        if let Ok((total, mine, theirs)) = counts {
            println!("[{name}] total={total} mine={mine} theirs={theirs}");
        }
    }
}
