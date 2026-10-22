//! Pure current release-claim and receiver-message policy. A matching claim is
//! never cryptographic proof: the authenticated controller must independently
//! verify signatures, selected run/API identity and current bytes before SSH.
//! No function here invokes a verifier or publishes a receipt. Read wrappers
//! inspect only explicitly supplied local paths through the bounded reader.

use crate::{
    Error, files,
    json::{self, Json},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

pub const PREDICATE_TYPE: &str =
    "https://github.com/swiftraccoon/simplestChat/attestations/release/v1";
pub const WORKFLOW: &str = ".github/workflows/ci.yml";
pub const REF: &str = "refs/heads/main";
pub const ISSUER: &str = "https://token.actions.githubusercontent.com";
pub const BOUND_FILES: [&str; 8] = [
    "image.tar",
    "release.json",
    "outcome.json",
    "source.json",
    "sbom.spdx.json",
    "image-security.json",
    "runtime-proof.json",
    "vex.openvex.json",
];
pub const SUBJECTS: [&str; 4] = [
    "image.tar",
    "sbom.spdx.json",
    "runtime-proof.json",
    "vex.openvex.json",
];
pub const MAX_METADATA: usize = 64 * 1024 * 1024;
pub const MAX_LINE: usize = 16 * 1024;

/// Fixed policy failures; no externally supplied key, URL or value is formatted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Object,
    Selection,
    PredicateShape,
    Schema,
    Source,
    Release,
    Image,
    Files,
    ProofShape,
    ProofDigests,
    Envelope,
    Request,
    HelperSet,
    HelperDigest,
    PreflightReceipt,
    Journal,
    Receipt,
    Identity,
    Evidence,
    Timing,
    ByteCount,
    Digest,
    Phase,
    FailureClass,
    Success,
    Failed,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Object => "expected_object",
            Self::Selection => "invalid_selection",
            Self::PredicateShape => "attestation_predicate_shape",
            Self::Schema => "attestation_schema",
            Self::Source => "attestation_source_identity",
            Self::Release => "attestation_release_policy",
            Self::Image => "attestation_image_identity",
            Self::Files => "attestation_file_digests",
            Self::ProofShape => "attestation_proof_shape",
            Self::ProofDigests => "attestation_proof_digests",
            Self::Envelope => "invalid_envelope",
            Self::Request => "invalid_request",
            Self::HelperSet => "invalid_helper_set",
            Self::HelperDigest => "invalid_helper_digest",
            Self::PreflightReceipt => "invalid_preflight_receipt",
            Self::Journal => "unfinished_operation",
            Self::Receipt => "receiver_receipt_invalid",
            Self::Identity => "receiver_identity_mismatch",
            Self::Evidence => "receiver_evidence_invalid",
            Self::Timing => "receiver_timing_invalid",
            Self::ByteCount => "receiver_byte_count_invalid",
            Self::Digest => "receiver_digest_invalid",
            Self::Phase => "receiver_phase_invalid",
            Self::FailureClass => "receiver_failure_invalid",
            Self::Success => "receiver_success_inconsistent",
            Self::Failed => "receiver_failure_inconsistent",
        }
    }
}
fn require(condition: bool, failure: Failure) -> Result<(), Error> {
    if condition {
        Ok(())
    } else {
        Err(Error::Policy(failure))
    }
}
fn object(value: &Json) -> Result<&BTreeMap<String, Json>, Error> {
    value.object().ok_or(Error::Policy(Failure::Object))
}
fn exact(value: &BTreeMap<String, Json>, names: &[&str]) -> bool {
    value.len() == names.len() && names.iter().all(|name| value.contains_key(*name))
}
fn string<'a>(value: &'a BTreeMap<String, Json>, key: &str) -> Option<&'a str> {
    value.get(key)?.string()
}
fn integer(value: &BTreeMap<String, Json>, key: &str) -> Option<i64> {
    value.get(key)?.integer()
}
fn boolean(value: &BTreeMap<String, Json>, key: &str) -> Option<bool> {
    value.get(key)?.boolean()
}
fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn digest(value: &Json) -> bool {
    value.string().is_some_and(|value| hex(value, 64))
}
fn hashes(value: &Json, names: &[&str]) -> Result<Option<BTreeMap<String, String>>, Error> {
    let values = object(value)?;
    if !exact(values, names) || !values.values().all(digest) {
        return Ok(None);
    }
    Ok(Some(
        values
            .iter()
            .map(|(key, v)| (key.clone(), v.string().unwrap().into()))
            .collect(),
    ))
}
fn repository(value: &str) -> bool {
    let Some((owner, repo)) = value.split_once('/') else {
        return false;
    };
    let name = |s: &str, maximum, extra: &[u8]| {
        !s.is_empty()
            && s.len() <= maximum
            && s.as_bytes()[0].is_ascii_alphanumeric()
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
    };
    name(owner, 39, b"-") && name(repo, 100, b"_.-")
}

/// The exact original encoded bytes and their SHA-256, separate from a typed
/// semantic projection. Retain this privately: unknown receipt fields may be in
/// the source even when the public validated projection deliberately omits them.
pub struct Document<T> {
    value: T,
    original: Vec<u8>,
    sha256: String,
}
impl<T> std::fmt::Debug for Document<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Document")
            .field("bytes", &self.original.len())
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}
impl<T> Document<T> {
    fn new(value: T, bytes: &[u8]) -> Self {
        Self {
            value,
            original: bytes.into(),
            sha256: Sha256::digest(bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect(),
        }
    }
    pub fn value(&self) -> &T {
        &self.value
    }
    pub fn as_json(&self) -> &[u8] {
        &self.original
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

/// Canonical receiver-domain identities, not a claim that a GitHub API/run was
/// observed. All IDs are positive signed64 integers; build/CI equality remains a
/// predicate check, so a mismatched selected run can never validate a claim.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    repository: String,
    revision: String,
    build_run_id: i64,
    ci_run_id: i64,
    run_attempt: i64,
}
impl Selection {
    pub fn new(
        repository_: &str,
        revision: &str,
        build_run_id: i64,
        ci_run_id: i64,
        run_attempt: i64,
    ) -> Result<Self, Error> {
        require(
            repository(repository_)
                && hex(revision, 40)
                && build_run_id > 0
                && ci_run_id > 0
                && run_attempt > 0,
            Failure::Selection,
        )?;
        Ok(Self {
            repository: repository_.into(),
            revision: revision.into(),
            build_run_id,
            ci_run_id,
            run_attempt,
        })
    }
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Predicate {
    schema_version: u8,
    repository: String,
    revision: String,
    run_id: i64,
    run_attempt: i64,
    workflow: String,
    #[serde(rename = "ref")]
    reference: String,
    event: String,
    platform: String,
    image_id: String,
    security_passed: bool,
    file_digests: BTreeMap<String, String>,
}
impl Predicate {
    pub fn decode(bytes: &[u8], selection: &Selection) -> Result<Document<Self>, Error> {
        Ok(Document::new(
            Self::parse(&json::decode_limited(bytes, MAX_METADATA)?, selection)?,
            bytes,
        ))
    }
    fn parse(value: &Json, selection: &Selection) -> Result<Self, Error> {
        let value = object(value)?;
        require(
            exact(
                value,
                &[
                    "schemaVersion",
                    "repository",
                    "revision",
                    "runId",
                    "runAttempt",
                    "workflow",
                    "ref",
                    "event",
                    "platform",
                    "imageId",
                    "securityPassed",
                    "fileDigests",
                ],
            ),
            Failure::PredicateShape,
        )?;
        require(integer(value, "schemaVersion") == Some(1), Failure::Schema)?;
        require(
            string(value, "repository") == Some(&selection.repository)
                && string(value, "revision") == Some(&selection.revision)
                && integer(value, "runId") == Some(selection.ci_run_id)
                && integer(value, "runAttempt") == Some(selection.run_attempt)
                && selection.build_run_id == selection.ci_run_id,
            Failure::Source,
        )?;
        require(
            string(value, "workflow") == Some(WORKFLOW)
                && string(value, "ref") == Some(REF)
                && string(value, "event") == Some("push")
                && string(value, "platform") == Some("linux/amd64")
                && boolean(value, "securityPassed") == Some(true),
            Failure::Release,
        )?;
        let image = string(value, "imageId").unwrap_or("");
        require(
            image.strip_prefix("sha256:").is_some_and(|v| hex(v, 64)),
            Failure::Image,
        )?;
        let files =
            hashes(&value["fileDigests"], &BOUND_FILES)?.ok_or(Error::Policy(Failure::Files))?;
        Ok(Self {
            schema_version: 1,
            repository: selection.repository.clone(),
            revision: selection.revision.clone(),
            run_id: selection.ci_run_id,
            run_attempt: selection.run_attempt,
            workflow: WORKFLOW.into(),
            reference: REF.into(),
            event: "push".into(),
            platform: "linux/amd64".into(),
            image_id: image.into(),
            security_passed: true,
            file_digests: files,
        })
    }
    pub fn file_digest(&self, name: &str) -> Option<&str> {
        self.file_digests.get(name).map(String::as_str)
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Proof {
    predicate: Predicate,
    bundle_sha256: String,
    verification_sha256: BTreeMap<String, String>,
}
impl Proof {
    pub fn decode(bytes: &[u8], selection: &Selection) -> Result<Document<Self>, Error> {
        Ok(Document::new(
            Self::parse(&json::decode_limited(bytes, MAX_METADATA)?, selection)?,
            bytes,
        ))
    }
    fn parse(value: &Json, selection: &Selection) -> Result<Self, Error> {
        let value = object(value)?;
        require(
            exact(value, &["predicate", "bundleSha256", "verificationSha256"]),
            Failure::ProofShape,
        )?;
        let predicate = Predicate::parse(&value["predicate"], selection)?;
        let hashes = object(&value["verificationSha256"])?;
        require(
            digest(&value["bundleSha256"])
                && exact(hashes, &SUBJECTS)
                && hashes.values().all(digest),
            Failure::ProofDigests,
        )?;
        Ok(Self {
            predicate,
            bundle_sha256: string(value, "bundleSha256").unwrap().into(),
            verification_sha256: hashes
                .iter()
                .map(|(k, v)| (k.clone(), v.string().unwrap().into()))
                .collect(),
        })
    }
    pub fn predicate(&self) -> &Predicate {
        &self.predicate
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    schema_version: u8,
    #[serde(flatten)]
    selection: Selection,
    artifact_id: i64,
    artifact_zip_bytes: i64,
    zip_sha256: String,
    attestation: Proof,
}
impl Envelope {
    /// Read one explicitly selected local envelope through the nonlink,
    /// descriptor-bound reader. This snapshot does not authenticate its claim.
    pub fn read(path: &Path) -> Result<Document<Self>, Error> {
        let bytes = files::read(path, MAX_LINE).map_err(|_| Error::Policy(Failure::Envelope))?;
        Self::decode(&bytes)
    }
    pub fn decode(bytes: &[u8]) -> Result<Document<Self>, Error> {
        let value = json::decode_limited(bytes, MAX_LINE)?;
        let value = value.object().ok_or(Error::Policy(Failure::Envelope))?;
        require(
            exact(
                value,
                &[
                    "schemaVersion",
                    "repository",
                    "artifactId",
                    "buildRunId",
                    "ciRunId",
                    "revision",
                    "artifactZipBytes",
                    "zipSha256",
                    "runAttempt",
                    "attestation",
                ],
            ) && integer(value, "schemaVersion") == Some(1),
            Failure::Envelope,
        )?;
        let selection = Selection::new(
            string(value, "repository").unwrap_or(""),
            string(value, "revision").unwrap_or(""),
            integer(value, "buildRunId").unwrap_or(0),
            integer(value, "ciRunId").unwrap_or(0),
            integer(value, "runAttempt").unwrap_or(0),
        )
        .map_err(|_| Error::Policy(Failure::Envelope))?;
        let artifact_id = integer(value, "artifactId").unwrap_or(0);
        let artifact_zip_bytes = integer(value, "artifactZipBytes").unwrap_or(0);
        let zip = string(value, "zipSha256").unwrap_or("");
        require(
            artifact_id > 0
                && (1..=2 * 1024 * 1024 * 1024).contains(&artifact_zip_bytes)
                && hex(zip, 64),
            Failure::Envelope,
        )?;
        let attestation = Proof::parse(&value["attestation"], &selection)
            .map_err(|_| Error::Policy(Failure::Envelope))?;
        Ok(Document::new(
            Self {
                schema_version: 1,
                selection,
                artifact_id,
                artifact_zip_bytes,
                zip_sha256: zip.into(),
                attestation,
            },
            bytes,
        ))
    }
    /// Validate one already-delimited physical stdin line, without accepting
    /// trailing messages. The operation owner must bound the actual stream read.
    pub fn decode_line(bytes: &[u8]) -> Result<Document<Self>, Error> {
        require(
            !bytes.is_empty()
                && bytes.len() <= MAX_LINE
                && bytes.ends_with(b"\n")
                && !bytes[..bytes.len() - 1].contains(&b'\n'),
            Failure::Request,
        )?;
        require(
            json::decode_limited(bytes, MAX_LINE)?.object().is_some(),
            Failure::Request,
        )?;
        Self::decode(bytes)
    }
    pub fn selection(&self) -> &Selection {
        &self.selection
    }
    pub fn proof(&self) -> &Proof {
        &self.attestation
    }
}

#[path = "trust_receipts.rs"]
mod receipts;
pub use receipts::{FinalizedJournal, HelperSet, PreflightReceipt, ReceiverReceipt};
#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
