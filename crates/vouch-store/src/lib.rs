//! Durable backends for vouch-core's storage seams.
//!
//! vouch-core owns every invariant (convergence, monotone redaction, body
//! fill-in, verify-on-arrival); this crate owns none. It provides dumb
//! storage that the core's logic drives:
//!
//! - [`SqliteClaimStorage`] — claims, backlinks, blob referrers, and
//!   redactions as SQLite tables. This *is* persistence for claims: every
//!   `put_claim` is durable, redaction's body-drop is a column update (so
//!   cooperative deletion reaches the disk with no compaction machinery),
//!   and reopening the file is reopening the database.
//! - [`FileBlobStorage`] — media as content-addressed files, one per blob,
//!   named by hash. Bytes verify against their name on read, so a corrupt
//!   file degrades to a missing blob (re-fetched via the want-list), never
//!   to corrupt media.
//! - [`open`] — the convenience wiring: a directory becomes a
//!   [`Database`] (claims.db + blobs/). Where storage lives is decided
//!   here, upstream of the engine, never inside it.
//!
//! Writers are NOT persisted — keys belong to the OS keychain / mnemonic.
//! Re-adding a writer on open is the caller's job and needs nothing but the
//! key: a writer carries no position, so there is no counter to restore.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};
use vouch_core::value::{BlobHash, ClaimHash};
use vouch_core::{
    BlobStorage, ClaimStorage, Database, Error, EventHeader, LogId, Provenance, Signature,
    SignedEvent, StoredClaim,
};

/// Open (or create) a durable [`Database`] in `dir`: SQLite claim storage
/// at `dir/claims.db`, file blob storage under `dir/blobs/`.
pub fn open(dir: impl AsRef<Path>) -> Result<Database, Error> {
    let dir = dir.as_ref();
    let claims = SqliteClaimStorage::open(dir.join("claims.db"))?;
    let blobs = FileBlobStorage::open(dir.join("blobs"))?;
    Ok(Database::with_stores(Box::new(claims), Box::new(blobs)))
}

fn storage_err(e: impl std::fmt::Display) -> Error {
    Error::Storage(e.to_string())
}

// ---------------------------------------------------------------------------
// SQLite claim storage
// ---------------------------------------------------------------------------

/// [`ClaimStorage`] over SQLite: four tables, no logic.
pub struct SqliteClaimStorage {
    conn: Connection,
}

impl SqliteClaimStorage {
    pub fn open(path: impl AsRef<Path>) -> Result<SqliteClaimStorage, Error> {
        let conn = Connection::open(path).map_err(storage_err)?;
        Self::init(conn)
    }

    /// An in-memory SQLite database — the same backend code path with no
    /// file, useful for tests of this crate itself.
    pub fn open_in_memory() -> Result<SqliteClaimStorage, Error> {
        let conn = Connection::open_in_memory().map_err(storage_err)?;
        Self::init(conn)
    }

    fn init(conn: Connection) -> Result<SqliteClaimStorage, Error> {
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage_err)?;
        // FULL, not NORMAL: a committed ingest must survive power loss, not
        // just a process/OS crash. Under WAL+NORMAL the latest committed
        // transactions can be lost on power loss, which would (a) lose a
        // user's own just-authored claims and (b) silently rewind this
        // store's arrival count below a peer's cursor — the
        // relay-restored-from-stale-backup hazard. FULL costs an fsync per
        // commit; at this data scale that's imperceptible for authoring, and
        // bulk backfill can batch many claims per transaction later.
        conn.pragma_update(None, "synchronous", "FULL")
            .map_err(storage_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS claims (
                 id         BLOB PRIMARY KEY,
                 log_id     BLOB NOT NULL,
                 arrival    INTEGER NOT NULL,
                 received_at INTEGER NOT NULL,
                 header     BLOB NOT NULL,
                 signature  BLOB NOT NULL,
                 body       BLOB,
                 provenance INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS claims_by_log ON claims(log_id, arrival);
             CREATE TABLE IF NOT EXISTS backlinks (
                 target BLOB NOT NULL,
                 source BLOB NOT NULL,
                 PRIMARY KEY (target, source)
             );
             CREATE TABLE IF NOT EXISTS blob_referrers (
                 blob   BLOB NOT NULL,
                 source BLOB NOT NULL,
                 PRIMARY KEY (blob, source)
             );
             CREATE TABLE IF NOT EXISTS redactions (
                 target      BLOB PRIMARY KEY,
                 redacted_by BLOB NOT NULL
             );",
        )
        .map_err(storage_err)?;
        Ok(SqliteClaimStorage { conn })
    }
}

/// Rebuild a [`StoredClaim`] from its row. The decoded views (header, body,
/// refs) are derived from the stored canonical bytes — the row stores
/// artifacts, not interpretations, exactly like the wire.
#[allow(clippy::too_many_arguments)]
fn row_to_claim(
    header_bytes: Vec<u8>,
    signature: Vec<u8>,
    body_bytes: Option<Vec<u8>>,
    provenance: i64,
    arrival: i64,
    received_at: i64,
) -> Result<StoredClaim, Error> {
    let header = EventHeader::decode(&header_bytes)?;
    let signature = Signature::from_slice(&signature)
        .map_err(|_| Error::Storage("stored signature is not 64 bytes".into()))?;
    let body = match &body_bytes {
        Some(b) => Some(vouch_core::cbor::from_bytes(b)?),
        None => None,
    };
    let (refs, _embeds, blobs) = match &body {
        Some(b) => b.collect_refs(),
        None => Default::default(),
    };
    Ok(StoredClaim {
        signed: SignedEvent {
            header_bytes,
            signature,
            body_bytes,
        },
        header,
        body,
        refs,
        blobs,
        provenance: if provenance == 0 {
            Provenance::Direct
        } else {
            Provenance::Embedded
        },
        arrival: arrival as u64,
        received_at,
    })
}

type ClaimRow = (Vec<u8>, Vec<u8>, Option<Vec<u8>>, i64, i64, i64);

impl ClaimStorage for SqliteClaimStorage {
    fn get_claim(&self, id: &ClaimHash) -> Result<Option<StoredClaim>, Error> {
        let row: Option<ClaimRow> = self
            .conn
            .query_row(
                "SELECT header, signature, body, provenance, arrival, received_at
                 FROM claims WHERE id = ?1",
                params![id.0.as_slice()],
                |r| {
                    Ok((
                        r.get(0)?,
                        r.get(1)?,
                        r.get(2)?,
                        r.get(3)?,
                        r.get(4)?,
                        r.get(5)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err)?;
        row.map(|(h, s, b, p, a, t)| row_to_claim(h, s, b, p, a, t))
            .transpose()
    }

    fn put_claim(&mut self, claim: StoredClaim) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO claims
                     (id, log_id, arrival, received_at, header, signature, body, provenance)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    claim.signed.id().0.as_slice(),
                    claim.header.log_id.0.as_slice(),
                    claim.arrival as i64,
                    claim.received_at,
                    claim.signed.header_bytes,
                    claim.signed.signature.to_bytes().as_slice(),
                    claim.signed.body_bytes,
                    matches!(claim.provenance, Provenance::Embedded) as i64,
                ],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn claim_count(&self) -> Result<usize, Error> {
        let n: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM claims", [], |r| r.get(0))
            .map_err(storage_err)?;
        Ok(n as usize)
    }

    fn scan_claims(&self, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT header, signature, body, provenance, arrival, received_at FROM claims")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(storage_err)?;
        for row in rows {
            let (h, s, b, p, a, t) = row.map_err(storage_err)?;
            visit(&row_to_claim(h, s, b, p, a, t)?);
        }
        Ok(())
    }

    fn scan_log(&self, log: &LogId, visit: &mut dyn FnMut(&StoredClaim)) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT header, signature, body, provenance, arrival, received_at
                 FROM claims WHERE log_id = ?1",
            )
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![log.0.as_slice()], |r| {
                Ok((
                    r.get::<_, Vec<u8>>(0)?,
                    r.get::<_, Vec<u8>>(1)?,
                    r.get::<_, Option<Vec<u8>>>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                ))
            })
            .map_err(storage_err)?;
        for row in rows {
            let (h, s, b, p, a, t) = row.map_err(storage_err)?;
            visit(&row_to_claim(h, s, b, p, a, t)?);
        }
        Ok(())
    }

    fn add_backlink(&mut self, target: ClaimHash, source: ClaimHash) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO backlinks (target, source) VALUES (?1, ?2)",
                params![target.0.as_slice(), source.0.as_slice()],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn remove_backlink(&mut self, target: &ClaimHash, source: &ClaimHash) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM backlinks WHERE target = ?1 AND source = ?2",
                params![target.0.as_slice(), source.0.as_slice()],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn backlinks(&self, target: &ClaimHash) -> Result<Vec<ClaimHash>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT source FROM backlinks WHERE target = ?1 ORDER BY source")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![target.0.as_slice()], |r| r.get::<_, [u8; 32]>(0))
            .map_err(storage_err)?;
        rows.map(|r| r.map(ClaimHash).map_err(storage_err))
            .collect()
    }

    fn add_blob_referrer(&mut self, blob: BlobHash, source: ClaimHash) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO blob_referrers (blob, source) VALUES (?1, ?2)",
                params![blob.0.as_slice(), source.0.as_slice()],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn remove_blob_referrer(&mut self, blob: &BlobHash, source: &ClaimHash) -> Result<(), Error> {
        self.conn
            .execute(
                "DELETE FROM blob_referrers WHERE blob = ?1 AND source = ?2",
                params![blob.0.as_slice(), source.0.as_slice()],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn blob_referrers(&self, blob: &BlobHash) -> Result<Vec<ClaimHash>, Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT source FROM blob_referrers WHERE blob = ?1 ORDER BY source")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map(params![blob.0.as_slice()], |r| r.get::<_, [u8; 32]>(0))
            .map_err(storage_err)?;
        rows.map(|r| r.map(ClaimHash).map_err(storage_err))
            .collect()
    }

    fn redaction(&self, target: &ClaimHash) -> Result<Option<ClaimHash>, Error> {
        self.conn
            .query_row(
                "SELECT redacted_by FROM redactions WHERE target = ?1",
                params![target.0.as_slice()],
                |r| r.get::<_, [u8; 32]>(0),
            )
            .optional()
            .map_err(storage_err)
            .map(|o| o.map(ClaimHash))
    }

    fn set_redaction(&mut self, target: ClaimHash, by: ClaimHash) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT OR REPLACE INTO redactions (target, redacted_by) VALUES (?1, ?2)",
                params![target.0.as_slice(), by.0.as_slice()],
            )
            .map_err(storage_err)?;
        Ok(())
    }

    fn scan_redactions(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT target, redacted_by FROM redactions")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, [u8; 32]>(0)?, r.get::<_, [u8; 32]>(1)?))
            })
            .map_err(storage_err)?;
        for row in rows {
            let (t, by) = row.map_err(storage_err)?;
            visit(ClaimHash(t), ClaimHash(by));
        }
        Ok(())
    }

    fn scan_backlinks(&self, visit: &mut dyn FnMut(ClaimHash, ClaimHash)) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT target, source FROM backlinks")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, [u8; 32]>(0)?, r.get::<_, [u8; 32]>(1)?))
            })
            .map_err(storage_err)?;
        for row in rows {
            let (t, s) = row.map_err(storage_err)?;
            visit(ClaimHash(t), ClaimHash(s));
        }
        Ok(())
    }

    fn scan_blob_referrers(&self, visit: &mut dyn FnMut(BlobHash, ClaimHash)) -> Result<(), Error> {
        let mut stmt = self
            .conn
            .prepare("SELECT blob, source FROM blob_referrers")
            .map_err(storage_err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, [u8; 32]>(0)?, r.get::<_, [u8; 32]>(1)?))
            })
            .map_err(storage_err)?;
        for row in rows {
            let (b, s) = row.map_err(storage_err)?;
            visit(BlobHash(b), ClaimHash(s));
        }
        Ok(())
    }

    // Real transactions: a crash, kill, or power loss mid-ingest leaves no
    // trace — WAL discards the uncommitted transaction on next open.
    fn begin(&mut self) -> Result<(), Error> {
        self.conn
            .execute_batch("BEGIN IMMEDIATE")
            .map_err(storage_err)
    }

    fn commit(&mut self) -> Result<(), Error> {
        self.conn.execute_batch("COMMIT").map_err(storage_err)
    }

    fn rollback(&mut self) -> Result<(), Error> {
        self.conn.execute_batch("ROLLBACK").map_err(storage_err)
    }
}

// ---------------------------------------------------------------------------
// File blob storage
// ---------------------------------------------------------------------------

/// [`BlobStorage`] over a directory: one file per blob, named by hash
/// hex. Reads verify bytes against the filename — a corrupt file is
/// evicted and reads as missing, so it heals through the want-list like
/// any absence. (This check is storage integrity — "is my disk lying to
/// me" — distinct from the engine's verify-on-arrival, which guards the
/// network and lives in core where backends can't touch it.)
pub struct FileBlobStorage {
    dir: PathBuf,
}

impl FileBlobStorage {
    pub fn open(dir: impl Into<PathBuf>) -> Result<FileBlobStorage, Error> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir).map_err(storage_err)?;
        // Sweep orphaned temp files from inserts interrupted by a crash —
        // they're never valid blob names (so reads ignore them) but would
        // otherwise accumulate forever.
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if entry
                    .file_name()
                    .to_str()
                    .is_some_and(|n| n.ends_with(".tmp"))
                {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
        Ok(FileBlobStorage { dir })
    }

    fn path(&self, hash: &BlobHash) -> PathBuf {
        self.dir.join(hash.to_string())
    }
}

fn parse_hash(name: &str) -> Option<BlobHash> {
    if name.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&name[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(BlobHash(out))
}

impl BlobStorage for FileBlobStorage {
    fn insert(&mut self, hash: BlobHash, bytes: Vec<u8>) -> Result<(), Error> {
        // Write-then-rename so a crash never leaves a half-written blob
        // under a valid name.
        let tmp = self.dir.join(format!("{hash}.tmp"));
        std::fs::write(&tmp, &bytes).map_err(storage_err)?;
        std::fs::rename(&tmp, self.path(&hash)).map_err(storage_err)?;
        Ok(())
    }

    fn get(&self, hash: &BlobHash) -> Option<Vec<u8>> {
        let bytes = std::fs::read(self.path(hash)).ok()?;
        if *blake3::hash(&bytes).as_bytes() != hash.0 {
            // Corrupt storage degrades to absence: evict, let the
            // want-list re-fetch from any pipe.
            let _ = std::fs::remove_file(self.path(hash));
            return None;
        }
        Some(bytes)
    }

    fn contains(&self, hash: &BlobHash) -> bool {
        self.path(hash).exists()
    }

    fn remove(&mut self, hash: &BlobHash) -> Result<bool, Error> {
        match std::fs::remove_file(self.path(hash)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(storage_err(e)),
        }
    }

    fn hashes(&self) -> Vec<BlobHash> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| parse_hash(e.file_name().to_str()?))
            .collect()
    }
}
