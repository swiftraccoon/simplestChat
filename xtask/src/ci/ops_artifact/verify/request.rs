//! Exact caller selection; none of these fields supplies verifier authority.

use super::failure;
use crate::tooling::{Environment, Result};
use serde::Deserialize;
use serde_json::value::RawValue;
use simplestchat_ops::distribution::manifest::Selection;
use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

const MAX_REQUEST: usize = 16 * 1024;
const SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Authentication {
    GhToken,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    selection: Box<RawValue>,
    directory: PathBuf,
    output: PathBuf,
    authentication: Authentication,
}
#[derive(Clone)]
pub(super) struct Request {
    pub selection: Selection,
    pub directory: PathBuf,
    pub output: PathBuf,
    pub authentication: Authentication,
    pub sha256: String,
    pub bytes: usize,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedRetainedOpsRequest")
    }
}
pub(super) fn path(value: &Path) -> bool {
    super::super::namespace::path(value)
        && value
            .to_str()
            .is_some_and(|text| !text.chars().any(char::is_control))
        && value.components().count() <= 17
}
impl Request {
    pub fn decode(bytes: &[u8], selected: &str) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() > MAX_REQUEST
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
            || identity["repository"] != "swiftraccoon/simplestChat"
            || ["buildRunId", "runAttempt"]
                .iter()
                .any(|key| identity[*key].as_u64().is_none_or(|n| n == 0 || n > SAFE))
            || !path(&wire.directory)
            || !path(&wire.output)
            || wire.output.starts_with(&wire.directory)
            || wire.directory.starts_with(&wire.output)
        {
            return Err(failure(1));
        }
        Ok(Self {
            selection,
            directory: wire.directory,
            output: wire.output,
            authentication: wire.authentication,
            sha256: selected.into(),
            bytes: bytes.len(),
        })
    }
    pub fn token(&self, environment: &Environment) -> Result<std::ffi::OsString> {
        match self.authentication {
            Authentication::GhToken => {
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
    }
    pub fn maximum_bytes() -> u64 {
        MAX_REQUEST as u64
    }
}
