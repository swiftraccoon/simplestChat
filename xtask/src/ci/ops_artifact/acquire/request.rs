//! A closed original-byte selection; no caller URL or credential enters JSON.

use super::failure;
use crate::tooling::{Environment, Result};
use serde::{Deserialize, Deserializer};
use serde_json::value::RawValue;
use simplestchat_ops::distribution::manifest::Selection;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

pub(super) const MAXIMUM: u64 = 16 * 1024;
const SAFE: u64 = 9_007_199_254_740_991;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Authentication {
    GhToken,
}
#[derive(Default)]
enum ArtifactZip {
    #[default]
    Absent,
    Retained(PathBuf),
}
impl<'de> Deserialize<'de> for ArtifactZip {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        PathBuf::deserialize(deserializer).map(Self::Retained)
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wire {
    schema_version: u8,
    selection: Box<RawValue>,
    artifact_id: u64,
    output: PathBuf,
    authentication: Authentication,
    #[serde(default)]
    artifact_zip: ArtifactZip,
}
pub(super) struct Request {
    pub selection: Selection,
    pub artifact_id: u64,
    pub output: PathBuf,
    pub artifact_zip: Option<PathBuf>,
    pub sha256: String,
    pub bytes: usize,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOpsAcquisitionRequest")
    }
}
pub(super) fn path(value: &Path) -> bool {
    super::super::namespace::path(value)
        && value.file_name().is_some()
        && value.components().count() <= 17
        && value
            .to_str()
            .is_some_and(|s| !s.chars().any(char::is_control))
}
impl Request {
    pub fn decode(bytes: &[u8], selected: &str) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() as u64 > MAXIMUM
            || !super::super::digest(selected)
            || super::super::sha(bytes) != selected
        {
            return Err(failure(1));
        }
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        let selection =
            Selection::decode(wire.selection.get().as_bytes()).map_err(|_| failure(1))?;
        let identity = serde_json::to_value(selection.identity()).map_err(|_| failure(1))?;
        if wire.schema_version != 1
            || wire.artifact_id == 0
            || wire.artifact_id > SAFE
            || identity["repository"] != "swiftraccoon/simplestChat"
            || ["buildRunId", "runAttempt"]
                .iter()
                .any(|key| identity[*key].as_u64().is_none_or(|n| n == 0 || n > SAFE))
            || !path(&wire.output)
            || wire.output.components().count() > 16
        {
            return Err(failure(1));
        }
        let Authentication::GhToken = wire.authentication;
        let artifact_zip = match wire.artifact_zip {
            ArtifactZip::Absent => None,
            ArtifactZip::Retained(source) => {
                if !path(&source)
                    || source.starts_with(&wire.output)
                    || wire.output.starts_with(&source)
                {
                    return Err(failure(1));
                }
                Some(source)
            }
        };
        Ok(Self {
            selection,
            artifact_id: wire.artifact_id,
            output: wire.output,
            artifact_zip,
            sha256: selected.into(),
            bytes: bytes.len(),
        })
    }
    pub fn token(environment: &Environment) -> Result<std::ffi::OsString> {
        let value = environment
            .get(OsStr::new("GH_TOKEN"))
            .ok_or_else(|| failure(1))?;
        let bytes = value.as_encoded_bytes();
        if bytes.is_empty() || bytes.len() > 16 * 1024 || bytes.contains(&0) {
            return Err(failure(1));
        }
        Ok(value.clone())
    }
}
