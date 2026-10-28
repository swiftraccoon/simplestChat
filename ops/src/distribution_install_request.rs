//! Closed original-byte pins; caller selection is not certificate evidence.

use super::{Failure, Result};
use crate::json::{self, Json};
use std::{
    collections::BTreeMap,
    path::{Component, Path, PathBuf},
};

pub(super) const MAXIMUM: u64 = 16 * 1024;
pub(super) const DESTINATION: &str = "usr/local/libexec/simplestchat-public";
pub(super) const NAME: &str = "simplestchat-ops";

#[derive(Clone)]
pub(super) struct Pin {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}
pub(super) struct Request {
    pub manifest: Pin,
    pub selection: Pin,
    pub binary: Pin,
}
/// The old canonical executable expected by an authenticated delivery caller.
/// This is custody policy, not signature evidence or a caller-selected path.
pub(super) struct InstalledBinary {
    pub sha256: String,
    pub bytes: u64,
}
pub(super) struct UpgradeRequest {
    pub candidate: Request,
    pub installed: InstalledBinary,
}
pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub(super) fn path(value: &Path) -> Result<()> {
    if !value.is_absolute()
        || value
            .to_str()
            .is_none_or(|s| s.len() > 4096 || s.chars().any(char::is_control))
        || value.components().count() > 32
        || value.file_name().is_none()
        || value
            .to_str()
            .is_none_or(|s| s[1..].split('/').any(|p| matches!(p, "" | "." | "..")))
        || value
            .components()
            .any(|c| !matches!(c, Component::RootDir | Component::Normal(_)))
        || value.starts_with(Path::new("/").join(DESTINATION))
    {
        return Err(Failure::Unproven);
    }
    Ok(())
}
fn object<'a>(value: &'a Json, keys: &[&str]) -> Result<&'a BTreeMap<String, Json>> {
    let object = value.object().ok_or(Failure::Unproven)?;
    if object.len() != keys.len() || keys.iter().any(|key| !object.contains_key(*key)) {
        return Err(Failure::Unproven);
    }
    Ok(object)
}
fn pin(value: &Json, maximum: u64) -> Result<Pin> {
    let value = object(value, &["path", "sha256", "bytes"])?;
    let path = PathBuf::from(value["path"].string().ok_or(Failure::Unproven)?);
    path.as_path().file_name().ok_or(Failure::Unproven)?;
    self::path(&path)?;
    let sha256 = value["sha256"].string().ok_or(Failure::Unproven)?;
    let bytes = value["bytes"]
        .integer()
        .and_then(|n| u64::try_from(n).ok())
        .filter(|n| *n > 0 && *n <= maximum)
        .ok_or(Failure::Unproven)?;
    if !digest(sha256) {
        return Err(Failure::Unproven);
    }
    Ok(Pin {
        path,
        sha256: sha256.into(),
        bytes,
    })
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() as u64 > MAXIMUM
            || bytes.starts_with(b"\xef\xbb\xbf")
            || bytes.contains(&0)
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(Failure::Unproven);
        }
        let json = json::decode_limited(bytes, MAXIMUM as usize).map_err(|_| Failure::Unproven)?;
        let value = object(&json, &["schemaVersion", "manifest", "selection", "binary"])?;
        if !matches!(&value["schemaVersion"], Json::Integer(n) if n == "1") {
            return Err(Failure::Unproven);
        }
        Ok(Self {
            manifest: pin(&value["manifest"], 64 * 1024)?,
            selection: pin(&value["selection"], 16 * 1024)?,
            binary: pin(&value["binary"], super::super::MAX_BYTES)?,
        })
    }
}

impl UpgradeRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() as u64 > MAXIMUM
            || bytes.starts_with(b"\xef\xbb\xbf")
            || bytes.contains(&0)
            || std::str::from_utf8(bytes).is_err()
        {
            return Err(Failure::Unproven);
        }
        let json = json::decode_limited(bytes, MAXIMUM as usize).map_err(|_| Failure::Unproven)?;
        let value = object(
            &json,
            &[
                "schemaVersion",
                "manifest",
                "selection",
                "binary",
                "installedBinary",
            ],
        )?;
        if !matches!(&value["schemaVersion"], Json::Integer(n) if n == "2") {
            return Err(Failure::Unproven);
        }
        let installed = object(&value["installedBinary"], &["sha256", "bytes"])?;
        let sha256 = installed["sha256"].string().ok_or(Failure::Unproven)?;
        let bytes = installed["bytes"]
            .integer()
            .and_then(|n| u64::try_from(n).ok())
            .filter(|n| *n > 0 && *n <= super::super::MAX_BYTES)
            .ok_or(Failure::Unproven)?;
        if !digest(sha256) {
            return Err(Failure::Unproven);
        }
        let candidate = Request {
            manifest: pin(&value["manifest"], 64 * 1024)?,
            selection: pin(&value["selection"], 16 * 1024)?,
            binary: pin(&value["binary"], super::super::MAX_BYTES)?,
        };
        if candidate.binary.sha256 == sha256 {
            return Err(Failure::Unproven);
        }
        Ok(Self {
            candidate,
            installed: InstalledBinary {
                sha256: sha256.into(),
                bytes,
            },
        })
    }
}
