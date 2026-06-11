//! The dynamic value model.
//!
//! A claim body is a freeform CBOR map. Links are *values*, not fields: the
//! well-known tagged types ([`ClaimRef`], `Embed`, and [`BlobRef`]) may
//! appear anywhere in a body — a top-level field, a list entry, a span
//! target inside rich text. The store indexes them by walking the value
//! tree.

use std::collections::BTreeMap;
use std::fmt;

use crate::claim::SignedEvent;
use crate::keys::LogId;

/// CBOR tag number for a [`ClaimRef`] value.
///
/// From the IANA first-come-first-served tag space (>= 32768); registration
/// can happen if this format ever matters to anyone else.
pub const TAG_CLAIM_REF: u64 = 33001;

/// CBOR tag number for an `Embed` value (a carried [`SignedEvent`]).
pub const TAG_EMBED: u64 = 33002;

/// CBOR tag number for a [`BlobRef`] value.
pub const TAG_BLOB_REF: u64 = 33003;

/// The identity of a claim: the BLAKE3 hash of its canonical header bytes.
///
/// Content-addressed identity is what makes forks a non-concept: two
/// different claims are just two different claims, with nothing to collide.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimHash(pub [u8; 32]);

impl ClaimHash {
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

impl fmt::Debug for ClaimHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ClaimHash({})", self.short())
    }
}

impl fmt::Display for ClaimHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// A reference to a claim: which log it lives in (so it's locatable, not
/// just nameable) plus its content hash.
///
/// Wire form: tag 33001 wrapping `[bytes-32 log_id, bytes-32 hash]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ClaimRef {
    pub log_id: LogId,
    pub hash: ClaimHash,
}

/// The identity of a blob: the BLAKE3 hash of its bytes.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BlobHash(pub [u8; 32]);

impl BlobHash {
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

impl fmt::Debug for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BlobHash({})", self.short())
    }
}

impl fmt::Display for BlobHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

/// A reference to bulk bytes (an image, any media) pinned by hash from a
/// claim body. The bytes live outside the claim — in a content-addressed
/// blob store, fetched lazily from any pipe — so logs stay small while the
/// signature still transitively covers the media (header pins body, body
/// pins blob). `size` and `mime` let a UI render placeholders and budget
/// fetches before holding a single byte.
///
/// Wire form: tag 33003 wrapping `[bytes-32 hash, uint size, text mime]`.
/// `size` is a `u64` in memory but bounded to the decodable range
/// `[0, i64::MAX]` on the wire (the value model carries `i64`); the writer
/// refuses to sign a larger one. A blob that big is unrepresentable here by
/// design — real media is many orders of magnitude smaller.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BlobRef {
    pub hash: BlobHash,
    pub size: u64,
    pub mime: String,
}

/// A dynamically-typed body value.
///
/// This is deliberately a *subset* of CBOR: no floats, no indefinite-length
/// items, text map keys only. Anything a conformant Vouch implementation can
/// produce round-trips byte-identically through [`crate::cbor`].
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Bytes(Vec<u8>),
    Text(String),
    Array(Vec<Value>),
    Map(BTreeMap<String, Value>),
    /// A reference to another claim. Legal anywhere in a body.
    ClaimRef(ClaimRef),
    /// Another author's signed claim, carried whole. Verified by the engine.
    Embed(Box<SignedEvent>),
    /// Bulk bytes pinned by hash, stored and fetched outside the claim.
    BlobRef(BlobRef),
    /// An unrecognized CBOR tag, retained so unknown vocabulary re-encodes
    /// byte-identically (never drop signed data).
    Tagged(u64, Box<Value>),
}

/// One step of a path into a body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSeg {
    Key(String),
    Index(usize),
}

/// Where in a body a value was found.
pub type Path = Vec<PathSeg>;

/// Every `ClaimRef` found in a body, with where it was found.
pub type FoundRefs = Vec<(Path, ClaimRef)>;

/// Every embedded `SignedEvent` found in a body, with where it was found.
pub type FoundEmbeds = Vec<(Path, SignedEvent)>;

/// Every [`BlobRef`] found in a body, with where it was found.
pub type FoundBlobs = Vec<(Path, BlobRef)>;

impl Value {
    /// Convenience constructor for text values.
    pub fn text(s: impl Into<String>) -> Value {
        Value::Text(s.into())
    }

    /// Convenience constructor for maps.
    pub fn map<K: Into<String>>(entries: impl IntoIterator<Item = (K, Value)>) -> Value {
        Value::Map(entries.into_iter().map(|(k, v)| (k.into(), v)).collect())
    }

    /// Convenience constructor for arrays.
    pub fn array(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Array(items.into_iter().collect())
    }

    /// True if this value is a map (the only legal body shape).
    pub fn is_map(&self) -> bool {
        matches!(self, Value::Map(_))
    }

    /// Walk the tree, collecting every [`ClaimRef`], `Embed`, and
    /// [`BlobRef`] with the path where it was found.
    ///
    /// Walks *into* arrays, maps, and unknown tagged values (unknown
    /// vocabulary still gets its links indexed), but not into embed bytes —
    /// embedded claims are decoded and walked by the store when ingested.
    pub fn collect_refs(&self) -> (FoundRefs, FoundEmbeds, FoundBlobs) {
        let mut refs = Vec::new();
        let mut embeds = Vec::new();
        let mut blobs = Vec::new();
        let mut path = Vec::new();
        collect(self, &mut path, &mut refs, &mut embeds, &mut blobs);
        (refs, embeds, blobs)
    }

    /// The last `Key` segment of a path: the conventional "rel" of a link
    /// found there (e.g. a ref under `"about"` is an about-link).
    pub fn rel_of(path: &Path) -> Option<&str> {
        path.iter().rev().find_map(|seg| match seg {
            PathSeg::Key(k) => Some(k.as_str()),
            PathSeg::Index(_) => None,
        })
    }
}

fn collect(
    value: &Value,
    path: &mut Path,
    refs: &mut Vec<(Path, ClaimRef)>,
    embeds: &mut Vec<(Path, SignedEvent)>,
    blobs: &mut Vec<(Path, BlobRef)>,
) {
    match value {
        Value::ClaimRef(r) => refs.push((path.clone(), *r)),
        Value::Embed(e) => embeds.push((path.clone(), (**e).clone())),
        Value::BlobRef(b) => blobs.push((path.clone(), b.clone())),
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                path.push(PathSeg::Index(i));
                collect(item, path, refs, embeds, blobs);
                path.pop();
            }
        }
        Value::Map(entries) => {
            for (k, v) in entries {
                path.push(PathSeg::Key(k.clone()));
                collect(v, path, refs, embeds, blobs);
                path.pop();
            }
        }
        Value::Tagged(_, inner) => collect(inner, path, refs, embeds, blobs),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Bytes(_) | Value::Text(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_ref(n: u8, h: u8) -> ClaimRef {
        ClaimRef {
            log_id: LogId([n; 32]),
            hash: ClaimHash([h; 32]),
        }
    }

    #[test]
    fn collects_refs_at_any_depth() {
        let body = Value::map([
            ("about", Value::ClaimRef(dummy_ref(1, 7))),
            (
                "body",
                Value::array([
                    Value::text("see also "),
                    Value::Tagged(99999, Box::new(Value::ClaimRef(dummy_ref(2, 3)))),
                ]),
            ),
        ]);
        let (refs, embeds, blobs) = body.collect_refs();
        assert_eq!(refs.len(), 2);
        assert!(embeds.is_empty());
        assert!(blobs.is_empty());

        let rels: Vec<_> = refs.iter().map(|(p, _)| Value::rel_of(p)).collect();
        assert_eq!(rels, vec![Some("about"), Some("body")]);
        assert_eq!(refs[0].1, dummy_ref(1, 7));
        assert_eq!(refs[1].1, dummy_ref(2, 3));
    }
}
