//! Deterministic CBOR (RFC 8949 §4.2 core deterministic encoding).
//!
//! Signatures and claim ids are computed over encoded bytes, so this module
//! is normative: every implementation must produce byte-identical encodings
//! for the same value. The encoder always emits canonical form; the decoder
//! *rejects* non-canonical input (non-minimal integer encodings, unsorted or
//! duplicate map keys, indefinite lengths), so a conformant claim is exactly
//! one byte string.
//!
//! Restrictions relative to full CBOR (by design, see the architecture doc):
//! no floats, no simple values beyond bool/null, text map keys only.

use std::collections::BTreeMap;

use ed25519_dalek::Signature;

use crate::claim::SignedEvent;
use crate::error::Error;
use crate::keys::LogId;
use crate::value::{
    BlobHash, BlobRef, ClaimHash, ClaimRef, TAG_BLOB_REF, TAG_CLAIM_REF, TAG_EMBED, Value,
};

/// Maximum nesting depth the decoder will follow.
const MAX_DEPTH: usize = 128;

// ---------------------------------------------------------------------------
// Encoding
// ---------------------------------------------------------------------------

/// Write a CBOR head: major type + minimally-encoded argument.
pub(crate) fn head(out: &mut Vec<u8>, major: u8, arg: u64) {
    let mt = major << 5;
    if arg < 24 {
        out.push(mt | arg as u8);
    } else if arg <= 0xff {
        out.push(mt | 24);
        out.push(arg as u8);
    } else if arg <= 0xffff {
        out.push(mt | 25);
        out.extend((arg as u16).to_be_bytes());
    } else if arg <= 0xffff_ffff {
        out.push(mt | 26);
        out.extend((arg as u32).to_be_bytes());
    } else {
        out.push(mt | 27);
        out.extend(arg.to_be_bytes());
    }
}

pub(crate) fn encode_int(out: &mut Vec<u8>, i: i64) {
    if i >= 0 {
        head(out, 0, i as u64);
    } else {
        head(out, 1, !(i as u64)); // -(i+1) without overflow on i64::MIN
    }
}

pub(crate) fn encode_claim_ref(out: &mut Vec<u8>, r: &ClaimRef) {
    head(out, 6, TAG_CLAIM_REF);
    head(out, 4, 2);
    head(out, 2, 32);
    out.extend_from_slice(&r.log_id.0);
    head(out, 2, 32);
    out.extend_from_slice(&r.hash.0);
}

pub(crate) fn encode_signed_event(out: &mut Vec<u8>, e: &SignedEvent) {
    head(out, 4, 3);
    head(out, 2, e.header_bytes.len() as u64);
    out.extend_from_slice(&e.header_bytes);
    let sig = e.signature.to_bytes();
    head(out, 2, sig.len() as u64);
    out.extend_from_slice(&sig);
    match &e.body_bytes {
        None => out.push(0xf6),
        Some(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
    }
}

/// Encode a value in canonical form.
pub fn encode_value(out: &mut Vec<u8>, value: &Value) {
    match value {
        Value::Null => out.push(0xf6),
        Value::Bool(false) => out.push(0xf4),
        Value::Bool(true) => out.push(0xf5),
        Value::Int(i) => encode_int(out, *i),
        Value::Bytes(b) => {
            head(out, 2, b.len() as u64);
            out.extend_from_slice(b);
        }
        Value::Text(s) => {
            head(out, 3, s.len() as u64);
            out.extend_from_slice(s.as_bytes());
        }
        Value::Array(items) => {
            head(out, 4, items.len() as u64);
            for item in items {
                encode_value(out, item);
            }
        }
        Value::Map(entries) => {
            // Canonical order is bytewise comparison of *encoded* keys
            // (which sorts shorter keys first), not plain string order —
            // so we can't rely on BTreeMap iteration order.
            head(out, 5, entries.len() as u64);
            let mut encoded: Vec<(Vec<u8>, &Value)> = entries
                .iter()
                .map(|(k, v)| {
                    let mut kb = Vec::with_capacity(k.len() + 3);
                    head(&mut kb, 3, k.len() as u64);
                    kb.extend_from_slice(k.as_bytes());
                    (kb, v)
                })
                .collect();
            encoded.sort_by(|a, b| a.0.cmp(&b.0));
            for (kb, v) in encoded {
                out.extend_from_slice(&kb);
                encode_value(out, v);
            }
        }
        Value::ClaimRef(r) => encode_claim_ref(out, r),
        Value::Embed(e) => {
            head(out, 6, TAG_EMBED);
            encode_signed_event(out, e);
        }
        Value::BlobRef(b) => {
            head(out, 6, TAG_BLOB_REF);
            head(out, 4, 3);
            head(out, 2, 32);
            out.extend_from_slice(&b.hash.0);
            head(out, 0, b.size);
            head(out, 3, b.mime.len() as u64);
            out.extend_from_slice(b.mime.as_bytes());
        }
        Value::Tagged(tag, inner) => {
            head(out, 6, *tag);
            encode_value(out, inner);
        }
    }
}

/// Encode a value to a fresh buffer.
pub fn to_bytes(value: &Value) -> Vec<u8> {
    let mut out = Vec::new();
    encode_value(&mut out, value);
    out
}

// ---------------------------------------------------------------------------
// Decoding
// ---------------------------------------------------------------------------

/// A strict decoder over a byte slice. Rejects non-canonical input.
pub(crate) struct Decoder<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(buf: &'a [u8]) -> Self {
        Decoder { buf, pos: 0 }
    }

    fn err<T>(&self, reason: &'static str) -> Result<T, Error> {
        Err(Error::Cbor {
            offset: self.pos,
            reason,
        })
    }

    fn byte(&mut self) -> Result<u8, Error> {
        let b = *self.buf.get(self.pos).ok_or(Error::Cbor {
            offset: self.pos,
            reason: "unexpected end of input",
        })?;
        self.pos += 1;
        Ok(b)
    }

    pub(crate) fn take(&mut self, n: u64) -> Result<&'a [u8], Error> {
        let n: usize = usize::try_from(n).map_err(|_| Error::Cbor {
            offset: self.pos,
            reason: "length overflows usize",
        })?;
        let end = self.pos.checked_add(n).ok_or(Error::Cbor {
            offset: self.pos,
            reason: "length overflows usize",
        })?;
        if end > self.buf.len() {
            return self.err("length exceeds remaining input");
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// Read a byte string that must be exactly 32 bytes.
    pub(crate) fn bytes32(&mut self, reason: &'static str) -> Result<[u8; 32], Error> {
        let n = self.expect(2, reason)?;
        if n != 32 {
            return self.err(reason);
        }
        Ok(self.take(32)?.try_into().unwrap())
    }

    /// True if the next byte is CBOR null.
    pub(crate) fn peek_null(&self) -> bool {
        self.buf.get(self.pos) == Some(&0xf6)
    }

    /// Consume a CBOR null (callers check [`Self::peek_null`] first).
    pub(crate) fn skip_null(&mut self) {
        debug_assert!(self.peek_null());
        self.pos += 1;
    }

    /// Read a head; enforce minimal-length argument encoding.
    pub(crate) fn head(&mut self) -> Result<(u8, u64), Error> {
        let b = self.byte()?;
        let major = b >> 5;
        let ai = b & 0x1f;
        if major == 7 && ai >= 24 {
            return self.err("floats and extended simple values are unsupported");
        }
        let arg = match ai {
            0..=23 => ai as u64,
            24 => {
                let v = self.byte()? as u64;
                if v < 24 {
                    return self.err("non-minimal integer encoding");
                }
                v
            }
            25 => {
                let v = u16::from_be_bytes(self.take(2)?.try_into().unwrap()) as u64;
                if v <= 0xff {
                    return self.err("non-minimal integer encoding");
                }
                v
            }
            26 => {
                let v = u32::from_be_bytes(self.take(4)?.try_into().unwrap()) as u64;
                if v <= 0xffff {
                    return self.err("non-minimal integer encoding");
                }
                v
            }
            27 => {
                let v = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
                if v <= 0xffff_ffff {
                    return self.err("non-minimal integer encoding");
                }
                v
            }
            _ => return self.err("indefinite lengths and reserved encodings are forbidden"),
        };
        Ok((major, arg))
    }

    /// Read a head and require a specific major type.
    pub(crate) fn expect(&mut self, major: u8, reason: &'static str) -> Result<u64, Error> {
        let (m, arg) = self.head()?;
        if m != major {
            return self.err(reason);
        }
        Ok(arg)
    }

    /// Read a signed integer (major 0 or 1) bounded to i64.
    pub(crate) fn int(&mut self, reason: &'static str) -> Result<i64, Error> {
        let (m, arg) = self.head()?;
        match m {
            0 => i64::try_from(arg).or_else(|_| self.err("integer out of range")),
            1 => {
                if arg > i64::MAX as u64 {
                    self.err("integer out of range")
                } else {
                    Ok(-1 - arg as i64)
                }
            }
            _ => self.err(reason),
        }
    }

    pub(crate) fn done(&self) -> Result<(), Error> {
        if self.pos != self.buf.len() {
            Err(Error::Cbor {
                offset: self.pos,
                reason: "trailing bytes after value",
            })
        } else {
            Ok(())
        }
    }

    pub(crate) fn value(&mut self, depth: usize) -> Result<Value, Error> {
        if depth > MAX_DEPTH {
            return self.err("nesting too deep");
        }
        let (major, arg) = self.head()?;
        match major {
            0 => i64::try_from(arg)
                .map(Value::Int)
                .or_else(|_| self.err("integer out of range")),
            1 => {
                if arg > i64::MAX as u64 {
                    self.err("integer out of range")
                } else {
                    Ok(Value::Int(-1 - arg as i64))
                }
            }
            2 => Ok(Value::Bytes(self.take(arg)?.to_vec())),
            3 => {
                let raw = self.take(arg)?;
                match std::str::from_utf8(raw) {
                    Ok(s) => Ok(Value::Text(s.to_owned())),
                    Err(_) => self.err("invalid UTF-8 in text string"),
                }
            }
            4 => {
                let mut items = Vec::new();
                for _ in 0..arg {
                    items.push(self.value(depth + 1)?);
                }
                Ok(Value::Array(items))
            }
            5 => {
                let mut entries = BTreeMap::new();
                let mut prev_key: Option<&'a [u8]> = None;
                for _ in 0..arg {
                    let key_start = self.pos;
                    let klen = self.expect(3, "map keys must be text")?;
                    let kraw = self.take(klen)?;
                    let key_encoded = &self.buf[key_start..self.pos];
                    if let Some(prev) = prev_key
                        && key_encoded <= prev
                    {
                        self.pos = key_start;
                        return self.err("map keys not in canonical order (or duplicate)");
                    }
                    prev_key = Some(key_encoded);
                    let key = match std::str::from_utf8(kraw) {
                        Ok(s) => s.to_owned(),
                        Err(_) => return self.err("invalid UTF-8 in map key"),
                    };
                    let v = self.value(depth + 1)?;
                    entries.insert(key, v);
                }
                Ok(Value::Map(entries))
            }
            6 => {
                let inner = self.value(depth + 1)?;
                Ok(interpret_tag(arg, inner))
            }
            7 => match arg {
                20 => Ok(Value::Bool(false)),
                21 => Ok(Value::Bool(true)),
                22 => Ok(Value::Null),
                _ => self.err("unsupported simple value"),
            },
            _ => unreachable!("major type is 3 bits"),
        }
    }
}

/// Recognize well-known tags; fall back to `Tagged` if the content doesn't
/// have the expected shape (lenient validation: the claim is preserved, the
/// malformed link just won't index).
fn interpret_tag(tag: u64, inner: Value) -> Value {
    match tag {
        TAG_CLAIM_REF => match claim_ref_from_value(&inner) {
            Some(r) => Value::ClaimRef(r),
            None => Value::Tagged(tag, Box::new(inner)),
        },
        TAG_EMBED => match signed_event_from_value(&inner) {
            Some(e) => Value::Embed(Box::new(e)),
            None => Value::Tagged(tag, Box::new(inner)),
        },
        TAG_BLOB_REF => match blob_ref_from_value(&inner) {
            Some(b) => Value::BlobRef(b),
            None => Value::Tagged(tag, Box::new(inner)),
        },
        _ => Value::Tagged(tag, Box::new(inner)),
    }
}

fn claim_ref_from_value(v: &Value) -> Option<ClaimRef> {
    let Value::Array(items) = v else { return None };
    let [Value::Bytes(db), Value::Bytes(hash)] = items.as_slice() else {
        return None;
    };
    let db: [u8; 32] = db.as_slice().try_into().ok()?;
    let hash: [u8; 32] = hash.as_slice().try_into().ok()?;
    Some(ClaimRef {
        log_id: LogId(db),
        hash: ClaimHash(hash),
    })
}

fn blob_ref_from_value(v: &Value) -> Option<BlobRef> {
    let Value::Array(items) = v else { return None };
    let [Value::Bytes(hash), Value::Int(size), Value::Text(mime)] = items.as_slice() else {
        return None;
    };
    let hash: [u8; 32] = hash.as_slice().try_into().ok()?;
    let size = u64::try_from(*size).ok()?;
    Some(BlobRef {
        hash: BlobHash(hash),
        size,
        mime: mime.clone(),
    })
}

fn signed_event_from_value(v: &Value) -> Option<SignedEvent> {
    let Value::Array(items) = v else { return None };
    let [Value::Bytes(header), Value::Bytes(sig), body] = items.as_slice() else {
        return None;
    };
    let signature = Signature::from_slice(sig).ok()?;
    let body_bytes = match body {
        Value::Null => None,
        Value::Bytes(b) => Some(b.clone()),
        _ => return None,
    };
    Some(SignedEvent {
        header_bytes: header.clone(),
        signature,
        body_bytes,
    })
}

/// Decode a single value; the input must be exactly one canonical value.
pub fn from_bytes(buf: &[u8]) -> Result<Value, Error> {
    let mut d = Decoder::new(buf);
    let v = d.value(0)?;
    d.done()?;
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: &Value) -> Value {
        let bytes = to_bytes(v);
        let back = from_bytes(&bytes).expect("decode");
        assert_eq!(to_bytes(&back), bytes, "re-encode must be byte-identical");
        back
    }

    #[test]
    fn scalar_roundtrips() {
        for v in [
            Value::Null,
            Value::Bool(true),
            Value::Bool(false),
            Value::Int(0),
            Value::Int(23),
            Value::Int(24),
            Value::Int(255),
            Value::Int(256),
            Value::Int(65535),
            Value::Int(65536),
            Value::Int(i64::MAX),
            Value::Int(-1),
            Value::Int(-24),
            Value::Int(-25),
            Value::Int(i64::MIN),
            Value::Bytes(vec![1, 2, 3]),
            Value::text("hello"),
        ] {
            assert_eq!(roundtrip(&v), v);
        }
    }

    #[test]
    fn canonical_map_order_is_length_first() {
        // "b" encodes as 61 62; "aa" as 62 61 61 — so "b" sorts first.
        let v = Value::map([("aa", Value::Int(1)), ("b", Value::Int(2))]);
        let bytes = to_bytes(&v);
        assert_eq!(bytes, vec![0xa2, 0x61, b'b', 0x02, 0x62, b'a', b'a', 0x01]);
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn rejects_non_minimal_int() {
        // 0x18 0x05 = uint 5 encoded with one-byte argument: non-minimal.
        assert!(from_bytes(&[0x18, 0x05]).is_err());
        // 0x19 0x00 0xff = uint 255 with two-byte argument: non-minimal.
        assert!(from_bytes(&[0x19, 0x00, 0xff]).is_err());
    }

    #[test]
    fn rejects_unsorted_and_duplicate_keys() {
        // {"b":1,"a":2} — wrong order.
        assert!(from_bytes(&[0xa2, 0x61, b'b', 0x01, 0x61, b'a', 0x02]).is_err());
        // {"a":1,"a":2} — duplicate.
        assert!(from_bytes(&[0xa2, 0x61, b'a', 0x01, 0x61, b'a', 0x02]).is_err());
    }

    #[test]
    fn rejects_indefinite_and_floats() {
        assert!(from_bytes(&[0x9f, 0xff]).is_err()); // indefinite array
        assert!(from_bytes(&[0xf9, 0x3c, 0x00]).is_err()); // f16 1.0
    }

    #[test]
    fn unknown_tags_are_retained() {
        let v = Value::Tagged(777, Box::new(Value::array([Value::Int(1)])));
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn claim_ref_tag_roundtrips() {
        let r = ClaimRef {
            log_id: LogId([7; 32]),
            hash: ClaimHash([9; 32]),
        };
        let v = Value::ClaimRef(r);
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn blob_ref_tag_roundtrips() {
        let b = BlobRef {
            hash: BlobHash([3; 32]),
            size: 1_048_576,
            mime: "image/jpeg".into(),
        };
        let v = Value::BlobRef(b);
        assert_eq!(roundtrip(&v), v);
    }

    #[test]
    fn malformed_blob_ref_degrades_to_tagged() {
        // tag 33003 wrapping a negative size: wrong shape, still decodes.
        let inner = Value::array([Value::Bytes(vec![3; 32]), Value::Int(-1), Value::text("x")]);
        let mut bytes = Vec::new();
        head(&mut bytes, 6, TAG_BLOB_REF);
        encode_value(&mut bytes, &inner);
        let v = from_bytes(&bytes).expect("decodes leniently");
        assert_eq!(v, Value::Tagged(TAG_BLOB_REF, Box::new(inner)));
        assert_eq!(to_bytes(&v), bytes);
    }

    #[test]
    fn malformed_claim_ref_degrades_to_tagged() {
        // tag 33001 wrapping a bare int: wrong shape, but must still decode.
        let mut bytes = Vec::new();
        head(&mut bytes, 6, TAG_CLAIM_REF);
        bytes.push(0x05);
        let v = from_bytes(&bytes).expect("decodes leniently");
        assert_eq!(v, Value::Tagged(TAG_CLAIM_REF, Box::new(Value::Int(5))));
        // and it re-encodes byte-identically
        assert_eq!(to_bytes(&v), bytes);
    }
}
