//! Reading decoded values the way the journals of a move are read: a field
//! that is absent and a value of the wrong shape are different failures, and
//! a comparison never guesses across types.

use crate::host::failure::{Failure, Result};
use crate::json::Json;

/// A required field of an object.
pub(super) fn field<'a>(value: &'a Json, name: &str) -> Result<&'a Json> {
    value
        .object()
        .ok_or(Failure::Value)?
        .get(name)
        .ok_or(Failure::Key)
}

/// A value that must be text.
pub(super) fn text(value: &Json) -> Result<&str> {
    value.string().ok_or(Failure::Value)
}

/// A value that must be an object.
pub(super) fn object_of(value: &Json) -> Result<&Json> {
    value.object().map(|_| value).ok_or(Failure::Value)
}

/// Whether a field is exactly the text `expected`.
pub(super) fn says(value: &Json, name: &str, expected: &str) -> bool {
    value.get(name).and_then(Json::string) == Some(expected)
}

/// Whether a field is exactly the truth value `expected`.
pub(super) fn holds(value: &Json, name: &str, expected: bool) -> bool {
    matches!(value.get(name), Some(Json::Bool(held)) if *held == expected)
}

/// Whether two optional values are equal, absent ones only to each other.
pub(super) fn same(one: Option<&Json>, other: Option<&Json>) -> bool {
    match (one, other) {
        (Some(one), Some(other)) => one.semantic_eq(other),
        (None, None) => true,
        _ => false,
    }
}

pub(super) fn integer(value: i64) -> Json {
    Json::Integer(value.to_string())
}

/// Whether a field equals the whole number `expected`.
pub(super) fn counts(value: &Json, name: &str, expected: i64) -> bool {
    same(value.get(name), Some(&integer(expected)))
}

/// What the earlier helper took for blank in text: Unicode white space and
/// the four information separators.
pub(super) fn blank(character: char) -> bool {
    character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
}

/// Text without the blank at either end.
pub(super) fn stripped(text: &str) -> &str {
    text.trim_matches(blank)
}

/// The words of a text, whatever blank separates them.
pub(super) fn words(text: &str) -> Vec<&str> {
    text.split(blank).filter(|word| !word.is_empty()).collect()
}

/// Bytes without the ASCII white space at either end.
pub(super) fn trimmed(bytes: &[u8]) -> &[u8] {
    let blank = |byte: &u8| b" \t\n\r\x0b\x0c".contains(byte);
    let start = bytes
        .iter()
        .position(|byte| !blank(byte))
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !blank(byte))
        .map_or(start, |last| last + 1);
    &bytes[start..end]
}
