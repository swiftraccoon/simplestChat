//! What the Python package index says about a locked dependency.
//!
//! The index lists every file of a package with its SHA-256. Each locked
//! hash must be one of them, under a file name of the locked name and
//! version, at the package host. A wheel's metadata is fetched from beside
//! it and must have the SHA-256 the index gives for it; a source
//! distribution is only recorded, because nothing is installed from one.

use super::{
    Dependency,
    artifact::{sdist, wheel},
    metadata::{Reviewed, metadata_license},
    registry::{Evidence, Registry},
    requirement::canonical,
};
use crate::{
    Fault,
    context::{Context, Outcome},
    digest, record, require,
};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};

/// The host of the files.
const FILES: &str = "https://files.pythonhosted.org/";
const INDEX: Fault = Fault("dependency_python_index_format");
const ORIGIN: Fault = Fault("dependency_python_wheel_origin");
const HASH: Fault = Fault("dependency_python_metadata_hash");

/// The text `record` holds under `field`.
fn said<'a>(record: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    record.get(field).and_then(Value::as_str)
}

/// The SHA-256 `record` holds in the record under `field`.
fn hashed<'a>(record: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    said(record.get(field)?.as_object()?, "sha256")
}

/// The evidence for every locked artifact of the Python package
/// `dependency`: one licence for each wheel, from its own metadata or from
/// `reviewed`, and at least one wheel.
pub fn python_licenses(
    context: &Context,
    registry: &mut Registry<'_>,
    dependency: &Dependency,
    reviewed: &[Reviewed<'_>],
) -> Outcome<Vec<Evidence>> {
    let bounds = *registry.bounds();
    let url = format!("https://pypi.org/simple/{}/", dependency.name);
    let page = registry.fetch(context, &url, bounds.index)?;
    let document = record::strict(&page).map_err(|_| INDEX)?;
    let index = document.as_object().ok_or(INDEX)?;
    let name = said(index, "name").ok_or(INDEX)?;
    require(
        canonical(name) == dependency.name,
        "dependency_python_index_identity",
    )?;
    let files = index.get("files").and_then(Value::as_array).ok_or(INDEX)?;
    require(files.len() <= bounds.files, "dependency_python_index_count")?;
    let mut evidence = Vec::new();
    let mut licenses: BTreeMap<&str, String> = BTreeMap::new();
    let mut found = BTreeSet::new();
    let mut wheels = 0;
    let locked = |named: Option<(String, String)>| {
        named
            .is_some_and(|(name, version)| name == dependency.name && version == dependency.version)
    };
    for file in files {
        let file = file.as_object().ok_or(INDEX)?;
        let artifact = hashed(file, "hashes").ok_or(INDEX)?;
        let filename = said(file, "filename").ok_or(INDEX)?;
        if !dependency.hashes.contains(artifact) {
            continue;
        }
        let source = said(file, "url").ok_or(ORIGIN)?;
        let below = source.strip_prefix(FILES).is_some_and(|path| {
            path.bytes()
                .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'?' | b'#'))
        });
        let ends = source
            .strip_suffix(filename)
            .is_some_and(|before| before.ends_with('/'));
        require(below && ends, ORIGIN.0)?;
        found.insert(artifact);
        if !filename.ends_with(".whl") {
            require(locked(sdist(filename)), "dependency_python_sdist_identity")?;
            evidence.push(Evidence::Sdist {
                source: source.to_owned(),
                artifact: artifact.to_owned(),
            });
            continue;
        }
        require(locked(wheel(filename)), "dependency_python_wheel_identity")?;
        let lower = |byte: u8| matches!(byte, b'0'..=b'9' | b'a'..=b'f');
        let metadata = hashed(file, "core-metadata")
            .filter(|hash| hash.len() == 64 && hash.bytes().all(lower))
            .ok_or(HASH)?;
        let license = match licenses.get(metadata) {
            Some(license) => license.clone(),
            None => {
                let address = format!("{source}.metadata");
                let content = registry.fetch(context, &address, bounds.metadata)?;
                require(
                    digest(&content) == metadata,
                    "dependency_python_metadata_integrity",
                )?;
                let license = metadata_license(&content, dependency, reviewed, bounds.metadata)?;
                licenses.insert(metadata, license.clone());
                license
            }
        };
        evidence.push(Evidence::Wheel {
            license,
            source: source.to_owned(),
            wheel: artifact.to_owned(),
            metadata: metadata.to_owned(),
        });
        wheels += 1;
    }
    require(
        found.len() == dependency.hashes.len(),
        "dependency_python_unresolved_hash",
    )?;
    require(wheels > 0, "dependency_python_wheel_missing")?;
    Ok(evidence)
}
