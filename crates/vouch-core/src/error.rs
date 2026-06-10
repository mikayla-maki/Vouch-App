use crate::keys::LogId;

/// Errors produced by vouch-core.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input is not well-formed canonical CBOR.
    #[error("malformed or non-canonical CBOR at byte {offset}: {reason}")]
    Cbor { offset: usize, reason: &'static str },

    /// A claim body must be a CBOR map.
    #[error("claim body must be a CBOR map")]
    BodyNotMap,

    /// The body bytes don't hash to the header's `body_hash`.
    #[error("body bytes do not match the header's body hash")]
    BodyHashMismatch,

    /// The encoded body exceeds [`MAX_BODY_SIZE`](crate::claim::MAX_BODY_SIZE).
    #[error("claim body is {0} bytes; the maximum is 65536")]
    BodyTooLarge(usize),

    /// Fetched blob bytes don't hash to the reference that pinned them.
    #[error("blob bytes do not match the referenced blob hash")]
    BlobHashMismatch,

    /// The claimed log id is not a valid Ed25519 public key.
    #[error("invalid log id: not a valid Ed25519 public key")]
    InvalidLogId,

    /// A claim was requested for a log this database holds no writer for.
    #[error("no writer for log {0}; claims can only be minted into owned logs")]
    NotOurLog(LogId),

    /// Signature verification failed.
    #[error("signature verification failed for a claim by {log_id}")]
    BadSignature { log_id: LogId },

    /// The wire version is newer than this implementation understands.
    #[error("unsupported wire version {0}")]
    UnsupportedVersion(u16),

    /// Embeds nested deeper than the recursion cap.
    #[error("embed nesting exceeds maximum depth")]
    EmbedTooDeep,

    /// OS randomness was unavailable (key generation only).
    #[error("system randomness unavailable")]
    Randomness,
}
