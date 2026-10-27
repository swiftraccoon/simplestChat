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
    resolve: Resolve,
}
#[derive(Deserialize)]
struct Resolve {
    nodes: Vec<Node>,
}
#[derive(Deserialize)]
struct Node {
    id: String,
    features: Vec<String>,
    deps: Vec<Dependency>,
}
#[derive(Deserialize)]
struct Dependency {
    name: String,
    pkg: String,
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
    ops_features: Vec<String>,
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
    let mut ops_id = None;
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
            ops_id = Some(&package.id);
        }
    }
    if selected != 1 || value.workspace_members.iter().any(|id| !ids.contains(id)) {
        return Err(failure(1));
    }
    let mut nodes = std::collections::BTreeSet::new();
    for node in &value.resolve.nodes {
        if !ids.contains(&node.id)
            || !nodes.insert(&node.id)
            || node.features.len() > 512
            || node.deps.len() > 4096
            || node.deps.iter().any(|dep| !ids.contains(&dep.pkg))
        {
            return Err(failure(1));
        }
    }
    let selected = value
        .resolve
        .nodes
        .iter()
        .find(|node| Some(&node.id) == ops_id)
        .ok_or_else(|| failure(1))?;
    if selected.features != ["maintain-guest"] {
        return Err(failure(1));
    }
    for (edge, name, version) in [
        ("asn1_rs", "asn1-rs", "0.7.2"),
        ("openssl", "openssl", "0.10.81"),
        ("x509_parser", "x509-parser", "0.18.1"),
    ] {
        let dependencies = selected
            .deps
            .iter()
            .filter(|dep| dep.name == edge)
            .collect::<Vec<_>>();
        if dependencies.len() != 1
            || !value.packages.iter().any(|package| {
                package.id == dependencies[0].pkg
                    && package.name == name
                    && package.version == version
                    && package.source.as_deref()
                        == Some("registry+https://github.com/rust-lang/crates.io-index")
            })
        {
            return Err(failure(1));
        }
        let node = value
            .resolve
            .nodes
            .iter()
            .find(|node| node.id == dependencies[0].pkg)
            .ok_or_else(|| failure(1))?;
        if edge == "asn1_rs" && !node.features.iter().any(|feature| feature == "bigint") {
            return Err(failure(1));
        }
    }
    Ok(Resolution {
        schema_version: 1,
        profile: "ops-producer-resolution-v2",
        original_sha256: sha(bytes),
        original_bytes: bytes.len(),
        packages: value.packages.len(),
        workspace_members: value.workspace_members.len(),
        scope: "observedOpsFeaturesAndManifestLocations",
        ops_features: selected.features.clone(),
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
