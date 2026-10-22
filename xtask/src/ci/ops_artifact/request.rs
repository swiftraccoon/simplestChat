//! A closed original-byte-pinned request. Paths and JSON never enter errors.

use super::{digest, failure, namespace, sha};
use crate::tooling::Result;
use serde::Deserialize;
use serde_json::value::RawValue;
use simplestchat_ops::distribution::manifest::Selection;
use std::path::{Path, PathBuf};

pub(super) const MAX_REQUEST: u64 = 64 * 1024;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    selection: Box<RawValue>,
    source: Material,
    #[serde(rename = "builderKit")]
    builder_kit: Material,
    output: PathBuf,
}
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Material {
    pub path: PathBuf,
    pub sha256: String,
}

/// Debug deliberately omits original JSON, all paths and material-map details.
pub(super) struct Request {
    pub selection: Selection,
    pub source: Material,
    pub kit: Material,
    pub output: PathBuf,
    pub sha256: String,
    pub bytes: usize,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOpsProducerRequest")
    }
}
impl Request {
    pub fn decode(bytes: &[u8], selected: &str) -> Result<Self> {
        if bytes.len() as u64 > MAX_REQUEST || !digest(selected) || sha(bytes) != selected {
            return Err(failure(1));
        }
        let value: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        let selection =
            Selection::decode(value.selection.get().as_bytes()).map_err(|_| failure(1))?;
        if value.schema_version != 1
            || !namespace::path(&value.source.path)
            || !namespace::path(&value.builder_kit.path)
            || !namespace::path(&value.output)
            || value.output.components().count() > 20
            || value.output.as_os_str().as_encoded_bytes().contains(&b':')
            || !digest(&value.source.sha256)
            || !digest(&value.builder_kit.sha256)
            || value.source.path.starts_with(&value.builder_kit.path)
            || value.builder_kit.path.starts_with(&value.source.path)
            || value.output.starts_with(&value.source.path)
            || value.output.starts_with(&value.builder_kit.path)
            || value.source.path.starts_with(&value.output)
            || value.builder_kit.path.starts_with(&value.output)
        {
            return Err(failure(1));
        }
        Ok(Self {
            selection,
            source: value.source,
            kit: value.builder_kit,
            output: value.output,
            sha256: selected.to_owned(),
            bytes: bytes.len(),
        })
    }
    pub fn read(path: &Path, selected: &str, budget: &namespace::Budget) -> Result<Self> {
        let bytes = namespace::read(path, MAX_REQUEST, budget)?;
        Self::decode(&bytes, selected)
    }
}
