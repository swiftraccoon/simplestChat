//! File and JSON primitives that preserve the Python reference's framing.

use super::require;
use anyhow::Result;
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

pub(super) fn regular(path: &Path, limit: u64, nonempty: bool) -> Result<File> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    require(
        metadata.is_file(),
        if nonempty {
            "nonregular_tool_file"
        } else {
            "ci_cache_regular_file"
        },
    )?;
    require(
        metadata.len() <= limit && (!nonempty || metadata.len() > 0),
        if nonempty {
            "tool_file_size"
        } else {
            "ci_cache_file_size"
        },
    )?;
    Ok(file)
}

pub(super) fn bounded(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = regular(path, limit, true)?;
    let length = file.metadata()?.len();
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    require(bytes.len() as u64 == length, "tool_file_changed")?;
    Ok(bytes)
}

pub(super) fn digest(path: &Path) -> Result<String> {
    let mut file = regular(path, 1024 * 1024 * 1024, false)?;
    let mut hashed = Sha256::new();
    let mut buffer = [0_u8; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hashed.update(&buffer[..count]);
    }
    Ok(hex(&hashed.finalize()))
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    text
}

/// Python json.dumps defaults to ASCII escapes, including UTF-16 surrogate pairs.
pub(super) fn json_string(value: &str) -> String {
    let mut result = String::from("\"");
    for character in value.chars() {
        match character {
            '"' => result.push_str("\\\""),
            '\\' => result.push_str("\\\\"),
            '\u{0008}' => result.push_str("\\b"),
            '\u{000c}' => result.push_str("\\f"),
            '\n' => result.push_str("\\n"),
            '\r' => result.push_str("\\r"),
            '\t' => result.push_str("\\t"),
            ' '..='~' => result.push(character),
            _ => {
                for unit in character.encode_utf16(&mut [0; 2]) {
                    result.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    result.push('"');
    result
}

/// Sorted string objects use Python's default comma/colon spacing, not compact JSON.
pub(super) fn json_object(values: &BTreeMap<String, String>) -> String {
    let entries = values
        .iter()
        .map(|(key, value)| format!("{}: {}", json_string(key), json_string(value)))
        .collect::<Vec<_>>();
    format!("{{{}}}", entries.join(", "))
}

/// Path.resolve(strict=False): resolve the existing prefix and retain a missing
/// generated tail. Comparing this result with a dependency still rejects aliases.
pub(super) fn resolve(path: &Path) -> Result<PathBuf> {
    let mut prefix = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut tail = Vec::new();
    while !prefix.exists() {
        let name = prefix
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("ci_cache_path"))?;
        tail.push(name.to_owned());
        require(prefix.pop(), "ci_cache_path")?;
    }
    let mut result = prefix.canonicalize()?;
    for name in tail.into_iter().rev() {
        result.push(name);
    }
    let mut normalized = PathBuf::new();
    for component in result.components() {
        if component == Component::ParentDir {
            normalized.pop();
        } else {
            normalized.push(component);
        }
    }
    Ok(normalized)
}
