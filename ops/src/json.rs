//! Serde owns JSON grammar and structural validation. Borrowed RawValues retain
//! numeric spelling while a map visitor rejects decoded duplicate keys at every
//! depth. Finite float conversion matches Python, and integers remain distinct.
//! Each caller selects an encoded byte bound; decoded UTF-8 is limited to twice
//! that bound. An explicit depth limit of 128 and 100,000-value budget bound the
//! repeated structural visits of nested borrowed containers.

use crate::Error;
use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::BTreeMap;

const MAX_DEPTH: usize = 128;
const MAX_VALUES: usize = 100_000;
const MAX_DECODED: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(crate) enum Json {
    Object(BTreeMap<String, Self>, Vec<String>),
    String(String),
    Integer(String),
    Float(String),
    Bool(bool),
    Null,
    Array(Vec<Self>),
}
impl Json {
    pub(crate) fn string(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn object(&self) -> Option<&BTreeMap<String, Self>> {
        if let Self::Object(value, _) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        self.object()?.get(key)
    }
    pub(crate) fn array(&self) -> Option<&[Self]> {
        if let Self::Array(value) = self {
            Some(value)
        } else {
            None
        }
    }
    pub(crate) fn truthiness(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::String(value) => !value.is_empty(),
            Self::Integer(value) => !matches!(value.as_str(), "0" | "-0"),
            Self::Float(value) => value.parse::<f64>().expect("validated finite float") != 0.0,
            Self::Array(value) => !value.is_empty(),
            Self::Object(value, _) => !value.is_empty(),
        }
    }
    /// Python's recursive JSON equality, including numeric booleans and exact
    /// integer-vs-float comparison. Never round an arbitrary integer into f64.
    pub(crate) fn semantic_eq(&self, other: &Self) -> bool {
        fn integer(value: &Json) -> Option<&str> {
            match value {
                Json::Integer(v) => Some(if v == "-0" { "0" } else { v }),
                Json::Bool(true) => Some("1"),
                Json::Bool(false) => Some("0"),
                _ => None,
            }
        }
        if let (Some(a), Some(b)) = (integer(self), integer(other)) {
            return a == b;
        }
        let float = |v: &Self| {
            if let Self::Float(v) = v {
                v.parse::<f64>().ok()
            } else {
                None
            }
        };
        if let (Some(a), Some(b)) = (float(self), float(other)) {
            return a == b;
        }
        for (a, b) in [(self, other), (other, self)] {
            if let (Some(a), Some(b)) = (integer(a), float(b)) {
                return b.fract() == 0.0
                    && if b == 0.0 {
                        a == "0"
                    } else {
                        a == format!("{b:.0}")
                    };
            }
        }
        if let Some(a) = self.array() {
            return other.array().is_some_and(|b| {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| a.semantic_eq(b))
            });
        }
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::String(a), Self::String(b)) => a == b,
            (Self::Object(a, _), Self::Object(b, _)) => {
                a.len() == b.len()
                    && a.iter()
                        .all(|(key, a)| b.get(key).is_some_and(|b| a.semantic_eq(b)))
            }
            _ => false,
        }
    }
    pub(crate) fn integer(&self) -> Option<i64> {
        if let Self::Integer(value) = self {
            value.parse().ok()
        } else {
            None
        }
    }
    pub(crate) fn number(&self) -> Option<f64> {
        match self {
            Self::Integer(v) | Self::Float(v) => v.parse().ok(),
            _ => None,
        }
    }
    pub(crate) fn boolean(&self) -> Option<bool> {
        if let Self::Bool(value) = self {
            Some(*value)
        } else {
            None
        }
    }
}
/// A decoded value serialized with every object's keys in the order they
/// were read, where [`Json`] itself writes them sorted.
pub(crate) struct InOrder<'a>(pub &'a Json);

impl serde::Serialize for InOrder<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self.0 {
            Json::Object(entries, order) => {
                let fields = order
                    .iter()
                    .filter_map(|key| Some((key, Self(entries.get(key)?))));
                serializer.collect_map(fields)
            }
            Json::Array(items) => serializer.collect_seq(items.iter().map(Self)),
            other => other.serialize(serializer),
        }
    }
}

impl serde::Serialize for Json {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Object(value, _) => value.serialize(serializer),
            Self::Array(value) => value.serialize(serializer),
            Self::String(value) => value.serialize(serializer),
            Self::Integer(value) | Self::Float(value) => {
                let raw =
                    RawValue::from_string(value.clone()).map_err(serde::ser::Error::custom)?;
                raw.serialize(serializer)
            }
            Self::Bool(value) => value.serialize(serializer),
            Self::Null => serializer.serialize_none(),
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
            let mut values = Vec::new();
            for value in serde_json::from_str::<Array<'_>>(source)
                .map_err(|_| Error::Json)?
                .0
            {
                values.push(convert(value, remaining, depth + 1)?);
            }
            Ok(Json::Array(values))
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
            Ok(Json::Float(source.into()))
        }
        Some(b't') => Ok(Json::Bool(true)),
        Some(b'f') => Ok(Json::Bool(false)),
        _ => Ok(Json::Null),
    }
}

fn text(bytes: &[u8], maximum: usize) -> Result<Cow<'_, str>, Error> {
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
    // UTF32 requires no expansion. Twice the encoded byte budget covers both.
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
    if output.len() > maximum {
        return Err(Error::Json);
    }
    Ok(Cow::Owned(output))
}

pub(crate) fn decode(bytes: &[u8]) -> Result<Json, Error> {
    decode_limited(bytes, MAX_DECODED / 2)
}

pub(crate) fn decode_limited(bytes: &[u8], maximum: usize) -> Result<Json, Error> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(Error::Json);
    }
    let source = text(bytes, maximum.saturating_mul(2))?;
    let raw = serde_json::from_str::<&RawValue>(&source).map_err(|_| Error::Json)?;
    let mut remaining = MAX_VALUES;
    convert(raw, &mut remaining, 0)
}

pub(crate) fn character_count(bytes: &[u8], maximum: usize) -> Result<usize, Error> {
    if bytes.is_empty() || bytes.len() > maximum {
        return Err(Error::Json);
    }
    Ok(text(bytes, maximum.saturating_mul(2))?.chars().count())
}

/// A finite float as Python writes it: the shortest text that reads back as
/// the value, with a signed exponent of at least two digits outside the
/// fixed-notation range.
pub(crate) fn python_float(value: f64) -> String {
    let text = format!("{value:?}");
    let Some((mantissa, exponent)) = text.split_once('e') else {
        return text;
    };
    let (sign, digits) = match exponent.strip_prefix('-') {
        Some(digits) => ('-', digits),
        None => ('+', exponent),
    };
    format!("{mantissa}e{sign}{digits:0>2}")
}

fn python_string(text: &str, output: &mut String) {
    output.push('"');
    for character in text.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '\u{8}' => output.push_str("\\b"),
            '\u{c}' => output.push_str("\\f"),
            ' '..='~' => output.push(character),
            other => {
                let mut units = [0_u16; 2];
                for unit in other.encode_utf16(&mut units) {
                    output.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    output.push('"');
}

fn python_value(value: &Json, sorted: bool, output: &mut String) {
    match value {
        Json::Object(entries, order) => {
            output.push('{');
            let names: Vec<&String> = if sorted {
                entries.keys().collect()
            } else {
                order.iter().collect()
            };
            for (at, name) in names.iter().enumerate() {
                if at > 0 {
                    output.push_str(", ");
                }
                python_string(name, output);
                output.push_str(": ");
                if let Some(item) = entries.get(*name) {
                    python_value(item, sorted, output);
                }
            }
            output.push('}');
        }
        Json::Array(items) => {
            output.push('[');
            for (at, item) in items.iter().enumerate() {
                if at > 0 {
                    output.push_str(", ");
                }
                python_value(item, sorted, output);
            }
            output.push(']');
        }
        Json::String(text) => python_string(text, output),
        Json::Integer(text) => output.push_str(if text == "-0" { "0" } else { text }),
        Json::Float(text) => {
            let number = text.parse::<f64>().expect("validated finite float");
            output.push_str(&python_float(number));
        }
        Json::Bool(true) => output.push_str("true"),
        Json::Bool(false) => output.push_str("false"),
        Json::Null => output.push_str("null"),
    }
}

/// The text Python's `json.dumps` writes for a decoded value: `, ` and `: `
/// between items, everything outside printable ASCII escaped, floats in
/// Python's spelling, and each object's keys sorted when asked, or else in
/// the order they were read. A digest taken over this text is the one the
/// Python helpers recorded.
pub(crate) fn python_dump(value: &Json, sorted: bool) -> String {
    let mut output = String::new();
    python_value(value, sorted, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    /// Twelve recorded documents: nested sorting, every escape, surrogate
    /// pairs, exponents on both sides, negative zero, a whole number beyond
    /// any float, and a resolved project as a host spells it.
    #[test]
    fn sorted_dumps_and_their_digests_are_the_recorded_ones() {
        let recorded = include_str!("reboot/recorded_reboot.json");
        let recorded: serde_json::Value = serde_json::from_str(recorded).unwrap();
        let vectors = recorded["canonical"].as_array().unwrap();
        assert_eq!(vectors.len(), 12);
        for vector in vectors {
            let document = vector["document"].as_str().unwrap();
            let dump = python_dump(&decode(document.as_bytes()).unwrap(), true);
            assert_eq!(dump, vector["dump"].as_str().unwrap(), "{document}");
            let digest: String = Sha256::digest(dump.as_bytes())
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
            assert_eq!(digest, vector["sha256"].as_str().unwrap(), "{document}");
        }
        let ordered = decode(br#"{"b": [1, {"z": 1.50, "a": null}], "a": "x"}"#).unwrap();
        assert_eq!(
            python_dump(&ordered, false),
            r#"{"b": [1, {"z": 1.5, "a": null}], "a": "x"}"#
        );
    }
}
