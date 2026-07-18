//! The dynamic value model.
//!
//! A claim body is a freeform CBOR map. Links are *values*, not fields: the
//! well-known tagged types ([`ClaimRef`], `Embed`, and [`BlobRef`]) may
//! appear anywhere in a body — a top-level field, a list entry, a span
//! target inside rich text. The store indexes them by walking the value
//! tree.

use std::collections::BTreeMap;
use std::fmt;

use crate::claim::{Claim, SignedEvent};
use crate::keys::LogId;

/// How deep embedded claims may nest inside one another.
///
/// Each level of a re-vouch chain nests the previous event, so this bounds
/// the longest endorsement chain indexed in one artifact. ~33 bits suffice
/// to individually identify every human, so even a maximally viral chain of
/// re-vouches stays well under 64; the cap is pure headroom over reality
/// while still bounding adversarial verification work (one signature check
/// per level). An embed past the cap is skipped — left as opaque signed
/// bytes, neither verified nor indexed.
pub const MAX_EMBED_DEPTH: usize = 64;

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
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
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
#[cfg_attr(feature = "wire", derive(serde::Serialize, serde::Deserialize))]
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

/// Every outgoing edge of a body, collected *through* its embeds — what the
/// store indexes for a claim. See [`Value::collect_edges`].
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Edges {
    /// Claim edges: every [`ClaimRef`] in the body, plus one edge per
    /// verified embed (a quote is the strongest form of reference, so it
    /// backlinks like one), plus every ref inside those embeds.
    pub refs: FoundRefs,
    /// Blob edges: every [`BlobRef`] in the body, including those inside
    /// verified embeds — a quote that shows a photo pins that photo.
    pub blobs: FoundBlobs,
    /// Embeds whose interiors were NOT indexed: signature or body-hash
    /// verification failed, or nesting exceeded [`MAX_EMBED_DEPTH`]. The
    /// container is unaffected (its author signed the garbage; that is
    /// recorded, not endorsed) — these embeds are simply not edges.
    pub skipped: usize,
}

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
    /// this is the *shallow* walk. The store indexes through embeds with
    /// [`collect_edges`](Self::collect_edges); UI code recurses with
    /// [`embedded_claims`](Self::embedded_claims).
    pub fn collect_refs(&self) -> (FoundRefs, FoundEmbeds, FoundBlobs) {
        let mut refs = Vec::new();
        let mut embeds = Vec::new();
        let mut blobs = Vec::new();
        let mut path = Vec::new();
        collect(self, &mut path, &mut refs, &mut embeds, &mut blobs);
        (refs, embeds, blobs)
    }

    /// Every outgoing edge of this body, collected *through* its embeds:
    /// the deep walk the store indexes.
    ///
    /// An embedded claim is content, not a row — it is part of the speech
    /// that quotes it, so its edges belong to the quoting claim. Each embed
    /// is verified (signature, body hash) in place; a verified embed
    /// contributes one claim edge for itself (path: where the embed sits)
    /// plus everything inside it, with interior paths appended to the
    /// embed's path. An embed that fails verification, or nests past
    /// [`MAX_EMBED_DEPTH`], is skipped whole and counted — opaque signed
    /// bytes, not edges.
    ///
    /// Deterministic: the same body always yields the same edges, in tree
    /// order — so a backend can recompute them from stored bytes and fsck
    /// can compare.
    pub fn collect_edges(&self) -> Edges {
        let mut edges = Edges::default();
        let mut path = Vec::new();
        collect_deep(self, &mut path, 0, &mut edges);
        edges
    }

    /// The verified claims embedded in this body, shallow: each is decoded
    /// and verified (signature, body hash), with the path where it sits.
    /// Embeds that fail verification are omitted — unrenderable garbage the
    /// container's author signed. A UI recurses by calling this on each
    /// returned claim's body; identity is `claim.header.id()`.
    pub fn embedded_claims(&self) -> Vec<(Path, Claim)> {
        let (_, embeds, _) = self.collect_refs();
        embeds
            .into_iter()
            .filter_map(|(path, e)| e.verify().ok().map(|c| (path, c)))
            .collect()
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

/// The deep walk behind [`Value::collect_edges`]. `depth` counts embed
/// layers entered so far; entering another requires `depth < MAX_EMBED_DEPTH`
/// (one signature check per layer is the work being bounded).
fn collect_deep(value: &Value, path: &mut Path, depth: usize, edges: &mut Edges) {
    match value {
        Value::ClaimRef(r) => edges.refs.push((path.clone(), *r)),
        Value::BlobRef(b) => edges.blobs.push((path.clone(), b.clone())),
        Value::Embed(e) => {
            if depth >= MAX_EMBED_DEPTH {
                edges.skipped += 1;
                return;
            }
            match e.verify() {
                Ok(claim) => {
                    edges.refs.push((
                        path.clone(),
                        ClaimRef {
                            log_id: claim.header.log_id,
                            hash: e.id(),
                        },
                    ));
                    if let Some(body) = &claim.body {
                        collect_deep(body, path, depth + 1, edges);
                    }
                }
                Err(_) => edges.skipped += 1,
            }
        }
        Value::Array(items) => {
            for (i, item) in items.iter().enumerate() {
                path.push(PathSeg::Index(i));
                collect_deep(item, path, depth, edges);
                path.pop();
            }
        }
        Value::Map(entries) => {
            for (k, v) in entries {
                path.push(PathSeg::Key(k.clone()));
                collect_deep(v, path, depth, edges);
                path.pop();
            }
        }
        Value::Tagged(_, inner) => collect_deep(inner, path, depth, edges),
        Value::Null | Value::Bool(_) | Value::Int(_) | Value::Bytes(_) | Value::Text(_) => {}
    }
}

/// A quick, no-ceremony way to pull typed fields out of a claim body —
/// the alternative to hand-writing a `match` per field, or to a
/// derive-macro schema system this crate doesn't have (and doesn't need
/// yet: bodies are deliberately schemaless, so there's no fixed type to
/// generate a decoder from — this just reads whichever of the expected
/// shape happens to be there).
#[derive(Clone, Copy)]
pub struct Fields<'a>(pub &'a BTreeMap<String, Value>);

impl<'a> Fields<'a> {
    pub fn of(map: &'a BTreeMap<String, Value>) -> Fields<'a> {
        Fields(map)
    }

    pub fn text(&self, key: &str) -> Option<&'a str> {
        match self.0.get(key) {
            Some(Value::Text(t)) => Some(t.as_str()),
            _ => None,
        }
    }

    pub fn int(&self, key: &str) -> Option<i64> {
        match self.0.get(key) {
            Some(Value::Int(n)) => Some(*n),
            _ => None,
        }
    }

    /// Every `ClaimRef` under `key`, whether it's a single ref or an array
    /// of them — the shape an "of"/"references" field takes in an `edit`
    /// or `comment` claim.
    pub fn refs(&self, key: &str) -> Vec<ClaimRef> {
        match self.0.get(key) {
            Some(Value::ClaimRef(r)) => vec![*r],
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| match v {
                    Value::ClaimRef(r) => Some(*r),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
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
