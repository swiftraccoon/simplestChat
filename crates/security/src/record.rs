//! Documents of an exact shape: an object has the fields it is asked for and
//! no other, a string is present and bounded, and a path is spelled one way.

use crate::{Fault, Result, require};
use serde_json::{Map, Value};

/// The most bytes a lock, a receipt or a policy document may have.
pub(crate) const MAX_DOCUMENT: u64 = 1024 * 1024;
const MAX_STRING: usize = 2048;
const MAX_NAME: usize = 512;

/// A value read with every record's keys distinct.
struct Distinct;

impl<'de> serde::de::DeserializeSeed<'de> for Distinct {
    type Value = Value;

    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        source: D,
    ) -> std::result::Result<Value, D::Error> {
        source.deserialize_any(self)
    }
}

impl<'de> serde::de::Visitor<'de> for Distinct {
    type Value = Value;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, flag: bool) -> std::result::Result<Value, E> {
        Ok(flag.into())
    }

    fn visit_i64<E>(self, number: i64) -> std::result::Result<Value, E> {
        Ok(number.into())
    }

    fn visit_u64<E>(self, number: u64) -> std::result::Result<Value, E> {
        Ok(number.into())
    }

    fn visit_f64<E>(self, number: f64) -> std::result::Result<Value, E> {
        Ok(number.into())
    }

    fn visit_str<E>(self, text: &str) -> std::result::Result<Value, E> {
        Ok(text.into())
    }

    fn visit_unit<E>(self) -> std::result::Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut items: A,
    ) -> std::result::Result<Value, A::Error> {
        let mut list = Vec::new();
        while let Some(item) = items.next_element_seed(Distinct)? {
            list.push(item);
        }
        Ok(list.into())
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut pairs: A,
    ) -> std::result::Result<Value, A::Error> {
        let mut fields = Map::new();
        while let Some(key) = pairs.next_key::<String>()? {
            let value = pairs.next_value_seed(Distinct)?;
            if fields.insert(key, value).is_some() {
                return Err(serde::de::Error::custom("a key given twice"));
            }
        }
        Ok(fields.into())
    }
}

/// A JSON document none of whose records names a key twice: the reference
/// refuses such a record rather than choose one of its values.
pub(crate) fn strict(content: &[u8]) -> Result<Value> {
    use serde::de::DeserializeSeed as _;
    let mut source = serde_json::Deserializer::from_slice(content);
    let value = Distinct.deserialize(&mut source);
    let value = value.map_err(|_| Fault("invalid_tool_json"))?;
    source.end().map_err(|_| Fault("invalid_tool_json"))?;
    Ok(value)
}

/// A JSON document, or the one code for bytes that are none.
pub(crate) fn document(content: &[u8]) -> Result<Value> {
    serde_json::from_slice(content).map_err(|_| Fault("invalid_tool_json"))
}

/// An object with exactly `keys`: no field missing and none unknown.
pub(crate) fn record<'a>(value: &'a Value, keys: &[&str]) -> Result<&'a Map<String, Value>> {
    let Value::Object(fields) = value else {
        return Err(Fault("invalid_tool_record"));
    };
    require(
        fields.len() == keys.len() && keys.iter().all(|key| fields.contains_key(*key)),
        "invalid_tool_record_fields",
    )?;
    Ok(fields)
}

pub(crate) fn bounded(text: &str) -> Result<&str> {
    require(
        !text.is_empty() && text.chars().count() <= MAX_STRING,
        "invalid_tool_string",
    )?;
    Ok(text)
}

pub(crate) fn string(value: &Value) -> Result<&str> {
    match value {
        Value::String(text) => bounded(text),
        _ => Err(Fault("invalid_tool_string")),
    }
}

/// A canonical relative path: no other spelling of it is admitted. An empty
/// name is one empty component.
pub(crate) fn safe_name(value: &str) -> bool {
    value.chars().count() <= MAX_NAME
        && !value.contains('\\')
        && !value.chars().any(|character| character < ' ')
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}

/// A document as the reference writes one to be read: keys in order, two
/// spaces a level, ASCII only, and a line end after it.
pub(crate) fn pretty(value: &Value) -> String {
    let mut text = String::new();
    spread(value, 0, &mut text);
    text.push('\n');
    text
}

fn spread(value: &Value, depth: usize, text: &mut String) {
    let open = |text: &mut String, index: usize| {
        text.push_str(if index == 0 { "\n" } else { ",\n" });
        text.push_str(&"  ".repeat(depth + 1));
    };
    let close = |text: &mut String, bracket: char| {
        text.push('\n');
        text.push_str(&"  ".repeat(depth));
        text.push(bracket);
    };
    match value {
        Value::Array(items) if !items.is_empty() => {
            text.push('[');
            for (index, item) in items.iter().enumerate() {
                open(text, index);
                spread(item, depth + 1, text);
            }
            close(text, ']');
        }
        Value::Object(fields) if !fields.is_empty() => {
            let mut names: Vec<&String> = fields.keys().collect();
            names.sort_unstable();
            text.push('{');
            for (index, name) in names.into_iter().enumerate() {
                open(text, index);
                text.push_str(&quoted(name));
                text.push_str(": ");
                spread(&fields[name], depth + 1, text);
            }
            close(text, '}');
        }
        Value::String(string) => text.push_str(&quoted(string)),
        // Nothing, a truth value, a number, or a list or record of nothing.
        other => text.push_str(&other.to_string()),
    }
}

/// A JSON string as the reference writes one: ASCII only, with the short
/// escapes for the quote, the backslash and five control characters.
pub(crate) fn quoted(text: &str) -> String {
    use std::fmt::Write as _;
    let mut quoted = String::from("\"");
    for character in text.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            '\u{8}' => quoted.push_str("\\b"),
            '\u{c}' => quoted.push_str("\\f"),
            ' '..='~' => quoted.push(character),
            _ => {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    let _ = write!(quoted, "\\u{unit:04x}");
                }
            }
        }
    }
    quoted.push('"');
    quoted
}
