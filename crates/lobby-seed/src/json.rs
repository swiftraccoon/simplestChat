//! JSON as the seeding transaction reads it: an object that names a key
//! twice is refused, because two readers could disagree about its meaning.

use serde::de::{Deserialize, Deserializer, Error, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;

/// The code for text that is not one JSON value.
pub const INVALID: &str = "invalid_json";
/// The code for an object that names a key twice.
pub const DUPLICATE: &str = "duplicate_json_key";

/// One decoded value. The transaction reads no number, so only the fact of
/// one is kept.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Json {
    Null,
    Bool(bool),
    Number,
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    /// A member of an object, or nothing for any other value.
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(members) => members
                .iter()
                .find_map(|(name, value)| (name == key).then_some(value)),
            _ => None,
        }
    }

    /// The text of a string, or nothing for any other value.
    pub fn text(&self) -> Option<&str> {
        match self {
            Json::String(text) => Some(text),
            _ => None,
        }
    }
}

struct Reader;

impl<'de> Visitor<'de> for Reader {
    type Value = Json;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }
    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }
    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }
    fn visit_i64<E>(self, _: i64) -> Result<Json, E> {
        Ok(Json::Number)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Json, E> {
        Ok(Json::Number)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Json, E> {
        Ok(Json::Number)
    }
    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
    }
    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut items: A) -> Result<Json, A::Error> {
        let mut values = Vec::new();
        while let Some(value) = items.next_element()? {
            values.push(value);
        }
        Ok(Json::Array(values))
    }
    fn visit_map<A: MapAccess<'de>>(self, mut entries: A) -> Result<Json, A::Error> {
        let mut members: Vec<(String, Json)> = Vec::new();
        while let Some(member) = entries.next_entry()? {
            members.push(member);
        }
        // Only a complete object is judged, so text that breaks off inside
        // one is invalid before it is ambiguous.
        let mut names = BTreeSet::new();
        if members.iter().all(|(name, _)| names.insert(name.as_str())) {
            Ok(Json::Object(members))
        } else {
            Err(A::Error::custom(DUPLICATE))
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Json, D::Error> {
        deserializer.deserialize_any(Reader)
    }
}

/// Decode exactly one UTF-8 JSON value, or say why not with a fixed code.
pub fn parse(bytes: &[u8]) -> Result<Json, &'static str> {
    serde_json::from_slice(bytes).map_err(|error| match error.classify() {
        // The reader raises only one error of its own.
        serde_json::error::Category::Data => DUPLICATE,
        _ => INVALID,
    })
}
