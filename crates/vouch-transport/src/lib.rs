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
