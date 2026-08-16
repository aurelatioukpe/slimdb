//! The SlimDB document model.
//!
//! A document is a tree of [`Value`]s. The binary codec is deterministic
//! (objects are stored with sorted keys, NaN is canonicalized) so that the
//! same logical document always serializes to the same bytes — a property the
//! crash-recovery tests rely on.

use std::collections::BTreeMap;

use crate::error::{Result, SlimDbError};

/// Maximum nesting depth accepted by the codec (protects against stack
/// overflow when decoding untrusted input).
pub const MAX_DEPTH: usize = 128;

const TAG_NULL: u8 = 0x00;
const TAG_BOOL: u8 = 0x01;
const TAG_INT: u8 = 0x02;
const TAG_FLOAT: u8 = 0x03;
const TAG_STR: u8 = 0x04;
const TAG_BIN: u8 = 0x05;
const TAG_ARRAY: u8 = 0x06;
const TAG_OBJECT: u8 = 0x07;

/// A canonical NaN bit pattern (quiet NaN, sign bit set).
const CANONICAL_NAN: u64 = 0x7FF8_0000_0000_0000;

/// A SlimDB value: one of the eight supported types.
#[derive(Debug, Clone)]
pub enum Value {
    /// JSON `null`.
    Null,
    /// Boolean.
    Bool(bool),
    /// 64-bit signed integer.
    Int(i64),
    /// 64-bit IEEE float (NaN is canonicalized on construction).
    Float(f64),
    /// UTF-8 string.
    Str(String),
    /// Raw bytes.
    Binary(Vec<u8>),
    /// Array of values.
    Array(Vec<Value>),
    /// Object with sorted keys.
    Object(BTreeMap<String, Value>),
}

/// Equality is bitwise for floats so that canonicalized NaN equals itself.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Null, Value::Null) => true,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a.to_bits() == b.to_bits(),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Binary(a), Value::Binary(b)) => a == b,
            (Value::Array(a), Value::Array(b)) => a == b,
            (Value::Object(a), Value::Object(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Value {}

impl Value {
    /// Create a null value.
    pub fn null() -> Value {
        Value::Null
    }

    /// Convenience constructor for an object.
    pub fn object(entries: impl IntoIterator<Item = (String, Value)>) -> Value {
        Value::Object(entries.into_iter().collect())
    }

    /// Recursively canonicalize this value: floats NaN become a fixed bit
    /// pattern, objects are kept sorted.
    pub fn canonicalize(self) -> Value {
        match self {
            Value::Float(f) => {
                let bits = f.to_bits();
                let is_nan = bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000;
                if is_nan {
                    Value::Float(f64::from_bits(CANONICAL_NAN))
                } else {
                    Value::Float(f)
                }
            }
            Value::Array(items) => {
                Value::Array(items.into_iter().map(Value::canonicalize).collect())
            }
            Value::Object(map) => Value::Object(
                map.into_iter()
                    .map(|(k, v)| (k, v.canonicalize()))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Borrow the value as an integer, if it is one.
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// Borrow the value as a float, if it is one.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Borrow the value as a string, if it is one.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Borrow the value as a bool, if it is one.
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Borrow the value as an object map, if it is one.
    pub fn as_object(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Value::Object(m) => Some(m),
            _ => None,
        }
    }

    /// Borrow the value as an array, if it is one.
    pub fn as_array(&self) -> Option<&Vec<Value>> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }

    /// Encode this value in the SlimDB binary codec.
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.encode_inner(out, 0)
    }

    fn encode_inner(&self, out: &mut Vec<u8>, depth: usize) -> Result<()> {
        if depth > MAX_DEPTH {
            return Err(SlimDbError::InvalidArgument(format!(
                "value nesting deeper than {MAX_DEPTH}"
            )));
        }
        match self {
            Value::Null => out.push(TAG_NULL),
            Value::Bool(b) => {
                out.push(TAG_BOOL);
                out.push(*b as u8);
            }
            Value::Int(i) => {
                out.push(TAG_INT);
                out.extend_from_slice(&i.to_le_bytes());
            }
            Value::Float(f) => {
                out.push(TAG_FLOAT);
                out.extend_from_slice(&f.to_bits().to_le_bytes());
            }
            Value::Str(s) => {
                out.push(TAG_STR);
                push_u32(out, s.len())?;
                out.extend_from_slice(s.as_bytes());
            }
            Value::Binary(b) => {
                out.push(TAG_BIN);
                push_u32(out, b.len())?;
                out.extend_from_slice(b);
            }
            Value::Array(items) => {
                out.push(TAG_ARRAY);
                push_u32(out, items.len())?;
                for item in items {
                    item.encode_inner(out, depth + 1)?;
                }
            }
            Value::Object(map) => {
                out.push(TAG_OBJECT);
                push_u32(out, map.len())?;
                for (k, v) in map {
                    push_u32(out, k.len())?;
                    out.extend_from_slice(k.as_bytes());
                    v.encode_inner(out, depth + 1)?;
                }
            }
        }
        Ok(())
    }

    /// Decode a single value from `bytes`, returning the value and the number
    /// of bytes consumed.
    pub fn decode(bytes: &[u8]) -> Result<(Value, usize)> {
        decode_inner(bytes, 0, 0)
    }

    /// Decode a value that must span exactly `bytes`.
    pub fn decode_all(bytes: &[u8]) -> Result<Value> {
        let (value, consumed) = Self::decode(bytes)?;
        if consumed != bytes.len() {
            return Err(SlimDbError::Decode(format!(
                "trailing bytes after value: consumed {consumed} of {}",
                bytes.len()
            )));
        }
        Ok(value)
    }
}

fn push_u32(out: &mut Vec<u8>, len: usize) -> Result<()> {
    let len = u32::try_from(len)
        .map_err(|_| SlimDbError::InvalidArgument("value too large".to_string()))?;
    out.extend_from_slice(&len.to_le_bytes());
    Ok(())
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Cursor { bytes, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.pos + n > self.bytes.len() {
            return Err(SlimDbError::Decode("unexpected end of input".to_string()));
        }
        let slice = &self.bytes[self.pos..self.pos + n];
        self.pos += n;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64> {
        let b = self.take(8)?;
        Ok(u64::from_le_bytes([
            b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7],
        ]))
    }

    fn bytes_len(&mut self, len: u32) -> Result<Vec<u8>> {
        Ok(self.take(len as usize)?.to_vec())
    }
}

fn decode_inner(bytes: &[u8], start: usize, depth: usize) -> Result<(Value, usize)> {
    if depth > MAX_DEPTH {
        return Err(SlimDbError::Decode("nesting deeper than limit".to_string()));
    }
    let mut cur = Cursor::new(&bytes[start..]);
    let tag = cur.u8()?;
    let value = match tag {
        TAG_NULL => Value::Null,
        TAG_BOOL => {
            let b = cur.u8()?;
            if b > 1 {
                return Err(SlimDbError::Decode(format!("invalid bool byte {b}")));
            }
            Value::Bool(b == 1)
        }
        TAG_INT => Value::Int(cur.u64()? as i64),
        TAG_FLOAT => {
            let bits = cur.u64()?;
            let is_nan = bits & 0x7FFF_FFFF_FFFF_FFFF > 0x7FF0_0000_0000_0000;
            let bits = if is_nan { CANONICAL_NAN } else { bits };
            Value::Float(f64::from_bits(bits))
        }
        TAG_STR => {
            let len = cur.u32()?;
            let s = cur.bytes_len(len)?;
            let s = String::from_utf8(s)
                .map_err(|_| SlimDbError::Decode("invalid UTF-8 in string".to_string()))?;
            Value::Str(s)
        }
        TAG_BIN => {
            let len = cur.u32()?;
            Value::Binary(cur.bytes_len(len)?)
        }
        TAG_ARRAY => {
            let count = cur.u32()?;
            let mut items = Vec::with_capacity(count as usize);
            for _ in 0..count {
                let (v, consumed) = decode_inner(bytes, start + cur.pos, depth + 1)?;
                cur.pos += consumed;
                items.push(v);
            }
            Value::Array(items)
        }
        TAG_OBJECT => {
            let count = cur.u32()?;
            let mut map = BTreeMap::new();
            for _ in 0..count {
                let klen = cur.u32()?;
                let kb = cur.bytes_len(klen)?;
                let key = String::from_utf8(kb)
                    .map_err(|_| SlimDbError::Decode("invalid UTF-8 in object key".to_string()))?;
                let (v, consumed) = decode_inner(bytes, start + cur.pos, depth + 1)?;
                cur.pos += consumed;
                if map.insert(key.clone(), v).is_some() {
                    return Err(SlimDbError::Decode(format!("duplicate object key {key:?}")));
                }
            }
            Value::Object(map)
        }
        other => {
            return Err(SlimDbError::Decode(format!(
                "unknown value tag {other:#04x}"
            )))
        }
    };
    Ok((value, cur.pos))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: Value) -> Value {
        let mut buf = Vec::new();
        v.encode(&mut buf).unwrap();
        Value::decode_all(&buf).unwrap()
    }

    #[test]
    fn roundtrips_all_types() {
        let doc = Value::object(vec![
            ("null".to_string(), Value::Null),
            ("bool".to_string(), Value::Bool(true)),
            ("int".to_string(), Value::Int(-42)),
            ("float".to_string(), Value::Float(3.5)),
            ("str".to_string(), Value::Str("héllo".to_string())),
            ("bin".to_string(), Value::Binary(vec![0, 1, 2, 255])),
            (
                "arr".to_string(),
                Value::Array(vec![Value::Int(1), Value::Str("x".to_string())]),
            ),
            (
                "obj".to_string(),
                Value::Object(
                    [
                        ("b".to_string(), Value::Int(2)),
                        ("a".to_string(), Value::Int(1)),
                    ]
                    .into_iter()
                    .collect(),
                ),
            ),
        ]);
        assert_eq!(roundtrip(doc.clone()), doc);
    }

    #[test]
    fn nan_is_canonicalized() {
        let v = Value::Float(f64::NAN);
        let c = v.canonicalize();
        assert_eq!(c.as_f64().unwrap().to_bits(), CANONICAL_NAN);
        assert_eq!(roundtrip(c.clone()), c);
    }

    #[test]
    fn float_nan_equals_itself() {
        let a = Value::Float(f64::from_bits(CANONICAL_NAN));
        let b = Value::Float(f64::from_bits(CANONICAL_NAN));
        assert_eq!(a, b);
        assert!(a != Value::Float(0.0));
    }

    #[test]
    fn decode_rejects_garbage() {
        assert!(Value::decode_all(&[0xFF]).is_err());
        assert!(Value::decode_all(&[TAG_STR, 0x05, 0, 0, 0, b'a']).is_err()); // truncated
        assert!(Value::decode_all(&[TAG_BOOL, 0x02]).is_err()); // invalid bool byte
    }

    #[test]
    fn nesting_limit() {
        // 200 nested arrays exceeds the depth limit.
        let mut v = Value::Null;
        for _ in 0..200 {
            v = Value::Array(vec![v]);
        }
        let mut buf = Vec::new();
        assert!(v.encode(&mut buf).is_err());
    }
}
