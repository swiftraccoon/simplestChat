//! Pure policy for a separate retained Ops artifact. These functions interpret
//! caller-supplied API and successful-verifier output; they never execute a
//! verifier, perform cryptography, fetch trust roots or grant install authority.

#[cfg(test)]
#[path = "distribution_verification_tests.rs"]
mod tests;

use super::{ArtifactManifest, Selection, VerifiedPair, assembly};
use crate::json::{self, Json};
use serde::Serialize;
use std::collections::BTreeMap;

pub const REPOSITORY: &str = "swiftraccoon/simplestChat";
pub const REPOSITORY_ID: u64 = 1_155_881_649;
pub const OWNER_ID: u64 = 47_705_353;
pub const OWNER: &str = "swiftraccoon";
pub const REPOSITORY_URI: &str = "https://github.com/swiftraccoon/simplestChat";
pub const OWNER_URI: &str = "https://github.com/swiftraccoon";
pub const WORKFLOW: &str = ".github/workflows/ci.yml";
const WORKFLOW_PATHS: [&str; 3] = [
    WORKFLOW,
    ".github/workflows/ci.yml@main",
    ".github/workflows/ci.yml@refs/heads/main",
];
pub const SOURCE_REF: &str = "refs/heads/main";
pub const CERTIFICATE_IDENTITY: &str = concat!(
    "https://github.com/swiftraccoon/simplestChat/",
    ".github/workflows/ci.yml@refs/heads/main"
);
pub const OIDC_ISSUER: &str = "https://token.actions.githubusercontent.com";
pub const MAX_SAFE_INTEGER: u64 = (1_u64 << 53) - 1;
pub const MAX_API_BYTES: usize = 64 * 1024;
pub const MAX_VERIFIER_BYTES: usize = 8 * 1024 * 1024;
const MAX_METADATA_BYTES: usize = 64 * 1024;
const RESULT_MEDIA: &str = "application/vnd.dev.sigstore.verificationresult+json;version=0.1";
const STATEMENT_TYPE: &str = "https://in-toto.io/Statement/v1";
const PREREQUISITES: [&str; 5] = [
    "controlledMuslBuildAndCompleteInputClosure",
    "authenticatedSourceToolchainHostAndRegistryInputs",
    "trustedWorkflowAttestationAndFreshControllerVerification",
    "ownedDebianRuntimeEvidence",
    "transportBootstrapAndInstallationOwnership",
];

/// Fixed private-input refusals. Neither formatting trait includes supplied
/// names, paths, JSON, certificate fields or external verifier diagnostics.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Json,
    Selection,
    Metadata,
    Evidence,
    Identity,
    Api,
    Verifier,
    Certificate,
    Timestamp,
    Subjects,
    Predicate,
    Pair,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Json => "ops_verification_json",
            Self::Selection => "ops_verification_selection",
            Self::Metadata => "ops_verification_metadata",
            Self::Evidence => "ops_verification_evidence",
            Self::Identity => "ops_verification_identity",
            Self::Api => "ops_verification_api",
            Self::Verifier => "ops_verification_output",
            Self::Certificate => "ops_verification_certificate",
            Self::Timestamp => "ops_verification_timestamp",
            Self::Subjects => "ops_verification_subjects",
            Self::Predicate => "ops_verification_predicate",
            Self::Pair => "ops_verification_pair",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Failure {}

/// One fixed generated basename and its expected original bytes. Logical
/// evidence names never enter this projection or its Debug representation.
#[derive(Clone, Debug)]
pub struct PayloadRequirement {
    name: String,
    sha256: String,
    bytes: u64,
}
impl PayloadRequirement {
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
}

/// A conditional match of caller-supplied output. This is deliberately not a
/// cryptographic-verification receipt: the caller owns successful gh execution,
/// selected executable/trust-root authority and process/file finalization.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchedVerifierPolicy {
    schema_version: u8,
    kind: &'static str,
    repository_id: u64,
    repository_owner_id: u64,
    revision: String,
    build_run_id: i64,
    run_attempt: i64,
    manifest_sha256: String,
    preparation_sha256: String,
    binary_sha256: String,
    subject_count: usize,
    evidence_count: usize,
    observer_timestamp_count: usize,
}

/// All small original records are checked before the caller opens the binary
/// or creates its output. Exact original manifest bytes remain available to the
/// unchanged pair inspector; declarations alone authenticate no payload.
pub struct RetainedMetadata {
    manifest: ArtifactManifest,
    predicate: Json,
    identity: Json,
    payloads: Vec<PayloadRequirement>,
    preparation_sha256: String,
    preparation_bytes: u64,
}
impl std::fmt::Debug for RetainedMetadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RetainedOpsMetadata")
            .field("manifestSha256", &self.manifest.sha256())
            .field("payloadCount", &self.payloads.len())
            .finish_non_exhaustive()
    }
}
impl RetainedMetadata {
    pub fn decode(
        selection: &Selection,
        original_manifest: &[u8],
        index: &[u8],
        predicate: &[u8],
        preparation: &[u8],
    ) -> Result<Self, Failure> {
        selection_policy(selection)?;
        // Admit all small-record bounds before hashing supplied index bytes.
        // The caller separately owns bounded filesystem reads.
        if [original_manifest, index, predicate, preparation]
            .iter()
            .any(|bytes| bytes.is_empty() || bytes.len() > MAX_METADATA_BYTES)
        {
            return Err(Failure::Json);
        }
        let manifest =
            ArtifactManifest::decode(original_manifest).map_err(|_| Failure::Metadata)?;
        if manifest.source != selection.0 {
            return Err(Failure::Selection);
        }
        let mut payloads = vec![
            requirement("ops-release.json", original_manifest),
            PayloadRequirement {
                name: "simplestchat-ops".into(),
                sha256: manifest.binary.sha256.clone(),
                bytes: manifest.binary.bytes,
            },
            requirement("ops-evidence.json", index),
        ];
        let index = decode(index, MAX_METADATA_BYTES)?;
        let index = object(&index, Failure::Evidence)?;
        exact(
            index,
            &["schemaVersion", "kind", "entries"],
            Failure::Evidence,
        )?;
        schema(index, "ops-evidence-index", Failure::Evidence)?;
        let entries = index["entries"].array().ok_or(Failure::Evidence)?;
        if entries.len() != manifest.evidence_digests.len() {
            return Err(Failure::Evidence);
        }
        for (n, (entry, (logical, digest))) in
            entries.iter().zip(&manifest.evidence_digests).enumerate()
        {
            let entry = object(entry, Failure::Evidence)?;
            exact(
                entry,
                &["logicalName", "payload", "sha256", "bytes"],
                Failure::Evidence,
            )?;
            let name = format!("ops-evidence-{n:02}.bin");
            let bytes = unsigned(&entry["bytes"], assembly::MAX_EVIDENCE_BYTES)
                .filter(|v| *v > 0)
                .ok_or(Failure::Evidence)?;
            if text(entry, "logicalName") != Some(logical)
                || text(entry, "payload") != Some(name.as_str())
                || text(entry, "sha256") != Some(digest)
            {
                return Err(Failure::Evidence);
            }
            payloads.push(PayloadRequirement {
                name,
                sha256: digest.clone(),
                bytes,
            });
        }
        let predicate_value = decode(predicate, MAX_METADATA_BYTES)?;
        let value = object(&predicate_value, Failure::Predicate)?;
        exact(
            value,
            &[
                "schemaVersion",
                "kind",
                "predicateType",
                "identity",
                "fileDigests",
                "evidenceCount",
            ],
            Failure::Predicate,
        )?;
        schema(value, "ops-artifact-identity", Failure::Predicate)?;
        if text(value, "predicateType") != Some(assembly::PREDICATE_TYPE)
            || unsigned(&value["evidenceCount"], 16) != Some(entries.len() as u64)
        {
            return Err(Failure::Predicate);
        }
        check_digests(&value["fileDigests"], &payloads)?;
        let identity = value["identity"].clone();
        check_identity(&manifest, &identity)?;
        payloads.push(requirement("ops-predicate.json", predicate));
        let preparation_value = decode(preparation, MAX_METADATA_BYTES)?;
        let prepared = object(&preparation_value, Failure::Metadata)?;
        exact(
            prepared,
            &[
                "schemaVersion",
                "kind",
                "claims",
                "fileDigests",
                "evidenceCount",
                "prerequisites",
            ],
            Failure::Metadata,
        )?;
        schema(prepared, "ops-unsigned-artifact", Failure::Metadata)?;
        if !same_json(&prepared["claims"], &encoded(&selection.0)?)
            || unsigned(&prepared["evidenceCount"], 16) != Some(entries.len() as u64)
        {
            return Err(Failure::Selection);
        }
        let prerequisites = prepared["prerequisites"].array().ok_or(Failure::Metadata)?;
        if prerequisites.len() != PREREQUISITES.len()
            || !prerequisites
                .iter()
                .zip(PREREQUISITES)
                .all(|(a, b)| a.string() == Some(b))
        {
            return Err(Failure::Metadata);
        }
        check_digests(&prepared["fileDigests"], &payloads)?;
        let preparation_sha256 = super::digest(preparation);
        let preparation_bytes = preparation.len() as u64;
        payloads.push(requirement("ops-preparation.json", preparation));
        Ok(Self {
            manifest,
            predicate: predicate_value,
            identity,
            payloads,
            preparation_sha256,
            preparation_bytes,
        })
    }
    pub fn manifest(&self) -> &ArtifactManifest {
        &self.manifest
    }
    pub fn payloads(&self) -> &[PayloadRequirement] {
        &self.payloads
    }
    pub fn preparation_sha256(&self) -> &str {
        &self.preparation_sha256
    }
    pub fn preparation_bytes(&self) -> u64 {
        self.preparation_bytes
    }
    /// Compare actual unchanged pair-inspector output with the retained claim.
    /// This function does not run the inspector or execute the candidate.
    pub fn check_pair(&self, pair: &VerifiedPair) -> Result<(), Failure> {
        if same_json(&self.identity, &encoded(pair)?) {
            Ok(())
        } else {
            Err(Failure::Pair)
        }
    }
    /// Interpret only output the caller separately obtained from a successful
    /// pinned verifier process. Supplying fabricated JSON can satisfy this pure
    /// function; its result is never evidence that cryptography was executed.
    pub fn interpret_successful_verifier_output(
        &self,
        repository_json: &[u8],
        exact_attempt_json: &[u8],
        verification_json: &[u8],
    ) -> Result<MatchedVerifierPolicy, Failure> {
        let repository = decode(repository_json, MAX_API_BYTES)?;
        repository_policy(&repository)?;
        let attempt = decode(exact_attempt_json, MAX_API_BYTES)?;
        self.attempt_policy(&attempt)?;
        let output = decode(verification_json, MAX_VERIFIER_BYTES)?;
        let rows = output.array().ok_or(Failure::Verifier)?;
        if rows.len() != 1 {
            return Err(Failure::Verifier);
        }
        let row = object(&rows[0], Failure::Verifier)?;
        exact(
            row,
            &["attestation", "verificationResult"],
            Failure::Verifier,
        )?;
        // The top-level attestation is not an authentication source.
        let result = row.get("verificationResult").ok_or(Failure::Verifier)?;
        let result = object(result, Failure::Verifier)?;
        if text(result, "mediaType") != Some(RESULT_MEDIA) {
            return Err(Failure::Verifier);
        }
        let signature = object(
            result.get("signature").ok_or(Failure::Certificate)?,
            Failure::Certificate,
        )?;
        let certificate = object(
            signature.get("certificate").ok_or(Failure::Certificate)?,
            Failure::Certificate,
        )?;
        self.certificate_policy(certificate)?;
        let timestamps = result
            .get("verifiedTimestamps")
            .and_then(Json::array)
            .ok_or(Failure::Timestamp)?;
        if timestamps.is_empty() || timestamps.len() > 32 {
            return Err(Failure::Timestamp);
        }
        for timestamp in timestamps {
            timestamp_policy(timestamp)?;
        }
        let statement = result.get("statement").ok_or(Failure::Verifier)?;
        let statement = object(statement, Failure::Verifier)?;
        exact(
            statement,
            &["_type", "subject", "predicateType", "predicate"],
            Failure::Verifier,
        )?;
        if text(statement, "_type") != Some(STATEMENT_TYPE)
            || text(statement, "predicateType") != Some(assembly::PREDICATE_TYPE)
        {
            return Err(Failure::Verifier);
        }
        self.subjects_policy(&statement["subject"])?;
        if !same_json(&statement["predicate"], &self.predicate) {
            return Err(Failure::Predicate);
        }
        Ok(MatchedVerifierPolicy {
            schema_version: 1,
            kind: "ops-caller-supplied-verifier-output-policy-match",
            repository_id: REPOSITORY_ID,
            repository_owner_id: OWNER_ID,
            revision: self.manifest.source.revision.clone(),
            build_run_id: self.manifest.source.build_run_id,
            run_attempt: self.manifest.source.run_attempt,
            manifest_sha256: self.manifest.sha256.clone(),
            preparation_sha256: self.preparation_sha256.clone(),
            binary_sha256: self.manifest.binary.sha256.clone(),
            subject_count: 3,
            evidence_count: self.manifest.evidence_digests.len(),
            observer_timestamp_count: timestamps.len(),
        })
    }
    fn attempt_policy(&self, value: &Json) -> Result<(), Failure> {
        let object = object(value, Failure::Api)?;
        let source = &self.manifest.source;
        if object.get("id").and_then(|v| unsigned(v, MAX_SAFE_INTEGER))
            != Some(source.build_run_id as u64)
            || object
                .get("run_attempt")
                .and_then(|v| unsigned(v, MAX_SAFE_INTEGER))
                != Some(source.run_attempt as u64)
            || text(object, "head_sha") != Some(source.revision.as_str())
            || text(object, "head_branch") != Some("main")
            || text(object, "name") != Some("CI")
            || !text(object, "path").is_some_and(|path| WORKFLOW_PATHS.contains(&path))
            || text(object, "event") != Some("push")
            || text(object, "status") != Some("completed")
            || text(object, "conclusion") != Some("success")
        {
            return Err(Failure::Api);
        }
        for key in ["repository", "head_repository"] {
            repository_policy(object.get(key).ok_or(Failure::Api)?)?;
        }
        Ok(())
    }
    fn certificate_policy(&self, certificate: &BTreeMap<String, Json>) -> Result<(), Failure> {
        let source = &self.manifest.source;
        let run_uri = format!(
            "{REPOSITORY_URI}/actions/runs/{}/attempts/{}",
            source.build_run_id, source.run_attempt
        );
        for (key, expected) in [
            ("sourceRepositoryIdentifier", REPOSITORY_ID.to_string()),
            ("sourceRepositoryOwnerIdentifier", OWNER_ID.to_string()),
            ("subjectAlternativeName", CERTIFICATE_IDENTITY.into()),
            ("issuer", OIDC_ISSUER.into()),
            ("sourceRepositoryURI", REPOSITORY_URI.into()),
            ("sourceRepositoryOwnerURI", OWNER_URI.into()),
            ("sourceRepositoryDigest", source.revision.clone()),
            ("sourceRepositoryRef", SOURCE_REF.into()),
            ("buildSignerDigest", source.revision.clone()),
            ("runInvocationURI", run_uri),
            ("buildTrigger", "push".into()),
            ("runnerEnvironment", "github-hosted".into()),
        ] {
            if text(certificate, key) != Some(expected.as_str()) {
                return Err(Failure::Certificate);
            }
        }
        Ok(())
    }
    fn subjects_policy(&self, value: &Json) -> Result<(), Failure> {
        let subjects = value.array().ok_or(Failure::Subjects)?;
        if subjects.len() != 3 {
            return Err(Failure::Subjects);
        }
        let expected = BTreeMap::from([
            ("ops-release.json", self.manifest.sha256.as_str()),
            ("ops-preparation.json", self.preparation_sha256.as_str()),
            ("simplestchat-ops", self.manifest.binary.sha256.as_str()),
        ]);
        let mut seen = std::collections::BTreeSet::new();
        for subject in subjects {
            let subject = object(subject, Failure::Subjects)?;
            exact(subject, &["name", "digest"], Failure::Subjects)?;
            let name = text(subject, "name").ok_or(Failure::Subjects)?;
            if !seen.insert(name) {
                return Err(Failure::Subjects);
            }
            let digest = object(&subject["digest"], Failure::Subjects)?;
            exact(digest, &["sha256"], Failure::Subjects)?;
            if expected.get(name).copied() != text(digest, "sha256") {
                return Err(Failure::Subjects);
            }
        }
        Ok(())
    }
}

fn decode(bytes: &[u8], limit: usize) -> Result<Json, Failure> {
    json::decode_limited(bytes, limit).map_err(|_| Failure::Json)
}
fn object(value: &Json, failure: Failure) -> Result<&BTreeMap<String, Json>, Failure> {
    value.object().ok_or(failure)
}
fn exact(value: &BTreeMap<String, Json>, keys: &[&str], failure: Failure) -> Result<(), Failure> {
    if value.len() == keys.len() && keys.iter().all(|k| value.contains_key(*k)) {
        Ok(())
    } else {
        Err(failure)
    }
}
fn text<'a>(value: &'a BTreeMap<String, Json>, key: &str) -> Option<&'a str> {
    value.get(key).and_then(Json::string)
}
fn unsigned(value: &Json, maximum: u64) -> Option<u64> {
    value
        .integer()
        .and_then(|v| u64::try_from(v).ok())
        .filter(|v| *v <= maximum)
}
fn schema(value: &BTreeMap<String, Json>, kind: &str, failure: Failure) -> Result<(), Failure> {
    if value.get("schemaVersion").and_then(|v| unsigned(v, 1)) == Some(1)
        && text(value, "kind") == Some(kind)
    {
        Ok(())
    } else {
        Err(failure)
    }
}
fn encoded(value: &impl Serialize) -> Result<Json, Failure> {
    let bytes = serde_json::to_vec(value).map_err(|_| Failure::Metadata)?;
    decode(&bytes, MAX_METADATA_BYTES)
}
fn requirement(name: &str, bytes: &[u8]) -> PayloadRequirement {
    PayloadRequirement {
        name: name.into(),
        sha256: super::digest(bytes),
        bytes: bytes.len() as u64,
    }
}
fn selection_policy(selection: &Selection) -> Result<(), Failure> {
    let source = &selection.0;
    if source.repository != REPOSITORY
        || source.build_run_id <= 0
        || source.run_attempt <= 0
        || source.build_run_id as u64 > MAX_SAFE_INTEGER
        || source.run_attempt as u64 > MAX_SAFE_INTEGER
    {
        return Err(Failure::Selection);
    }
    Ok(())
}
fn check_digests(value: &Json, expected: &[PayloadRequirement]) -> Result<(), Failure> {
    let values = object(value, Failure::Metadata)?;
    if values.len() != expected.len() {
        return Err(Failure::Metadata);
    }
    for payload in expected {
        let digest = object(
            values.get(payload.name()).ok_or(Failure::Metadata)?,
            Failure::Metadata,
        )?;
        exact(digest, &["sha256", "bytes"], Failure::Metadata)?;
        if text(digest, "sha256") != Some(payload.sha256())
            || unsigned(&digest["bytes"], crate::distribution::MAX_BYTES) != Some(payload.bytes())
        {
            return Err(Failure::Metadata);
        }
    }
    Ok(())
}
fn check_identity(manifest: &ArtifactManifest, identity: &Json) -> Result<(), Failure> {
    let value = object(identity, Failure::Identity)?;
    exact(
        value,
        &["manifestSha256", "manifestBytes", "claims", "executable"],
        Failure::Identity,
    )?;
    let mut claims = serde_json::to_value(&manifest.source).map_err(|_| Failure::Identity)?;
    let fields = claims.as_object_mut().ok_or(Failure::Identity)?;
    fields.insert("schemaVersion".into(), 1.into());
    fields.insert("artifactType".into(), "simplestchat-ops".into());
    fields.insert(
        "inputDigestCount".into(),
        manifest.input_digests.len().into(),
    );
    fields.insert(
        "evidenceDigestCount".into(),
        manifest.evidence_digests.len().into(),
    );
    if text(value, "manifestSha256") != Some(manifest.sha256.as_str())
        || unsigned(&value["manifestBytes"], MAX_METADATA_BYTES as u64)
            != Some(manifest.original.len() as u64)
        || !same_json(&value["claims"], &encoded(&claims)?)
    {
        return Err(Failure::Identity);
    }
    let executable = object(&value["executable"], Failure::Identity)?;
    exact(
        executable,
        &[
            "sha256",
            "bytes",
            "architecture",
            "elfClass",
            "littleEndian",
            "fileType",
            "programHeaders",
            "noInterpreter",
            "neededCount",
            "noNeeded",
            "pieFlag",
            "bindNow",
            "nonExecutableStack",
            "noWritableExecutableLoad",
            "relroPresent",
            "noRpath",
            "noRunpath",
            "noTextRelocations",
            "noLoaderHooks",
        ],
        Failure::Identity,
    )?;
    if text(executable, "sha256") != Some(manifest.binary.sha256.as_str())
        || unsigned(&executable["bytes"], crate::distribution::MAX_BYTES)
            != Some(manifest.binary.bytes)
        || text(executable, "architecture") != Some("x86_64")
        || unsigned(&executable["elfClass"], 64) != Some(64)
        || !matches!(text(executable, "fileType"), Some("executable" | "dynamic"))
        || unsigned(&executable["programHeaders"], u16::MAX.into()).is_none_or(|v| v == 0)
        || unsigned(&executable["neededCount"], 0) != Some(0)
    {
        return Err(Failure::Identity);
    }
    for key in ["littleEndian", "noInterpreter", "noNeeded", "noLoaderHooks"] {
        if executable[key].boolean() != Some(true) {
            return Err(Failure::Identity);
        }
    }
    for key in [
        "pieFlag",
        "bindNow",
        "nonExecutableStack",
        "noWritableExecutableLoad",
        "relroPresent",
        "noRpath",
        "noRunpath",
        "noTextRelocations",
    ] {
        if executable[key].boolean().is_none() {
            return Err(Failure::Identity);
        }
    }
    Ok(())
}
fn repository_policy(value: &Json) -> Result<(), Failure> {
    let value = object(value, Failure::Api)?;
    let owner = object(value.get("owner").ok_or(Failure::Api)?, Failure::Api)?;
    if value.get("id").and_then(|v| unsigned(v, MAX_SAFE_INTEGER)) != Some(REPOSITORY_ID)
        || text(value, "full_name") != Some(REPOSITORY)
        || value.get("fork").and_then(Json::boolean) != Some(false)
        || owner.get("id").and_then(|v| unsigned(v, MAX_SAFE_INTEGER)) != Some(OWNER_ID)
        || text(owner, "login") != Some(OWNER)
    {
        return Err(Failure::Api);
    }
    Ok(())
}
fn timestamp_policy(value: &Json) -> Result<(), Failure> {
    let value = object(value, Failure::Timestamp)?;
    exact(value, &["type", "uri", "timestamp"], Failure::Timestamp)?;
    if !matches!(text(value, "type"), Some("Tlog" | "TimestampAuthority")) {
        return Err(Failure::Timestamp);
    }
    let uri = text(value, "uri").ok_or(Failure::Timestamp)?;
    if !uri.starts_with("https://")
        || uri.len() <= 8
        || uri.len() > 2048
        || !uri.bytes().all(|b| b.is_ascii_graphic())
    {
        return Err(Failure::Timestamp);
    }
    let timestamp = text(value, "timestamp").ok_or(Failure::Timestamp)?;
    if timestamp.len() > 64 || chrono::DateTime::parse_from_rfc3339(timestamp).is_err() {
        return Err(Failure::Timestamp);
    }
    Ok(())
}

// Protojson may normalize integral numeric spellings. Booleans remain a
// distinct type; Json::semantic_eq intentionally has Python bool==int semantics
// and therefore is inappropriate at this new authentication policy boundary.
fn same_json(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Integer(_) | Json::Float(_), Json::Integer(_) | Json::Float(_)) => a.semantic_eq(b),
        (Json::Bool(a), Json::Bool(b)) => a == b,
        (Json::Null, Json::Null) => true,
        (Json::String(a), Json::String(b)) => a == b,
        (Json::Array(a), Json::Array(b)) => {
            a.len() == b.len() && a.iter().zip(b).all(|(a, b)| same_json(a, b))
        }
        (Json::Object(a, _), Json::Object(b, _)) => {
            a.len() == b.len()
                && a.iter()
                    .all(|(k, a)| b.get(k).is_some_and(|b| same_json(a, b)))
        }
        _ => false,
    }
}
