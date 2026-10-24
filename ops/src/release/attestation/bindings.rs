//! Fresh current release-file and conditional-runtime relationships.
//! Decoded claims remain passive; these checks supply no signature authority.

use super::control::{self, Cause, Owner, Result, Window};
use crate::{json::Json, release::trust::Selection};
use std::collections::BTreeMap;

pub(super) const METADATA: usize = 64 * 1024 * 1024;
pub(super) const IMAGE: u64 = 2 * 1024 * 1024 * 1024;

pub(super) fn require(owner: &mut Owner, condition: bool) -> Result<()> {
    owner.check(Window::Body)?;
    if condition {
        Ok(())
    } else {
        control::failed(owner, Cause::Policy)
    }
}

pub(super) fn decode(bytes: &[u8], owner: &mut Owner) -> Result<Json> {
    owner.check(Window::Body)?;
    let value = crate::json::decode_limited(bytes, METADATA).map_err(|_| {
        owner.record(Cause::Policy);
        Cause::Policy
    })?;
    require(owner, value.object().is_some())?;
    Ok(value)
}

pub(super) fn text<'a>(value: &'a Json, key: &str) -> Option<&'a str> {
    value.get(key)?.string()
}

pub(super) fn boolean(value: &Json, key: &str) -> Option<bool> {
    value.get(key)?.boolean()
}

pub(super) fn bind_security(
    security: &Json,
    selected: &Selection,
    image: &str,
    digests: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<()> {
    require(
        owner,
        boolean(security, "passed") == Some(true)
            && text(security, "revision") == Some(selected.revision())
            && text(security, "platform") == Some("linux/amd64")
            && text(security, "imageId") == Some(image)
            && text(security, "archiveSha256") == digests.get("image.tar").map(String::as_str)
            && text(security, "sbomSha256") == digests.get("sbom.spdx.json").map(String::as_str),
    )
}

pub(super) fn runtime_security(
    security: &Json,
    digests: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<bool> {
    let required = boolean(security, "vexRequired");
    require(
        owner,
        required.is_some()
            && text(security, "runtimeProofSha256")
                == digests.get("runtime-proof.json").map(String::as_str)
            && text(security, "vexSha256") == digests.get("vex.openvex.json").map(String::as_str),
    )?;
    Ok(required.unwrap())
}

pub(super) fn runtime_proof(
    proof: &Json,
    required: bool,
    selected: &Selection,
    image: &str,
    digests: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<()> {
    require(
        owner,
        proof.get("schemaVersion").and_then(Json::integer) == Some(1)
            && boolean(proof, "required") == Some(required)
            && boolean(proof, "passed") == Some(true)
            && text(proof, "archiveSha256") == digests.get("image.tar").map(String::as_str)
            && text(proof, "imageId") == Some(image)
            && text(proof, "revision") == Some(selected.revision())
            && text(proof, "platform") == Some("linux/amd64"),
    )
}

pub(super) fn runtime_vex(vex: &Json, required: bool, owner: &mut Owner) -> Result<()> {
    require(
        owner,
        vex.get("statements")
            .and_then(Json::array)
            .is_some_and(|values| values.len() == usize::from(required)),
    )
}

pub(super) fn runtime_review(
    security: &Json,
    proof: &Json,
    required: bool,
    owner: &mut Owner,
) -> Result<()> {
    if !required {
        return Ok(());
    }
    let expires = proof.get("sourceReview").and_then(|v| text(v, "expires"));
    require(owner, expires.is_some())?;
    let expires = expires.unwrap();
    let bytes = expires.as_bytes();
    require(
        owner,
        bytes.len() == 10
            && bytes[4] == b'-'
            && bytes[7] == b'-'
            && bytes
                .iter()
                .enumerate()
                .all(|(at, b)| at == 4 || at == 7 || b.is_ascii_digit()),
    )?;
    // Match the reference's lexical guards, then current UTC, then calendar parse.
    let today = owner.utc_date()?;
    let year = expires[..4].parse::<i32>().ok();
    let month = expires[5..7].parse::<u32>().ok();
    let day = expires[8..].parse::<u32>().ok();
    let date = year
        .filter(|year| *year != 0)
        .zip(month)
        .zip(day)
        .and_then(|((year, month), day)| chrono::NaiveDate::from_ymd_opt(year, month, day));
    require(owner, date.is_some_and(|date| today < date))?;
    require(
        owner,
        text(proof, "scope") == Some("managed-default-server-dtls")
            && ["binarySha256", "nativeSha256", "elfSha256", "sbomSha256"]
                .iter()
                .all(|key| {
                    let digest = text(proof, key);
                    digest.is_some_and(|digest| {
                        digest.len() == 64
                            && digest
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    }) && digest == text(security, key)
                }),
    )
}

pub(super) fn bind_outcome(
    outcome: &Json,
    selected: &Selection,
    image: &str,
    owner: &mut Owner,
) -> Result<()> {
    require(
        owner,
        boolean(outcome, "passed") == Some(true)
            && text(outcome, "revision") == Some(selected.revision())
            && text(outcome, "exportedImageId") == Some(image),
    )
}

pub(super) fn bind_manifest(
    manifest: &Json,
    selected: &Selection,
    digests: &BTreeMap<String, String>,
    owner: &mut Owner,
) -> Result<()> {
    require(
        owner,
        text(manifest, "archiveSha256") == digests.get("image.tar").map(String::as_str)
            && text(manifest, "revision") == Some(selected.revision())
            && text(manifest, "platform") == Some("linux/amd64"),
    )
}
