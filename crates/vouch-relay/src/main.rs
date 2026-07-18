//! A dumb relay: pairs incoming TCP connections two at a time, in arrival
//! order, and pipes raw bytes bidirectionally between each pair until
//! either side closes. It never looks at a byte of what crosses it — no
//! framing, no protocol, no addressing. That's deliberate: the sync
//! engine's sessions carry their own correlation and are meant to run over
//! *any* transport that just moves bytes, a relay included.
//!
//! Listens on `VOUCH_RELAY_ADDR` (or the first CLI argument), default
//! `127.0.0.1:7777`.

use std::env;
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    let addr = env::args()
        .nth(1)
        .or_else(|| env::var("VOUCH_RELAY_ADDR").ok())
        .unwrap_or_else(|| "127.0.0.1:7777".to_string());

    let listener = TcpListener::bind(&addr).expect("bind relay address");
    println!("vouch-relay: listening on {addr}");

    // The one connection waiting for a partner, if any. A relay handles
    // any number of concurrent pairs; this mutex only ever guards the
    // single half-formed pair at a time.
    let waiting: Arc<Mutex<Option<TcpStream>>> = Arc::new(Mutex::new(None));

    for incoming in listener.incoming() {
        let Ok(conn) = incoming else { continue };
        let peer_addr = conn.peer_addr().ok();

        let partner = waiting.lock().unwrap().take();
        match partner {
            Some(other) => {
                println!("vouch-relay: pairing {:?} <-> {:?}", other.peer_addr().ok(), peer_addr);
                pipe_pair(other, conn);
            }
            None => {
                println!("vouch-relay: {peer_addr:?} waiting for a partner");
                *waiting.lock().unwrap() = Some(conn);
            }
        }
    }
}

/// Spawn the two directions of one pair as independent threads. Neither
/// thread understands what it's forwarding; `io::copy` just moves bytes
/// until one side hangs up, at which point both directions are torn down.
fn pipe_pair(a: TcpStream, b: TcpStream) {
    let a_write = a.try_clone().expect("clone socket a");
    let b_write = b.try_clone().expect("clone socket b");

    thread::spawn(move || {
        let _ = copy_and_shutdown(a, b_write);
    });
    thread::spawn(move || {
        let _ = copy_and_shutdown(b, a_write);
    });
}

fn copy_and_shutdown(mut read_from: TcpStream, mut write_to: TcpStream) -> io::Result<()> {
    io::copy(&mut read_from, &mut write_to)?;
    let _ = write_to.shutdown(std::net::Shutdown::Write);
    Ok(())
}
