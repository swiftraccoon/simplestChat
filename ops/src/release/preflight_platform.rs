//! CPython's selected os-release field semantics without shell execution.

use super::{Failure, Result};

pub(super) struct Identity {
    pub euid: u32,
    pub system: Vec<u8>,
    pub machine: Vec<u8>,
}
impl Identity {
    pub fn validate(self) -> Result<()> {
        if self.euid != 0 {
            return Err(Failure::Root);
        }
        if self.system != b"Linux" || self.machine != b"x86_64" {
            return Err(Failure::Platform);
        }
        Ok(())
    }
}

/// Match platform._parse_os_release's two selected values, including last-key
/// wins, optional whole quotes, five escapes and text-file universal newlines.
/// Unknown fields are ignored and none of these bytes become diagnostic output.
pub(super) fn distribution(bytes: &[u8]) -> Result<()> {
    let text = std::str::from_utf8(bytes).map_err(|_| Failure::Unavailable)?;
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut id = "linux".to_owned();
    let mut version = String::new();
    for line in normalized.split('\n') {
        let Some((key, mut value)) = line.split_once('=') else {
            continue;
        };
        if key.is_empty() || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
            continue;
        }
        if value.len() >= 2
            && matches!(value.as_bytes()[0], b'\'' | b'"')
            && value.as_bytes().last() == value.as_bytes().first()
        {
            value = &value[1..value.len() - 1];
        }
        if !matches!(key, "ID" | "VERSION_ID") {
            continue;
        }
        let mut decoded = String::new();
        let mut chars = value.chars().peekable();
        while let Some(character) = chars.next() {
            if character == '\\'
                && chars
                    .peek()
                    .is_some_and(|next| matches!(next, '\\' | '$' | '"' | '\'' | '`'))
            {
                decoded.push(chars.next().unwrap());
            } else {
                decoded.push(character);
            }
        }
        if key == "ID" {
            id = decoded;
        } else {
            version = decoded;
        }
    }
    if id == "debian" && version.split('.').next() == Some("13") {
        Ok(())
    } else {
        Err(Failure::Distribution)
    }
}
