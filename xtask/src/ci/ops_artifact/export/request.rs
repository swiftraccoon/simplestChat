//! Closed input roles. The remote directory is a fixed bootstrap namespace,
//! beneath the executable helper ancestor, not an arbitrary destination or
//! authentication statement. The transport caller checks the host mount policy.

use super::failure;
use crate::tooling::Result;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{json, value::RawValue};
use simplestchat_ops::distribution::manifest::Selection;
use std::path::{Path, PathBuf};

pub(super) const MAXIMUM: u64 = 16 * 1024;
const SAFE: u64 = 9_007_199_254_740_991;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct BinaryPin {
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Default)]
enum Installed {
    #[default]
    Absent,
    Expected(BinaryPin),
}
impl<'de> Deserialize<'de> for Installed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        BinaryPin::deserialize(deserializer).map(Self::Expected)
    }
}
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
    remote_input: PathBuf,
    #[serde(default)]
    installed_binary: Installed,
}
pub(super) struct Request {
    pub selection: Selection,
    pub artifact_id: u64,
    pub output: PathBuf,
    pub remote_input: PathBuf,
    pub installed: Option<BinaryPin>,
    pub sha256: String,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PinnedOpsDistributionExportRequest")
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
fn remote(path: &Path) -> bool {
    let Some(text) = path.to_str() else {
        return false;
    };
    let Some(nonce) = text
        .strip_prefix("/usr/local/libexec/simplestchat-ops-bootstrap/")
        .and_then(|tail| tail.strip_suffix("/input"))
    else {
        return false;
    };
    nonce.len() == 32
        && nonce
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
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
            || identity["packageVersion"] != env!("CARGO_PKG_VERSION")
            || identity["target"] != "x86_64-unknown-linux-musl"
            || identity["protocolVersion"] != 1
            || ["buildRunId", "runAttempt"]
                .iter()
                .any(|key| identity[*key].as_u64().is_none_or(|n| n == 0 || n > SAFE))
            || !path(&wire.output)
            || wire.output.components().count() > 14
            || !remote(&wire.remote_input)
        {
            return Err(failure(1));
        }
        let Authentication::GhToken = wire.authentication;
        let installed = match wire.installed_binary {
            Installed::Absent => None,
            Installed::Expected(pin) => {
                if !super::super::digest(&pin.sha256)
                    || pin.bytes == 0
                    || pin.bytes > 256 * 1024 * 1024
                {
                    return Err(failure(1));
                }
                Some(pin)
            }
        };
        Ok(Self {
            selection,
            artifact_id: wire.artifact_id,
            output: wire.output,
            remote_input: wire.remote_input,
            installed,
            sha256: selected.into(),
        })
    }
    pub fn acquire(&self) -> Result<Vec<u8>> {
        serde_json::to_vec(
            &json!({"schemaVersion":1,"selection":self.selection.identity(),
            "artifactId":self.artifact_id,"output":self.output.join("acquired"),
            "authentication":"gh-token"}),
        )
        .map_err(|_| failure(1))
    }
}
