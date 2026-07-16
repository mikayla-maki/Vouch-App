use crate::Error as CoreError;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// A failure from the engine underneath — for a session this is always
    /// `Storage` (verification failures of peer-served artifacts are
    /// counted in the report, never fatal: a misbehaving peer must not be
    /// able to abort your sync).
    #[error(transparent)]
    Core(#[from] CoreError),
    /// The peer answered with the wrong response type, or the driver fed a
    /// response no request is outstanding for. The session is done; start
    /// a fresh one — cursors only ever advance after successful ingest, so
    /// nothing is lost.
    #[error("protocol violation: {0}")]
    Protocol(String),
    /// The cursor store failed. Same posture as `Core(Storage)`: local
    /// trouble, abort loudly.
    #[error("sync state storage: {0}")]
    State(String),
}
