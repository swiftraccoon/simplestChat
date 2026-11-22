//! JSON the fixture reads from the engine, the Ops executable and the browser
//! helper: an object that repeats a key, at any depth, is refused instead of
//! silently keeping one of its values. Integers and floats stay distinct.

use serde::{
    Deserialize,
    de::{self, Deserializer, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};
use std::fmt;

/// One decoded document, or `None` when it is not one strict JSON value.
pub(super) fn decode(bytes: &[u8]) -> Option<Value> {
    let mut reader = serde_json::Deserializer::from_slice(bytes);
    let value = Strict::deserialize(&mut reader).ok()?.0;
    reader.end().ok()?;
    Some(value)
}

/// The object `value` is, if it is one.
pub(super) fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}

/// A JSON integer that is not a boolean, as an `i64`.
pub(super) fn integer(value: Option<&Value>) -> Option<i64> {
    match value? {
        Value::Number(number) if !number.is_f64() => number.as_i64(),
        _ => None,
    }
}

struct Strict(Value);

impl<'de> serde::Deserialize<'de> for Strict {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(StrictVisitor).map(Strict)
    }
}

struct StrictVisitor;

impl<'de> Visitor<'de> for StrictVisitor {
    type Value = Value;
    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without repeated keys")
    }
    fn visit_bool<E>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }
    fn visit_i64<E>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_u64<E>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }
    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite number"))
    }
    fn visit_str<E>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.to_owned()))
    }
    fn visit_string<E>(self, value: String) -> Result<Value, E> {
        Ok(Value::String(value))
    }
    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        let mut values = Vec::new();
        while let Some(Strict(value)) = sequence.next_element()? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            let Strict(value) = map.next_value()?;
            if values.insert(key, value).is_some() {
                return Err(de::Error::custom("repeated key"));
            }
        }
        Ok(Value::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn repeated_keys_at_any_depth_and_trailing_text_are_refused() {
        assert_eq!(
            decode(br#"{"a":[1,{"b":2.0}]}"#),
            Some(json!({"a":[1,{"b":2.0}]}))
        );
        assert_eq!(decode(br#"{"a":1,"a":1}"#), None);
        assert_eq!(decode(br#"{"a":{"b":1,"b":2}}"#), None);
        assert_eq!(decode(br#"[{"a":1,"a":2}]"#), None);
        assert_eq!(decode(b"{} {}"), None);
        assert_eq!(decode(b"NaN"), None);
        assert_ne!(decode(b"5"), decode(b"5.0"));
        assert_eq!(integer(decode(b"5").as_ref()), Some(5));
        assert_eq!(integer(decode(b"5.0").as_ref()), None);
        assert_eq!(integer(decode(b"true").as_ref()), None);
    }
}
