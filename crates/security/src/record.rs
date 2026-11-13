//! Documents of an exact shape: an object has the fields it is asked for and
//! no other, a string is present and bounded, and a path is spelled one way.

use crate::{Fault, Result, require};
use serde_json::{Map, Value};

/// The most bytes a lock, a receipt or a policy document may have.
pub(crate) const MAX_DOCUMENT: u64 = 1024 * 1024;
const MAX_STRING: usize = 2048;
const MAX_NAME: usize = 512;

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
