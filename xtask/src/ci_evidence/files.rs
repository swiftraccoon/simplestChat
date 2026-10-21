//! Bounded regular input and private, exclusive Python-compatible JSON output.

use anyhow::{Result, ensure};
use rustix::fs::{Mode, OFlags};
use serde_json::Value;
use std::fs::{DirBuilder, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

pub(super) const MAX_DOCUMENT: u64 = 1024 * 1024;

pub(super) fn bounded(path: &Path) -> Result<Vec<u8>> {
    let descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )?;
    let file = File::from(descriptor);
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "nonregular_tool_file");
    ensure!(
        metadata.len() > 0 && metadata.len() <= MAX_DOCUMENT,
        "tool_file_size"
    );
    let mut bytes = Vec::new();
    file.take(MAX_DOCUMENT + 1).read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 == metadata.len(), "tool_file_changed");
    Ok(bytes)
}

pub(super) fn document(path: &Path) -> Result<Value> {
    let document: Value = serde_json::from_slice(&bounded(path)?)?;
    ensure!(document.is_object(), "scanner_object_expected");
    Ok(document)
}

/// Match `json.dumps(sort_keys=True, indent=2)` including ASCII surrogate pairs.
pub(super) fn json_bytes(value: &Value) -> Result<Vec<u8>> {
    let text = serde_json::to_string_pretty(value)?;
    let mut output = String::with_capacity(text.len() + 1);
    for character in text.chars() {
        if !character.is_ascii() || character == '\u{007f}' {
            for unit in character.encode_utf16(&mut [0; 2]) {
                output.push_str(&format!("\\u{unit:04x}"));
            }
        } else {
            output.push(character);
        }
    }
    output.push('\n');
    Ok(output.into_bytes())
}

pub(super) fn write_new(path: &Path, value: &Value) -> Result<()> {
    let bytes = json_bytes(value)?;
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("local_ci_evidence_path"))?;
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(&bytes)?;
    file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    Ok(())
}
