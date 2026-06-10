use std::fmt;

use ed25519_dalek::VerifyingKey;

use crate::error::Error;

/// A log's identity: the Ed25519 public key itself.
///
/// There is no separate user concept — one log, one keypair, one writer.
/// You subscribe to logs; merging your subscribed logs gives you your
/// [`Database`](crate::Database).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LogId(pub [u8; 32]);

impl LogId {
    /// Interpret the id as a verifying key. Fails if the bytes are not a
    /// valid Ed25519 point (a malformed or hostile id).
    pub fn verifying_key(&self) -> Result<VerifyingKey, Error> {
        VerifyingKey::from_bytes(&self.0).map_err(|_| Error::InvalidLogId)
    }

    /// Short hex prefix for display ("a1b2c3d4…").
    pub fn short(&self) -> String {
        let mut s = String::with_capacity(9);
        for b in &self.0[..4] {
            s.push_str(&format!("{b:02x}"));
        }
        s.push('…');
        s
    }
}

impl fmt::Debug for LogId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "LogId({})", self.short())
    }
}

impl fmt::Display for LogId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}
