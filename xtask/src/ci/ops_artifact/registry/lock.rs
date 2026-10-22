//! Cargo lock v4's registry selections, parsed by bounded mature TOML.
//! No resolver, target/feature graph or authenticated registry claim is made.

use super::*;
use serde::Deserialize;
use std::collections::BTreeSet;

pub(super) const MAXIMUM: u64 = 4 * 1024 * 1024;
const REGISTRY: &str = "registry+https://github.com/rust-lang/crates.io-index";
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wire {
    version: u8,
    package: Vec<Package>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Package {
    name: String,
    version: String,
    source: Option<String>,
    checksum: Option<String>,
    #[serde(default, rename = "dependencies")]
    _dependencies: Vec<String>,
}
pub(super) struct Crate {
    pub name: String,
    pub version: String,
    pub directory: String,
    pub checksum: String,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && value.bytes().next().unwrap().is_ascii_alphanumeric()
}
fn version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'+' | b'_'))
        && value.bytes().next().unwrap().is_ascii_digit()
        && !value.contains("..")
}
pub(super) fn parse(bytes: &[u8]) -> Result<Vec<Crate>> {
    if bytes.is_empty() || bytes.len() as u64 > MAXIMUM {
        return Err(failure(1));
    }
    // The dependency's default 80-level RecursionGuard remains enabled. Its
    // `unbounded` feature is deliberately absent; no TOML parser is recreated.
    let wire: Wire = toml::from_slice(bytes).map_err(|_| failure(1))?;
    if wire.version != 4 || wire.package.len() > 4096 {
        return Err(failure(1));
    }
    let mut unique = BTreeSet::new();
    let mut output = Vec::new();
    for package in wire.package {
        if !name(&package.name) || !version(&package.version) {
            return Err(failure(1));
        }
        if package.source.is_none() && package.checksum.is_none() {
            continue;
        }
        let checksum = package.checksum.ok_or_else(|| failure(1))?;
        let directory = format!("{}-{}", package.name, package.version);
        if package.source.as_deref() != Some(REGISTRY)
            || !digest(&checksum)
            || !unique.insert(directory.clone())
            || output.len() >= 1024
        {
            return Err(failure(1));
        }
        output.push(Crate {
            name: package.name,
            version: package.version,
            directory,
            checksum,
        });
    }
    if output.is_empty() {
        return Err(failure(1));
    }
    output.sort_by(|a, b| a.directory.cmp(&b.directory));
    Ok(output)
}

/// Crates.io archives contain a normalized manifest. Read only these identity
/// fields; dependency/build-script contents remain uninterpreted material.
pub(super) fn manifest(bytes: &[u8], selected: &Crate) -> Result<()> {
    #[derive(Deserialize)]
    struct Document {
        package: Identity,
    }
    #[derive(Deserialize)]
    struct Identity {
        name: String,
        version: String,
    }
    if bytes.len() > 1024 * 1024 {
        return Err(failure(1));
    }
    let value: Document = toml::from_slice(bytes).map_err(|_| failure(1))?;
    if value.package.name != selected.name || value.package.version != selected.version {
        return Err(failure(1));
    }
    Ok(())
}
