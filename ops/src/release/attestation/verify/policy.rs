//! Successful maintained-verifier output bound to the original validated claim.
//! This policy decodes no unverified DSSE/signature as execution authority.

use super::{
    super::{
        bindings,
        control::{self, Cause, Owner, Result, Window},
    },
    options::{Metadata, hex},
};
use crate::{json::Json, release::trust};
use std::collections::BTreeMap;

pub(super) struct Claim {
    pub original: Json,
    pub document: trust::Document<trust::Predicate>,
}
pub(super) fn require(owner: &mut Owner, value: bool) -> Result<()> {
    owner.check(Window::Body)?;
    if value {
        Ok(())
    } else {
        control::failed(owner, Cause::Policy)
    }
}
fn text<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key)?.string()
}
pub(super) fn claim(bytes: &[u8], metadata: &Metadata, owner: &mut Owner) -> Result<Claim> {
    owner.check(Window::Body)?;
    let original = bindings::decode(bytes, owner)?;
    let document = trust::Predicate::decode(bytes, &metadata.selection()?).map_err(|_| {
        owner.record(Cause::Policy);
        Cause::Policy
    })?;
    owner.check(Window::Body)?;
    Ok(Claim { original, document })
}
impl Claim {
    pub fn image(&self) -> &str {
        text(&self.original, "imageId").unwrap()
    }
    pub fn digests(&self) -> BTreeMap<String, String> {
        trust::BOUND_FILES
            .into_iter()
            .map(|name| {
                (
                    name.to_owned(),
                    self.document.value().file_digest(name).unwrap().to_owned(),
                )
            })
            .collect()
    }
}
pub(super) fn statement(
    bytes: &[u8],
    metadata: &Metadata,
    claim: &Claim,
    owner: &mut Owner,
) -> Result<()> {
    owner.check(Window::Body)?;
    let value = crate::json::decode_limited(
        bytes,
        super::process::OUT_LIMIT
            .try_into()
            .map_err(|_| Cause::Bounds)?,
    )
    .map_err(|_| {
        owner.record(Cause::Policy);
        Cause::Policy
    })?;
    let entries = value.array().ok_or(Cause::Policy)?;
    require(owner, entries.len() == 1)?;
    let result = entries[0].get("verificationResult").ok_or(Cause::Policy)?;
    require(owner, result.object().is_some())?;
    let certificate = result
        .get("signature")
        .and_then(|v| v.get("certificate"))
        .ok_or(Cause::Policy)?;
    require(owner, certificate.object().is_some())?;
    let repository = metadata.repository();
    let revision = metadata.revision();
    let expected = [
        (
            "subjectAlternativeName",
            format!(
                "https://github.com/{repository}/{}@{}",
                trust::WORKFLOW,
                trust::REF
            ),
        ),
        ("issuer", trust::ISSUER.to_owned()),
        ("buildSignerDigest", revision.to_owned()),
        (
            "sourceRepositoryURI",
            format!("https://github.com/{repository}"),
        ),
        ("sourceRepositoryDigest", revision.to_owned()),
        ("sourceRepositoryRef", trust::REF.to_owned()),
        (
            "runInvocationURI",
            format!(
                "https://github.com/{repository}/actions/runs/{}/attempts/{}",
                metadata.run(),
                metadata.attempt()
            ),
        ),
        ("runnerEnvironment", "github-hosted".into()),
        ("buildTrigger", "push".into()),
    ];
    require(
        owner,
        expected
            .iter()
            .all(|(key, value)| text(certificate, key) == Some(value)),
    )?;
    require(
        owner,
        result
            .get("verifiedTimestamps")
            .and_then(Json::array)
            .is_some_and(|values| !values.is_empty()),
    )?;
    let statement = result.get("statement").ok_or(Cause::Policy)?;
    require(
        owner,
        statement.object().is_some()
            && text(statement, "_type") == Some("https://in-toto.io/Statement/v1")
            && text(statement, "predicateType") == Some(trust::PREDICATE_TYPE)
            && statement
                .get("predicate")
                .is_some_and(|value| value.semantic_eq(&claim.original)),
    )?;
    let subjects = statement
        .get("subject")
        .and_then(Json::array)
        .ok_or(Cause::Policy)?;
    require(
        owner,
        subjects.len() == trust::SUBJECTS.len()
            && trust::SUBJECTS.iter().all(|name| {
                subjects.iter().any(|subject| {
                    subject.object().is_some_and(|object| object.len() == 2)
                        && text(subject, "name") == Some(*name)
                        && subject.get("digest").is_some_and(|digest| {
                            digest.object().is_some_and(|object| object.len() == 1)
                                && text(digest, "sha256")
                                    == claim.document.value().file_digest(name)
                        })
                })
            }),
    )
}
pub(super) fn outcome(value: &Json, metadata: &Metadata, owner: &mut Owner) -> Result<()> {
    require(
        owner,
        value.object().is_some()
            && value.get("schemaVersion").and_then(Json::integer) == Some(1)
            && value.get("passed").and_then(Json::boolean) == Some(true)
            && matches!(value.get("error"), Some(Json::Null))
            && text(value, "revision") == Some(metadata.revision()),
    )
}
pub(super) fn source(value: &Json, metadata: &Metadata, owner: &mut Owner) -> Result<()> {
    let inputs = value.get("inputsSha256").and_then(Json::object);
    let names = [
        "Dockerfile",
        ".dockerignore",
        "Cargo.lock",
        "web/package-lock.json",
        "build/pip-constraints.txt",
    ];
    require(
        owner,
        value.object().is_some_and(|object| object.len() == 2)
            && text(value, "revision") == Some(metadata.revision())
            && inputs.is_some_and(|inputs| {
                inputs.len() == names.len()
                    && names.iter().all(|name| {
                        inputs
                            .get(*name)
                            .and_then(Json::string)
                            .is_some_and(|value| hex(value, 64))
                    })
            }),
    )
}
pub(super) fn envelope(
    metadata: &Metadata,
    claim: &Claim,
    bundle: &str,
    verification: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<Vec<u8>> {
    owner.check(Window::Body)?;
    let proof = serde_json::json!({
        "predicate": claim.document.value(),
        "bundleSha256": bundle,
        "verificationSha256": verification
    });
    let bytes = serde_json::to_vec(&proof).map_err(|_| Cause::Policy)?;
    let _ = trust::Proof::decode(&bytes, &metadata.selection()?).map_err(|_| Cause::Policy)?;
    let mut value = serde_json::to_value(metadata).map_err(|_| Cause::Policy)?;
    value
        .as_object_mut()
        .unwrap()
        .insert("attestation".into(), proof);
    let mut bytes = serde_json::to_vec(&value).map_err(|_| Cause::Policy)?;
    bytes.push(b'\n');
    let _ = trust::Envelope::decode(&bytes).map_err(|_| Cause::Policy)?;
    require(owner, bytes.len() <= trust::MAX_LINE)?;
    Ok(bytes)
}
