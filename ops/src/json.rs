//! Serde owns JSON grammar and structural validation. Borrowed RawValues retain
//! numeric spelling while a map visitor rejects decoded duplicate keys at every
//! depth. Finite float conversion matches Python, and integers remain distinct.
//! The manifest input bound, explicit depth limit of 128 and 100,000-value budget bound
//! the repeated structural visits of nested borrowed containers.

use crate::Error;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::BTreeMap;

const MAX_DEPTH: usize = 128;
const MAX_VALUES: usize = 100_000;
const MAX_DECODED: usize = 2 * 1024 * 1024;

#[derive(Debug)]
pub(crate) enum Json {
    Object(BTreeMap<String, Self>, Vec<String>),
    String(String),
    Integer(String),
    Other,
}
impl Json {
    pub(crate) fn string(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
}

struct Object<'a>(Vec<(String, &'a RawValue)>);
impl<'de> Deserialize<'de> for Object<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Map;
        impl<'de> Visitor<'de> for Map {
            type Value = Object<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut keys = std::collections::BTreeSet::new();
                let mut values = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, &'de RawValue>()? {
                    if values.len() >= MAX_VALUES {
                        return Err(serde::de::Error::custom("invalid_json"));
                    }
                    if !keys.insert(key.clone()) {
                        return Err(serde::de::Error::custom("ambiguous_json"));
                    }
                    values.push((key, value));
                }
                Ok(Object(values))
            }
        }
        deserializer.deserialize_map(Map)
    }
}

struct Array<'a>(Vec<&'a RawValue>);
impl<'de> Deserialize<'de> for Array<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Sequence;
        impl<'de> Visitor<'de> for Sequence {
            type Value = Array<'de>;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON array")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut array: A) -> Result<Self::Value, A::Error> {
                let mut values = Vec::new();
                while let Some(value) = array.next_element::<&'de RawValue>()? {
                    if values.len() >= MAX_VALUES {
                        return Err(serde::de::Error::custom("invalid_json"));
                    }
                    values.push(value);
                }
                Ok(Array(values))
            }
        }
        deserializer.deserialize_seq(Sequence)
    }
}

fn convert(raw: &RawValue, remaining: &mut usize, depth: usize) -> Result<Json, Error> {
    if depth >= MAX_DEPTH && matches!(raw.get().as_bytes().first(), Some(b'{' | b'[')) {
        return Err(Error::Json);
    }
    *remaining = remaining.checked_sub(1).ok_or(Error::Json)?;
    let source = raw.get();
    match source.as_bytes().first() {
        Some(b'{') => {
            let object = serde_json::from_str::<Object<'_>>(source).map_err(|error| {
                if error.to_string().starts_with("ambiguous_json") {
                    Error::AmbiguousJson
                } else {
                    Error::Json
                }
            })?;
            let mut result = BTreeMap::new();
            let mut order = Vec::new();
            for (key, value) in object.0 {
                order.push(key.clone());
                result.insert(key, convert(value, remaining, depth + 1)?);
            }
            Ok(Json::Object(result, order))
        }
        Some(b'[') => {
            for value in serde_json::from_str::<Array<'_>>(source)
                .map_err(|_| Error::Json)?
                .0
            {
                convert(value, remaining, depth + 1)?;
            }
            Ok(Json::Other)
        }
        Some(b'"') => serde_json::from_str(source)
            .map(Json::String)
            .map_err(|_| Error::Json),
        Some(b'-' | b'0'..=b'9') if !source.contains(['.', 'e', 'E']) => {
            // CPython's default JSON decoder bounds decimal integer conversion.
            if source.trim_start_matches('-').len() > 4300 {
                return Err(Error::Json);
            }
            Ok(Json::Integer(source.into()))
        }
        Some(b'-' | b'0'..=b'9') => {
            if !source.parse::<f64>().map_err(|_| Error::Json)?.is_finite() {
                return Err(Error::Json);
            }
            Ok(Json::Other)
        }
        _ => Ok(Json::Other),
    }
}

fn text(bytes: &[u8]) -> Result<Cow<'_, str>, Error> {
    // Match json.detect_encoding: four-byte BOMs precede two-byte BOMs,
    // followed by null-pattern detection for BOM-less UTF16/UTF32 input.
    let (width, little, skip) = if bytes.starts_with(&[0xff, 0xfe, 0, 0]) {
        (4, true, 4)
    } else if bytes.starts_with(&[0, 0, 0xfe, 0xff]) {
        (4, false, 4)
    } else if bytes.starts_with(&[0xff, 0xfe]) {
        (2, true, 2)
    } else if bytes.starts_with(&[0xfe, 0xff]) {
        (2, false, 2)
    } else if bytes.starts_with(&[0xef, 0xbb, 0xbf]) {
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
            .map_err(|_| Error::Json);
    }
    if !bytes.len().is_multiple_of(width) {
        return Err(Error::Json);
    }
    let mut output = String::new();
    // UTF16 BMP scalars need at most three UTF8 bytes per two source bytes;
    // UTF32 requires no expansion. A fixed2MiB decoded bound covers both.
    output
        .try_reserve(bytes.len().saturating_mul(2))
        .map_err(|_| Error::Json)?;
    if width == 2 {
        let units = bytes.as_chunks::<2>().0.iter().map(|b| {
            if little {
                u16::from_le_bytes([b[0], b[1]])
            } else {
                u16::from_be_bytes([b[0], b[1]])
            }
        });
        for character in char::decode_utf16(units) {
            output.push(character.map_err(|_| Error::Json)?);
        }
    } else {
        for b in bytes.as_chunks::<4>().0 {
            let unit = if little {
                u32::from_le_bytes([b[0], b[1], b[2], b[3]])
            } else {
                u32::from_be_bytes([b[0], b[1], b[2], b[3]])
            };
            output.push(char::from_u32(unit).ok_or(Error::Json)?);
        }
    }
    if output.len() > MAX_DECODED {
        return Err(Error::Json);
    }
    Ok(Cow::Owned(output))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Json, Error> {
    let source = text(bytes)?;
    let raw = serde_json::from_str::<&RawValue>(&source).map_err(|_| Error::Json)?;
    let mut remaining = MAX_VALUES;
    convert(raw, &mut remaining, 0)
}
