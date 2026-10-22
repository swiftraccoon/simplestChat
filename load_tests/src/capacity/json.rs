//! Bounded calibration JSON with Python's original numeric-token semantics.
//!
//! Serde owns structural JSON parsing. A lexical adapter retains number tokens
//! for correctly rounded f64 conversion and the reference's NaN/Infinity values;
//! it neither accepts comments/JSON5 nor reinterprets strings or object keys.
//! Inputs are limited to 128 MiB and one million combined numeric tokens and
//! distinct strings. The internal adapted JSON is limited to 256 MiB and 16 times
//! the input size (with a 64 KiB floor). These bounds are checked before adapting;
//! marker length depends on the bounded item count, never on user string length.

use anyhow::{Result, ensure};
use nix::fcntl::{OFlag, open};
use nix::sys::stat::Mode;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs::File;
use std::io::Read;
use std::ops::Index;
use std::path::Path;

const MAX_RESULT: u64 = 128 * 1024 * 1024;
const MAX_ADAPTED: usize = 256 * 1024 * 1024;
const MAX_ITEMS: usize = 1_000_000;
const MIN_MARKER: &str = "@capacity:0:";

#[derive(Clone, Debug)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl Json {
    pub(crate) fn object(&self, what: &str) -> Result<&BTreeMap<String, Self>> {
        if let Self::Object(value) = self {
            Ok(value)
        } else {
            anyhow::bail!("{what} must be a JSON object")
        }
    }

    pub(crate) fn array(&self, what: &str) -> Result<&[Self]> {
        if let Self::Array(value) = self {
            Ok(value)
        } else {
            anyhow::bail!("{what} must be a JSON list")
        }
    }

    pub(crate) fn number(&self, what: &str) -> Result<f64> {
        if let Self::Number(value) = self {
            Ok(match value.as_str() {
                "NaN" => f64::NAN,
                "Infinity" => f64::INFINITY,
                "-Infinity" => f64::NEG_INFINITY,
                _ => value.parse()?,
            })
        } else {
            anyhow::bail!("{what} must be a number")
        }
    }

    pub(crate) fn integer(&self, what: &str) -> Result<i64> {
        let number = self.number(what)?;
        ensure!(
            number.is_finite() && number >= i64::MIN as f64 && number < -(i64::MIN as f64),
            "{what} is outside the integer range"
        );
        Ok(number as i64)
    }

    pub(crate) fn string(&self) -> Option<&str> {
        if let Self::String(value) = self {
            Some(value)
        } else {
            None
        }
    }

    pub(crate) fn is_true(&self) -> bool {
        matches!(self, Self::Bool(true))
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Self> {
        if let Self::Object(value) = self {
            value.get(key)
        } else {
            None
        }
    }

    pub(crate) fn display(&self) -> String {
        match self {
            Self::Null => "None".into(),
            Self::Bool(value) => if *value { "True" } else { "False" }.into(),
            Self::String(value) => value.clone(),
            Self::Number(value)
                if !value.contains(['.', 'e', 'E'])
                    && !value.contains("Infinity")
                    && value != "NaN" =>
            {
                if value == "-0" {
                    "0".into()
                } else {
                    value.clone()
                }
            }
            Self::Number(_) => super::format::repr(self.number("number").unwrap()),
            Self::Array(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::display)
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Object(values) => format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(key, value)| format!("{key:?}: {}", value.display()))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }
}

impl Index<&str> for Json {
    type Output = Self;
    fn index(&self, key: &str) -> &Self {
        self.get(key).unwrap_or(&Self::Null)
    }
}

pub(crate) fn read(path: &Path) -> Result<Json> {
    let file = File::from(open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "result_not_regular");
    ensure!(metadata.len() <= MAX_RESULT, "result_too_large");
    let mut contents = Vec::new();
    file.take(MAX_RESULT + 1).read_to_end(&mut contents)?;
    ensure!(
        contents.len() as u64 == metadata.len(),
        "result_size_changed"
    );
    let value = parse(&contents)?;
    value.object(&path.display().to_string())?;
    Ok(value)
}

struct Token {
    start: usize,
    end: usize,
}

fn numeric(token: &str) -> bool {
    if matches!(token, "NaN" | "Infinity" | "-Infinity") {
        return true;
    }
    let bytes = token.as_bytes();
    let mut position = usize::from(bytes.first() == Some(&b'-'));
    if bytes.get(position) == Some(&b'0') {
        position += 1;
    } else {
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == start {
            return false;
        }
    }
    if bytes.get(position) == Some(&b'.') {
        position += 1;
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == start {
            return false;
        }
    }
    if bytes
        .get(position)
        .is_some_and(|byte| matches!(byte, b'e' | b'E'))
    {
        position += 1;
        if bytes
            .get(position)
            .is_some_and(|byte| matches!(byte, b'+' | b'-'))
        {
            position += 1;
        }
        let start = position;
        while bytes.get(position).is_some_and(u8::is_ascii_digit) {
            position += 1;
        }
        if position == start {
            return false;
        }
    }
    position == bytes.len()
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Json> {
    parse_bounded(bytes, MAX_ADAPTED, MAX_ITEMS)
}

fn parse_bounded(bytes: &[u8], maximum: usize, items: usize) -> Result<Json> {
    ensure!(bytes.len() as u64 <= MAX_RESULT, "JSON input too large");
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let source = std::str::from_utf8(bytes)?;
    let budget = maximum.min(source.len().saturating_mul(16).max(64 * 1024));
    ensure!(source.len() <= budget, "adapted JSON too large");
    let mut strings = BTreeSet::new();
    let mut tokens = Vec::new();
    let mut position = 0;
    while position < bytes.len() {
        let start = position;
        if bytes[position] == b'"' {
            position += 1;
            while position < bytes.len() && bytes[position] != b'"' {
                if bytes[position] == b'\\' {
                    position += 1;
                }
                position += 1;
            }
            ensure!(position < bytes.len(), "unterminated JSON string");
            position += 1;
            let value = serde_json::from_slice::<String>(&bytes[start..position])?;
            if !strings.contains(&value) {
                ensure!(strings.len() + tokens.len() < items, "too many JSON items");
                strings.insert(value);
            }
        } else if matches!(bytes[position], b'-' | b'0'..=b'9' | b'N' | b'I') {
            while position < bytes.len() && !b" \t\r\n,]}:".contains(&bytes[position]) {
                position += 1;
            }
            ensure!(numeric(&source[start..position]), "invalid JSON number");
            ensure!(
                bytes[position..]
                    .iter()
                    .copied()
                    .find(|byte| !byte.is_ascii_whitespace())
                    != Some(b':'),
                "JSON object keys must be strings"
            );
            ensure!(strings.len() + tokens.len() < items, "too many JSON items");
            // Bound inventory growth during scanning, before storing another token.
            let minimum = (tokens.len() + 1)
                .checked_mul(MIN_MARKER.len() + 2)
                .and_then(|extra| source.len().checked_add(extra));
            ensure!(
                minimum.is_some_and(|size| size <= budget),
                "adapted JSON too large"
            );
            tokens.push(Token {
                start,
                end: position,
            });
        } else {
            position += 1;
        }
    }
    // Candidate prefixes are disjoint: each collision consumes a distinct string.
    // A range lookup examines only the first possible match, including decoded
    // object keys; long user strings cannot increase marker width or search cost.
    let prefix = (0..=strings.len())
        .map(|counter| format!("@capacity:{counter}:"))
        .find(|prefix| {
            !strings
                .range(prefix.clone()..)
                .next()
                .is_some_and(|value| value.starts_with(prefix))
        })
        .ok_or_else(|| anyhow::anyhow!("JSON marker unavailable"))?;
    let adapted_size = tokens
        .len()
        .checked_mul(prefix.len() + 2)
        .and_then(|extra| source.len().checked_add(extra))
        .filter(|size| *size <= budget)
        .ok_or_else(|| anyhow::anyhow!("adapted JSON too large"))?;
    let mut adapted = String::new();
    adapted.try_reserve_exact(adapted_size)?;
    let mut previous = 0;
    for token in tokens {
        adapted.push_str(&source[previous..token.start]);
        // Validated numeric tokens and the marker are ASCII without escapes.
        adapted.push('"');
        adapted.push_str(&prefix);
        adapted.push_str(&source[token.start..token.end]);
        adapted.push('"');
        previous = token.end;
    }
    adapted.push_str(&source[previous..]);
    fn convert(value: Value, prefix: &str) -> Json {
        match value {
            Value::Null => Json::Null,
            Value::Bool(value) => Json::Bool(value),
            Value::String(value) => match value.strip_prefix(prefix) {
                Some(value) => Json::Number(value.to_owned()),
                None => Json::String(value),
            },
            Value::Array(values) => Json::Array(
                values
                    .into_iter()
                    .map(|value| convert(value, prefix))
                    .collect(),
            ),
            Value::Object(values) => Json::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, convert(value, prefix)))
                    .collect(),
            ),
            Value::Number(_) => {
                unreachable!("every JSON numeric token was retained before parsing")
            }
        }
    }
    Ok(convert(serde_json::from_str(&adapted)?, &prefix))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_private_collision_strings_cannot_amplify_numeric_tokens() {
        let private = format!("@capacity:{}PRIVATE", ":".repeat(2048));
        let source = format!(
            "[{},{}]",
            serde_json::to_string(&private).unwrap(),
            vec!["0"; 2000].join(",")
        );
        // The old marker grew with `private`, producing over 4 MiB here.
        let Json::Array(values) = parse_bounded(source.as_bytes(), 64 * 1024, MAX_ITEMS).unwrap()
        else {
            panic!("expected array")
        };
        assert!(matches!(&values[0], Json::String(value) if value == &private));
        assert_eq!(values.len(), 2001);
        assert!(
            values[1..]
                .iter()
                .all(|value| matches!(value, Json::Number(n) if n == "0"))
        );
    }

    #[test]
    fn decoded_keys_and_values_cannot_collide_with_retained_numeric_tokens() {
        let source = br#"{"\u0040capacity:0:key":["\u0040capacity:1:NaN","@capacity:2:",
            -0,1.00,1e+03,9007199254740993,NaN,Infinity,-Infinity]}"#;
        let value = parse(source).unwrap();
        let Json::Array(values) = &value["@capacity:0:key"] else {
            panic!("expected array")
        };
        assert!(matches!(&values[0], Json::String(v) if v == "@capacity:1:NaN"));
        assert!(matches!(&values[1], Json::String(v) if v == "@capacity:2:"));
        for (value, expected) in values[2..].iter().zip([
            "-0",
            "1.00",
            "1e+03",
            "9007199254740993",
            "NaN",
            "Infinity",
            "-Infinity",
        ]) {
            assert!(matches!(value, Json::Number(token) if token == expected));
        }
    }

    #[test]
    fn adapted_size_and_lexical_inventories_are_bounded_before_construction() {
        assert_eq!(
            parse_bounded(b"[0,0]", 20, MAX_ITEMS)
                .unwrap_err()
                .to_string(),
            "adapted JSON too large"
        );
        assert_eq!(
            parse_bounded(br#"["key",0,1]"#, 1024, 2)
                .unwrap_err()
                .to_string(),
            "too many JSON items"
        );
        assert_eq!(
            parse_bounded(br#"["a","b","c"]"#, 1024, 2)
                .unwrap_err()
                .to_string(),
            "too many JSON items"
        );
        assert!(parse_bounded(br#"["a","a",0]"#, 1024, 2).is_ok());
        let source = format!("[{}]", vec!["0"; 10_000].join(","));
        assert!(parse(source.as_bytes()).is_ok());
    }
}
