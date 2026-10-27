//! The trusted hosted owner binds its local stage observations to the same
//! workflow selection. Opaque producer evidence alone never authorizes signing.

use super::{catalog::Catalog, context::Context, failure};
use crate::tooling::Result;
use serde_json::Value;
use std::collections::BTreeMap;

pub(super) const NAMES: [&str; 9] = [
    "hosted-context.json",
    "hosted-kit.json",
    "hosted-registry.json",
    "hosted-source.json",
    "producer-materials.json",
    "producer-outcome.json",
    "producer-recipe.json",
    "producer-resolution.json",
    "producer-toolchain.json",
];
pub(super) fn encoded(value: &impl serde::Serialize) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| failure(1))?;
    bytes.push(b'\n');
    if bytes.len() > 65536 {
        return Err(failure(1));
    }
    Ok(bytes)
}
fn digest(value: &Value) -> Result<&str> {
    value
        .as_str()
        .filter(|s| super::super::digest(s))
        .ok_or_else(|| failure(1))
}
pub(super) fn check(
    context: &Context,
    manifest: &Value,
    documents: &BTreeMap<String, Vec<u8>>,
    catalog: &Catalog,
) -> Result<()> {
    if documents.keys().map(String::as_str).collect::<Vec<_>>() != NAMES {
        return Err(failure(1));
    }
    let mut decoded = BTreeMap::new();
    for (name, bytes) in documents {
        // Each source is already SHA-bound to the strict retained assembly
        // metadata. These selected JSON fields do not supply signature authority.
        if bytes.is_empty() || bytes.len() > 65536 {
            return Err(failure(1));
        }
        let value: Value = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        decoded.insert(name.as_str(), value);
    }
    let selected = serde_json::to_value(context).map_err(|_| failure(1))?;
    if decoded["hosted-context.json"] != selected {
        return Err(failure(1));
    }
    let source = &decoded["hosted-source.json"];
    if source["schemaVersion"] != 1
        || source["selection"] != selected
        || source["commit"] != context.revision
        || source["source"]["profile"] != "ops-producer-tree-v1"
    {
        return Err(failure(1));
    }
    let kit = &decoded["hosted-kit.json"];
    if kit["schemaVersion"] != 1
        || kit["kind"] != "ops-hosted-kit-observation"
        || kit["catalogSha256"] != catalog.identity.sha256
        || kit["catalogBytes"] != catalog.identity.bytes
        || kit["archives"] != catalog.identity.archives
        || kit["archiveBytes"] != catalog.identity.archive_bytes
        || kit["resolverProbes"] != super::kit::arguments().len()
        || kit["compilerRuntime"] != "hostedOsAndDynamicLoaderTrusted"
    {
        return Err(failure(1));
    }
    let registry = &decoded["hosted-registry.json"];
    if registry["schemaVersion"] != 1
        || registry["kind"] != "ops-offline-registry-observation"
        || digest(&registry["registry"]["sha256"])? != digest(&kit["registrySha256"])?
        || registry["lockSha256"] != kit["lockSha256"]
    {
        return Err(failure(1));
    }
    if digest(&manifest["inputDigests"]["source-tree/frame-v1"])?
        != digest(&source["source"]["sha256"])?
        || digest(&manifest["inputDigests"]["builder-kit/frame-v1"])?
            != digest(&kit["kit"]["sha256"])?
        || digest(&manifest["inputDigests"]["hosted/catalog-v1"])? != catalog.identity.sha256
        || digest(&manifest["inputDigests"]["source-lock/original-sha256"])?
            != digest(&registry["lockSha256"])?
    {
        return Err(failure(1));
    }
    let materials = &decoded["producer-materials.json"];
    if materials["source"]["sha256"] != source["source"]["sha256"]
        || materials["builderKit"]["sha256"] != kit["kit"]["sha256"]
    {
        return Err(failure(1));
    }
    let outcome = &decoded["producer-outcome.json"];
    if outcome["schemaVersion"] != 1
        || outcome["kind"] != "ops-cargo-observation"
        || outcome["commandStatus"] != 0
        || outcome["cargoAndProbeCliGroupsSettled"] != true
        || outcome["sourceConfigAndToolOwnersClosed"] != true
    {
        return Err(failure(1));
    }
    super::native_provenance::check(
        kit,
        &decoded["producer-recipe.json"],
        &decoded["producer-resolution.json"],
        &decoded["producer-toolchain.json"],
        catalog,
    )
}
