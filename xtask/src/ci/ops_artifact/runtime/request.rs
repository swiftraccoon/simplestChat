//! Closed, byte-pinned selection. No host, executable or bootstrap path is input.

use super::failure;
use crate::tooling::Result;
use serde::Deserialize;
use serde_json::value::RawValue;
use simplestchat_ops::distribution::manifest::Selection;
use std::path::{Path, PathBuf};

pub(super) const MAXIMUM: u64 = 16 * 1024;
const SAFE: u64 = 9_007_199_254_740_991;
#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Authentication {
    GhToken,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Wire {
    schema_version: u8,
    selection: Box<RawValue>,
    artifact_id: u64,
    output: PathBuf,
    authentication: Authentication,
}
pub(super) struct Request {
    pub selection: Selection,
    pub selected_bytes: Vec<u8>,
    pub artifact_id: u64,
    pub output: PathBuf,
    pub sha256: String,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOpsDebianRuntimeRequest")
    }
}
pub(super) fn path(path: &Path) -> bool {
    super::super::namespace::path(path)
        && path.file_name().is_some()
        && path.components().count() <= 17
        && path
            .to_str()
            .is_some_and(|s| !s.chars().any(char::is_control))
}
impl Request {
    pub fn decode(bytes: &[u8], expected: &str) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() as u64 > MAXIMUM
            || !super::super::digest(expected)
            || super::super::sha(bytes) != expected
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
            || identity["packageVersion"] != env!("CARGO_PKG_VERSION")
            || identity["target"] != "x86_64-unknown-linux-musl"
            || identity["protocolVersion"] != 1
            || ["buildRunId", "runAttempt"]
                .iter()
                .any(|key| identity[*key].as_u64().is_none_or(|n| n == 0 || n > SAFE))
            || !path(&wire.output)
            || wire.output.components().count() > 14
        {
            return Err(failure(1));
        }
        let Authentication::GhToken = wire.authentication;
        let selected_bytes = serde_json::to_vec(&selection.identity()).map_err(|_| failure(1))?;
        Ok(Self {
            selection,
            selected_bytes,
            artifact_id: wire.artifact_id,
            output: wire.output,
            sha256: expected.into(),
        })
    }
    pub fn acquire_request(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(&serde_json::json!({
            "schemaVersion":1, "selection":self.selection.identity(),
            "artifactId":self.artifact_id, "output":self.output.join("acquired"),
            "authentication":"gh-token"
        }))
        .map_err(|_| failure(1))
    }
}
