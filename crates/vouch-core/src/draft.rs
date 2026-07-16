//! A claim under construction: body fields plus the media that belongs to
//! it, minted in one move.
//!
//! The two-step alternative (store a blob, get a ref, remember to pin it)
//! had two failure modes: an attached blob whose claim never materializes
//! (an orphan until GC), and a body pinning a ref the app dropped. A
//! [`Draft`] carries its attachments, so [`Database::compose`] can store
//! the blobs and sign the body as one operation — blob-before-claim
//! ordering stops being a discipline and becomes a fact of the API.
//!
//! [`Database::compose`]: crate::Database::compose

use crate::claim::SignedEvent;
use crate::value::{ClaimRef, Value};

/// One attachment: where it goes in the body, its bytes, its mime type.
pub(crate) struct Attachment {
    pub key: String,
    pub bytes: Vec<u8>,
    pub mime: String,
}

/// A claim being written. Build it up, hand it to
/// [`Database::compose`](crate::Database::compose) (or `Peer::claim`),
/// get back the signed event.
///
/// ```ignore
/// Draft::new("rec")
///     .at(now_ms)
///     .text("subject", "Joe's Pizza")
///     .attach("photo", jpeg_bytes, "image/jpeg")
///     .embed("original", their_event)   // a re-vouch carries the chain
/// ```
pub struct Draft {
    pub(crate) fields: Vec<(String, Value)>,
    pub(crate) attachments: Vec<Attachment>,
}

impl Draft {
    /// Start a draft of the given vocabulary type (the body's `type`
    /// field).
    pub fn new(claim_type: impl Into<String>) -> Draft {
        Draft {
            fields: vec![("type".into(), Value::Text(claim_type.into()))],
            attachments: Vec::new(),
        }
    }

    /// The claimed time (the engine-recognized `at` field, Unix ms):
    /// display ordering, author-asserted, never trusted for anything else.
    pub fn at(self, unix_ms: i64) -> Draft {
        self.field("at", Value::Int(unix_ms))
    }

    pub fn text(self, key: impl Into<String>, text: impl Into<String>) -> Draft {
        self.field(key, Value::Text(text.into()))
    }

    pub fn int(self, key: impl Into<String>, n: i64) -> Draft {
        self.field(key, Value::Int(n))
    }

    /// A link to another claim (a backlink-indexed edge).
    pub fn reference(self, key: impl Into<String>, target: ClaimRef) -> Draft {
        self.field(key, Value::ClaimRef(target))
    }

    /// Quote another claim whole: the event travels inside this one and
    /// re-verifies wherever it lands — the only sanctioned way third-party
    /// content crosses the network through you.
    pub fn embed(self, key: impl Into<String>, event: SignedEvent) -> Draft {
        self.field(key, Value::Embed(Box::new(event)))
    }

    /// Any value at any key (escape hatch for vocabulary the named helpers
    /// don't cover).
    pub fn field(mut self, key: impl Into<String>, value: Value) -> Draft {
        self.fields.push((key.into(), value));
        self
    }

    /// Media that belongs to this claim: stored as a content-addressed
    /// blob and pinned at `key` as a [`BlobRef`](crate::BlobRef) when the
    /// draft is composed.
    pub fn attach(
        mut self,
        key: impl Into<String>,
        bytes: Vec<u8>,
        mime: impl Into<String>,
    ) -> Draft {
        self.attachments.push(Attachment {
            key: key.into(),
            bytes,
            mime: mime.into(),
        });
        self
    }
}
