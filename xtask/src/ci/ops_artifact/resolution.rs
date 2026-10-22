//! Interpret bounded Cargo observations without claiming read containment or a
//! complete compiler/host I/O closure. No Cargo output is echoed.

use super::{
    HOST, VERSION, failure,
    materials::Materials,
    namespace::{self, OwnedRun},
    sha,
};
use crate::tooling::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Deserialize)]
struct Metadata {
    packages: Vec<Package>,
    workspace_root: PathBuf,
    workspace_members: Vec<String>,
}
#[derive(Deserialize)]
struct Package {
    id: String,
    name: String,
    version: String,
    manifest_path: PathBuf,
    source: Option<String>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Resolution {
    schema_version: u8,
    profile: &'static str,
    original_sha256: String,
    original_bytes: usize,
    packages: usize,
    workspace_members: usize,
    scope: &'static str,
}
pub(super) fn metadata(
    bytes: &[u8],
    run: &OwnedRun,
    materials: &Materials,
    version: &str,
) -> Result<Resolution> {
    if bytes.len() > 16 * 1024 * 1024 {
        return Err(failure(1));
    }
    let value: Metadata = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
    if value.workspace_root != run.path.join("source")
        || value.packages.is_empty()
        || value.packages.len() > 4096
        || value.workspace_members.len() > 64
    {
        return Err(failure(1));
    }
    let mut selected = 0;
    let mut ids = std::collections::BTreeSet::new();
    for package in &value.packages {
        if package.id.len() > 4096
            || !ids.insert(&package.id)
            || !namespace::path(&package.manifest_path)
            || package.manifest_path.file_name() != Some(std::ffi::OsStr::new("Cargo.toml"))
            || !materials.manifest(&package.manifest_path, run)
            || package.source.as_deref().is_some_and(|source| {
                source != "registry+https://github.com/rust-lang/crates.io-index"
            })
        {
            return Err(failure(1));
        }
        if package.manifest_path == run.path.join("source/ops/Cargo.toml") {
            if package.name != "simplestchat-ops"
                || package.version != version
                || package.source.is_some()
                || !value.workspace_members.contains(&package.id)
            {
                return Err(failure(1));
            }
            selected += 1;
        }
    }
    if selected != 1 || value.workspace_members.iter().any(|id| !ids.contains(id)) {
        return Err(failure(1));
    }
    Ok(Resolution {
        schema_version: 1,
        profile: "ops-producer-resolution-v1",
        original_sha256: sha(bytes),
        original_bytes: bytes.len(),
        packages: value.packages.len(),
        workspace_members: value.workspace_members.len(),
        scope: "observedManifestLocationsOnly",
    })
}
pub(super) fn version(bytes: &[u8], cargo: bool) -> Result<serde_json::Value> {
    if bytes.len() > 16 * 1024 {
        return Err(failure(1));
    }
    let value = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
    let lines = value.lines().collect::<Vec<_>>();
    if lines.len() > 64
        || !value.is_ascii()
        || !lines.first().is_some_and(|line| {
            line.starts_with(&format!(
                "{} {VERSION} (",
                if cargo { "cargo" } else { "rustc" }
            ))
        })
        || lines
            .iter()
            .filter(|line| **line == format!("host: {HOST}"))
            .count()
            != 1
        || lines
            .iter()
            .filter(|line| **line == format!("release: {VERSION}"))
            .count()
            != 1
    {
        return Err(failure(1));
    }
    Ok(serde_json::json!({"version":VERSION,"host":HOST,"sha256":sha(bytes),"bytes":bytes.len()}))
}
pub(super) fn sysroot(bytes: &[u8], run: &OwnedRun) -> Result<serde_json::Value> {
    if bytes.len() > 4096 {
        return Err(failure(1));
    }
    let expected = format!("{}\n", run.path.join("kit/toolchain").display());
    if bytes != expected.as_bytes() {
        return Err(failure(1));
    }
    Ok(serde_json::json!({"sha256":sha(bytes),"bytes":bytes.len(),"selectedStagedSysroot":true}))
}
