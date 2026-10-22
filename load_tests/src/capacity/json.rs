//! Bounded calibration JSON with Python's original numeric-token semantics.
//!
//! Serde owns structural JSON parsing. A lexical adapter retains number tokens
//! for correctly rounded f64 conversion and the reference's NaN/Infinity values;
//! it neither accepts comments/JSON5 nor reinterprets strings or object keys.

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
    let bytes = bytes.strip_prefix(&[0xef, 0xbb, 0xbf]).unwrap_or(bytes);
    let source = std::str::from_utf8(bytes)?;
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
            strings.insert(serde_json::from_slice::<String>(&bytes[start..position])?);
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
            tokens.push(Token {
                start,
                end: position,
            });
        } else {
            position += 1;
        }
    }
    let mut prefix = "@capacity:".to_owned();
    while strings.iter().any(|value| value.starts_with(&prefix)) {
        prefix.push(':');
    }
    let mut adapted = String::new();
    let mut previous = 0;
    for token in tokens {
        adapted.push_str(&source[previous..token.start]);
        adapted.push_str(&serde_json::to_string(&format!(
            "{prefix}{}",
            &source[token.start..token.end]
        ))?);
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
