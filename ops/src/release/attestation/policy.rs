//! Producer-only selector/claim checks over shared release-file bindings.

pub(super) use super::super::bindings::{
    IMAGE, METADATA, bind_manifest, bind_outcome, bind_security, decode, require, runtime_proof,
    runtime_review, runtime_security, runtime_vex,
};
use super::super::bindings::{boolean, text};
use super::{
    control::{Cause, Owner, Result, Window},
    options::Selection,
};
use crate::json::Json;
use std::collections::BTreeMap;

pub(super) const EVIDENCE: [(&str, &str); 8] = [
    ("sbomSha256", "spdx/sbom.spdx.json"),
    ("nativeSha256", "native.json"),
    ("elfSha256", "elf.json"),
    ("secretPathMapSha256", "secret-paths.json"),
    ("databaseEvidenceSha256", "database-status.json"),
    ("runtimeProofSha256", "runtime-proof.json"),
    ("vexSha256", "vex.openvex.json"),
    (
        "runtimeLicenseEvidenceSha256",
        "runtime-license-evidence.json",
    ),
];

pub(super) fn security(
    value: &Json,
    selected: &Selection,
    archive: &str,
    config: &str,
    owner: &mut Owner,
) -> Result<()> {
    require(
        owner,
        boolean(value, "passed") == Some(true)
            && boolean(value, "secretDetectorSelfTest") == Some(true)
            && text(value, "revision") == Some(&selected.revision)
            && text(value, "archiveSha256") == Some(archive),
    )?;
    require(
        owner,
        text(value, "imageId") == Some(config)
            && value
                .get("selectedImage")
                .and_then(|v| v.get("archiveConfigSha256"))
                .and_then(Json::string)
                == config.strip_prefix("sha256:"),
    )?;
    let checks = value.get("checks").and_then(Json::object);
    require(
        owner,
        checks.is_some_and(|checks| {
            checks.len() == 3
                && ["vulnerabilities", "licenses", "secrets"]
                    .iter()
                    .all(|name| checks.get(*name).and_then(|v| boolean(v, "passed")) == Some(true))
        }),
    )
}

pub(super) fn evidence(value: &Json, field: &str, actual: &str, owner: &mut Owner) -> Result<()> {
    require(owner, text(value, field) == Some(actual))
}

pub(super) fn claim(
    selected: &Selection,
    image: &str,
    digests: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<Vec<u8>> {
    let fields = BTreeMap::from([
        ("schemaVersion", serde_json::json!(1)),
        ("repository", serde_json::json!(selected.repository)),
        ("revision", serde_json::json!(selected.revision)),
        ("runId", serde_json::json!(selected.run)),
        ("runAttempt", serde_json::json!(selected.attempt)),
        (
            "workflow",
            serde_json::json!(crate::release::trust::WORKFLOW),
        ),
        ("ref", serde_json::json!(crate::release::trust::REF)),
        ("event", serde_json::json!("push")),
        ("platform", serde_json::json!("linux/amd64")),
        ("imageId", serde_json::json!(image)),
        ("securityPassed", serde_json::json!(true)),
        ("fileDigests", serde_json::json!(digests)),
    ]);
    let mut encoded = serde_json::to_vec_pretty(&fields).map_err(|_| {
        owner.record(Cause::Policy);
        Cause::Policy
    })?;
    encoded.push(b'\n');
    crate::release::trust::Predicate::decode(&encoded, &selected.claim_selection()?).map_err(
        |_| {
            owner.record(Cause::Policy);
            Cause::Policy
        },
    )?;
    owner.check(Window::Body)?;
    Ok(encoded)
}
