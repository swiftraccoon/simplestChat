//! Original caller-selected bytes select local material, not its provenance.

use super::*;
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    #[serde(rename = "schemaVersion")]
    schema_version: u8,
    lock: Lock,
    #[serde(rename = "archivesRoot")]
    archives_root: PathBuf,
    output: PathBuf,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Lock {
    pub path: PathBuf,
    pub sha256: String,
}
pub(super) struct Request {
    pub lock: Lock,
    pub archives_root: PathBuf,
    pub output: PathBuf,
    pub sha256: String,
    pub bytes: usize,
}
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PinnedOfflineRegistryRequest")
    }
}
fn disjoint(a: &Path, b: &Path) -> bool {
    !a.starts_with(b) && !b.starts_with(a)
}
impl Request {
    pub fn decode(bytes: &[u8], selected: &str) -> Result<Self> {
        if bytes.is_empty() || bytes.len() > 65536 || !digest(selected) || sha(bytes) != selected {
            return Err(failure(1));
        }
        let value: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        if value.schema_version != 1
            || !digest(&value.lock.sha256)
            || [&value.lock.path, &value.archives_root, &value.output]
                .into_iter()
                .any(|p| !namespace::path(p) || p.components().count() > 20)
            || !disjoint(&value.archives_root, &value.output)
            || !disjoint(&value.lock.path, &value.output)
        {
            return Err(failure(1));
        }
        Ok(Self {
            lock: value.lock,
            archives_root: value.archives_root,
            output: value.output,
            sha256: selected.into(),
            bytes: bytes.len(),
        })
    }
    pub fn read(path: &Path, selected: &str, budget: &Budget) -> Result<Self> {
        let value = Self::decode(&namespace::read(path, 65536, budget)?, selected)?;
        if !disjoint(path, &value.output) {
            return Err(failure(1));
        }
        Ok(value)
    }
}
