//! The server half: answer one request from a `Database`.
//!
//! Stateless by construction — every answer is computed fresh from the
//! database, so a responder holds no per-conversation memory and a session
//! can die at any boundary without leaving the server confused. The relay
//! binary is an HTTP (or iroh) shim around this one function plus an auth
//! check on `Publish`/`PutBlob`; the in-process test pipe is this function
//! with no shim at all.

use crate::{BlobHash, Database, Error as CoreError};

use super::protocol::{InstanceId, MAX_SERVE_BATCH, Request, Response};

/// Answer one request. `instance` is the incarnation of this database's
/// arrival order (minted by whoever owns the database's lifecycle — at
/// boot for a relay, at file creation for an app store). `now` is the
/// caller's clock for `received_at` metadata; vouch-sync never reads one.
///
/// Errors are *our* storage failing, never the requester misbehaving:
/// unverifiable published events are counted and dropped (`Ack.rejected`),
/// unknown ids and absent blobs answer empty. A request can't hurt us;
/// only our own disk can.
pub fn respond(
    db: &mut Database,
    instance: InstanceId,
    now: i64,
    req: Request,
) -> Result<Response, CoreError> {
    Ok(match req {
        Request::Status { log } => Response::Status {
            count: db.claims().log_len(&log),
            fingerprint: db.claims().fingerprint(&log),
            instance,
        },
        Request::Since { log, have, max } => {
            let mut events = db.claims().serve_since(&log, have);
            events.truncate(max.min(MAX_SERVE_BATCH) as usize);
            Response::Events { events }
        }
        Request::Hashes { log } => Response::Hashes {
            entries: db.claims().log_hashes(&log),
        },
        Request::Claims { ids } => Response::Events {
            events: ids
                .iter()
                .filter_map(|id| db.claims().get(id))
                .map(|c| c.event)
                .collect(),
        },

        Request::PutBlob { bytes } => {
            // Content-addressed: the bytes name themselves. Quota and
            // who-may-upload are the transport wrapper's policy; once the
            // bytes are in, they're an ordinary cache entry that GC
            // reclaims if no live body ever pins them.
            let hash = BlobHash(*blake3::hash(&bytes).as_bytes());
            let new = db.ingest_blob(hash, bytes)?;
            Response::Ack {
                stored: new as u64,
                rejected: 0,
            }
        }
        Request::Publish { events } => {
            let mut stored = 0u64;
            let mut rejected = 0u64;
            for event in events {
                match db.ingest_at(event, now) {
                    Ok(report) => {
                        stored += (report.newly_stored.is_some() as usize + report.bodies_attached)
                            as u64;
                    }
                    Err(CoreError::Storage(e)) => return Err(CoreError::Storage(e)),
                    Err(_) => rejected += 1,
                }
            }
            Response::Ack { stored, rejected }
        }
        Request::GetBlob { hash } => Response::Blob {
            bytes: db.blobs().get(&hash),
        },
    })
}

/// The blob wants one log implies: media its live claims pin that this
/// database doesn't hold, capped at [`BATCH`](super::protocol::BATCH).
/// Blob transfer is pull-only — this is what an eager fetcher (a relay
/// keeping itself stocked, a p2p pipe that opted in) walks to issue its
/// `GetBlob`s after claims land.
pub fn log_wants(db: &Database, log: &crate::LogId) -> Vec<crate::BlobHash> {
    db.missing_blobs()
        .into_iter()
        .filter(|blob| {
            db.claims()
                .blob_referrers(&blob.hash)
                .iter()
                .filter_map(|id| db.claims().get(id))
                .any(|c| c.header.log_id == *log)
        })
        .map(|blob| blob.hash)
        .take(super::protocol::BATCH as usize)
        .collect()
}
