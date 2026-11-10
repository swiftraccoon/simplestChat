//! Explicit caller selections; neither repository spelling nor pins authenticate origin.

use super::*;
use serde::Deserialize;
use serde_json::value::RawValue;
use simplestchat_ops::distribution::manifest::Selection;
use std::path::{Path, PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    selection: Box<RawValue>,
    repository: PathBuf,
    git: GitSelection,
    output: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GitSelection {
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
}
pub(super) struct Request {
    pub selection: Selection,
    pub revision: String,
    pub repository: PathBuf,
    pub git: GitSelection,
    pub output: PathBuf,
    pub sha256: String,
    pub bytes: usize,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOpsSourceRequest")
    }
}
fn disjoint(a: &Path, b: &Path) -> bool {
    !a.starts_with(b) && !b.starts_with(a)
}
impl Request {
    pub fn decode(bytes: &[u8], selected: &str) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() > 64 * 1024
            || !digest(selected)
            || sha(bytes) != selected
        {
            return Err(failure(1));
        }
        let value: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        let selection =
            Selection::decode(value.selection.get().as_bytes()).map_err(|_| failure(1))?;
        let identity = serde_json::to_value(selection.identity()).map_err(|_| failure(1))?;
        let revision = identity["revision"]
            .as_str()
            .ok_or_else(|| failure(1))?
            .to_owned();
        if value.schema_version != 1
            || !namespace::path(&value.repository)
            || !namespace::path(&value.git.path)
            || !namespace::path(&value.output)
            || value.output.components().count() > 20
            || value.git.bytes == 0
            || value.git.bytes > 64 * 1024 * 1024
            || !digest(&value.git.sha256)
            || !disjoint(&value.repository, &value.output)
            || !disjoint(&value.git.path, &value.output)
        {
            return Err(failure(1));
        }
        Ok(Self {
            selection,
            revision,
            repository: value.repository,
            git: value.git,
            output: value.output,
            sha256: selected.into(),
            bytes: bytes.len(),
        })
    }
    pub fn read(path: &Path, selected: &str, budget: &Budget) -> Result<Self> {
        let bytes = namespace::read(path, 64 * 1024, budget)?;
        let value = Self::decode(&bytes, selected)?;
        if !disjoint(path, &value.output) {
            return Err(failure(1));
        }
        Ok(value)
    }
}
