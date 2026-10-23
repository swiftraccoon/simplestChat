//! Bounded selected JSON with Python byte encodings, recursive duplicate refusal
//! and exact integer/float distinctions. Serde owns the grammar; raw number
//! spellings preserve Python's arbitrary integers without f64 rounding.

use super::{Result, fail};
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use serde_json::value::RawValue;
use std::{borrow::Cow, collections::BTreeMap, fmt};

#[derive(PartialEq)]
pub(super) enum Value {
    Object(BTreeMap<String, Self>),
    Array(Vec<Self>),
    String(String),
    Integer(String),
    Float(String),
    Bool(bool),
    Null,
}
impl Value {
    pub fn as_str(&self) -> Option<&str> {
        if let Self::String(s) = self {
            Some(s)
        } else {
            None
        }
    }
    pub fn as_object(&self) -> Option<&BTreeMap<String, Self>> {
        if let Self::Object(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_array(&self) -> Option<&[Self]> {
        if let Self::Array(v) = self {
            Some(v)
        } else {
            None
        }
    }
    pub fn as_u64(&self) -> Option<u64> {
        if let Self::Integer(v) = self {
            v.parse().ok()
        } else {
            None
        }
    }
    pub fn is_boolean(&self) -> bool {
        matches!(self, Self::Bool(_))
    }
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }
}
struct Object<'a>(BTreeMap<String, &'a RawValue>);
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("unique keys")
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut map: A,
            ) -> std::result::Result<Self::Value, A::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = map.next_entry::<String, &RawValue>()? {
                    if values.insert(key, value).is_some() {
                        return Err(de::Error::custom("duplicate"));
                    }
                }
                Ok(Object(values))
            }
        }
        d.deserialize_map(Entries)
    }
}
fn invalid() -> crate::tooling::Failure {
    fail(1, "http_policy_failed")
}
fn convert(raw: &RawValue, depth: usize) -> Result<Value> {
    if depth >= 128 {
        return Err(invalid());
    }
    let s = raw.get();
    Ok(match s.as_bytes().first() {
        Some(b'{') => {
            let Object(values) = serde_json::from_str(s).map_err(|_| invalid())?;
            Value::Object(
                values
                    .into_iter()
                    .map(|(k, v)| Ok((k, convert(v, depth + 1)?)))
                    .collect::<Result<_>>()?,
            )
        }
        Some(b'[') => {
            let values: Vec<&RawValue> = serde_json::from_str(s).map_err(|_| invalid())?;
            Value::Array(
                values
                    .into_iter()
                    .map(|v| convert(v, depth + 1))
                    .collect::<Result<_>>()?,
            )
        }
        Some(b'"') => Value::String(serde_json::from_str(s).map_err(|_| invalid())?),
        Some(b'-' | b'0'..=b'9') if !s.contains(['.', 'e', 'E']) => {
            if s.trim_start_matches('-').len() > 4300 {
                return Err(invalid());
            }
            Value::Integer(s.into())
        }
        Some(b'-' | b'0'..=b'9') => {
            if !s.parse::<f64>().map_err(|_| invalid())?.is_finite() {
                return Err(invalid());
            }
            Value::Float(s.into())
        }
        Some(b't') => Value::Bool(true),
        Some(b'f') => Value::Bool(false),
        _ => Value::Null,
    })
}
// Same bounded std transcode rule as the mature Ops JSON owner. Raw-byte body
// limits apply before conversion; decoded scalar text uses at most twice them.
fn text(bytes: &[u8]) -> Result<Cow<'_, str>> {
    let (width, little, skip) = if bytes.starts_with(&[255, 254, 0, 0]) {
        (4, true, 4)
    } else if bytes.starts_with(&[0, 0, 254, 255]) {
        (4, false, 4)
    } else if bytes.starts_with(&[255, 254]) {
        (2, true, 2)
    } else if bytes.starts_with(&[254, 255]) {
        (2, false, 2)
    } else if bytes.starts_with(&[239, 187, 191]) {
        (1, false, 3)
    } else if bytes.len() >= 4 && bytes[0] == 0 {
        (if bytes[1] != 0 { 2 } else { 4 }, false, 0)
    } else if bytes.len() >= 4 && bytes[1] == 0 {
        (if bytes[2] != 0 || bytes[3] != 0 { 2 } else { 4 }, true, 0)
    } else if bytes.len() == 2 && bytes[0] == 0 {
        (2, false, 0)
    } else if bytes.len() == 2 && bytes[1] == 0 {
        (2, true, 0)
    } else {
        (1, false, 0)
    };
    let bytes = &bytes[skip..];
    if width == 1 {
        return std::str::from_utf8(bytes)
            .map(Cow::Borrowed)
            .map_err(|_| invalid());
    }
    if !bytes.len().is_multiple_of(width) {
        return Err(invalid());
    }
    let mut out = String::new();
    out.try_reserve(bytes.len().saturating_mul(2))
        .map_err(|_| invalid())?;
    if width == 2 {
        let units = bytes.as_chunks::<2>().0.iter().map(|b| {
            if little {
                u16::from_le_bytes(*b)
            } else {
                u16::from_be_bytes(*b)
            }
        });
        for c in char::decode_utf16(units) {
            out.push(c.map_err(|_| invalid())?);
        }
    } else {
        for b in bytes.as_chunks::<4>().0 {
            out.push(
                char::from_u32(if little {
                    u32::from_le_bytes(*b)
                } else {
                    u32::from_be_bytes(*b)
                })
                .ok_or_else(invalid)?,
            );
        }
    }
    Ok(Cow::Owned(out))
}
pub(super) fn object(bytes: &[u8]) -> Result<BTreeMap<String, Value>> {
    if bytes.len() > 65536 {
        return Err(invalid());
    }
    let text = text(bytes)?;
    let raw: &RawValue = serde_json::from_str(&text).map_err(|_| invalid())?;
    match convert(raw, 0)? {
        Value::Object(v) => Ok(v),
        _ => Err(invalid()),
    }
}
