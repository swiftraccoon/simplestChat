//! Schema 1 for a separate Ops artifact. Build/source/version/digest declarations
//! are claims, not inspected provenance, cryptographic proof or install authority.

#[path = "distribution_pair.rs"]
mod pair;
#[cfg(test)]
#[path = "distribution_manifest_tests.rs"]
mod tests;

use crate::{
    files,
    json::{self, Json},
};
pub use pair::{VerifiedPair, verify_pair};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;
pub const MAX_SELECTION_BYTES: usize = 16 * 1024;
pub const TARGET: &str = "x86_64-unknown-linux-musl";
const SOURCE_KEYS: [&str; 7] = [
    "repository",
    "revision",
    "packageVersion",
    "protocolVersion",
    "target",
    "buildRunId",
    "runAttempt",
];
const KEYS: [&str; 12] = [
    "schemaVersion",
    "artifactType",
    "repository",
    "revision",
    "packageVersion",
    "protocolVersion",
    "target",
    "buildRunId",
    "runAttempt",
    "binary",
    "inputDigests",
    "evidenceDigests",
];

/// Fixed public failures; source paths, arbitrary keys, JSON and parser errors
/// remain private. No failure authorizes a fallback to a different artifact.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Input,
    Changed,
    Json,
    Fields,
    Schema,
    Source,
    Version,
    Target,
    Binary,
    Digests,
    Selection,
    Identity,
    Executable,
    Linkage,
}
impl Failure {
    pub fn code(self) -> &'static str {
        match self {
            Self::Input => "ops_artifact_input",
            Self::Changed => "ops_artifact_changed",
            Self::Json => "ops_artifact_json",
            Self::Fields => "ops_artifact_fields",
            Self::Schema => "ops_artifact_schema",
            Self::Source => "ops_artifact_source",
            Self::Version => "ops_artifact_version",
            Self::Target => "ops_artifact_target",
            Self::Binary => "ops_artifact_binary",
            Self::Digests => "ops_artifact_digests",
            Self::Selection => "ops_artifact_selection",
            Self::Identity => "ops_artifact_identity",
            Self::Executable => "ops_artifact_executable",
            Self::Linkage => "ops_artifact_linkage",
        }
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for Failure {}
impl From<crate::Error> for Failure {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Changed => Self::Changed,
            crate::Error::Json | crate::Error::AmbiguousJson => Self::Json,
            _ => Self::Input,
        }
    }
}

/// Canonical bounded source declarations, distinct from measured ELF facts.
/// The initial package-version profile is decimal MAJOR.MINOR.PATCH only;
/// protocol 1 is a compatibility declaration, never inferred from candidate code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceIdentity {
    repository: String,
    revision: String,
    package_version: String,
    protocol_version: u8,
    target: String,
    build_run_id: i64,
    run_attempt: i64,
}

/// Explicit caller selection, not a trusted API observation or signature proof.
#[derive(Clone, Debug)]
pub struct Selection(SourceIdentity);
impl Selection {
    pub fn new(
        repository: &str,
        revision: &str,
        package_version: &str,
        protocol_version: u8,
        target: &str,
        build_run_id: i64,
        run_attempt: i64,
    ) -> Result<Self, Failure> {
        validate_source(
            repository,
            revision,
            package_version,
            protocol_version,
            target,
            build_run_id,
            run_attempt,
        )
        .map_err(|_| Failure::Selection)?;
        let identity = SourceIdentity {
            repository: repository.into(),
            revision: revision.into(),
            package_version: package_version.into(),
            protocol_version,
            target: target.into(),
            build_run_id,
            run_attempt,
        };
        Ok(Self(identity))
    }
    /// Parse an exact seven-field caller-owned selection. No embedded manifest
    /// field can replace this explicit input. The input is not authenticated here.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let value = json::decode_limited(bytes, MAX_SELECTION_BYTES)?;
        let object = value.object().ok_or(Failure::Selection)?;
        exact(object, &SOURCE_KEYS).map_err(|_| Failure::Selection)?;
        Ok(Self(
            SourceIdentity::parse(object).map_err(|_| Failure::Selection)?,
        ))
    }
    pub fn read(path: &Path) -> Result<Self, Failure> {
        Self::decode(&files::read(path, MAX_SELECTION_BYTES)?)
    }
    pub fn identity(&self) -> &SourceIdentity {
        &self.0
    }
}
impl SourceIdentity {
    fn validate(&self) -> Result<(), Failure> {
        validate_source(
            &self.repository,
            &self.revision,
            &self.package_version,
            self.protocol_version,
            &self.target,
            self.build_run_id,
            self.run_attempt,
        )
    }
    fn parse(value: &BTreeMap<String, Json>) -> Result<Self, Failure> {
        let identity = Self {
            repository: string(value, "repository", Failure::Source)?.into(),
            revision: string(value, "revision", Failure::Source)?.into(),
            package_version: string(value, "packageVersion", Failure::Version)?.into(),
            protocol_version: value["protocolVersion"]
                .integer()
                .and_then(|v| u8::try_from(v).ok())
                .ok_or(Failure::Version)?,
            target: string(value, "target", Failure::Target)?.into(),
            build_run_id: value["buildRunId"].integer().ok_or(Failure::Source)?,
            run_attempt: value["runAttempt"].integer().ok_or(Failure::Source)?,
        };
        identity.validate()?;
        Ok(identity)
    }
}

#[derive(Clone)]
struct Binary {
    sha256: String,
    bytes: u64,
}

/// Retained original manifest bytes. Neither Debug nor the fixed public
/// projection serializes input/evidence map names or the original document.
pub struct ArtifactManifest {
    source: SourceIdentity,
    binary: Binary,
    input_digests: BTreeMap<String, String>,
    evidence_digests: BTreeMap<String, String>,
    original: Vec<u8>,
    sha256: String,
}
impl std::fmt::Debug for ArtifactManifest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactManifest")
            .field("bytes", &self.original.len())
            .field("sha256", &self.sha256)
            .finish_non_exhaustive()
    }
}
impl ArtifactManifest {
    /// Decode the new separate Ops contract, not the server release manifest.
    /// Encoded bytes are at most 64 KiB, decoded UTF-8 at most 128 KiB, with the
    /// shared 128-container/100,000-value limits and Unicode-scalar domain.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let value = json::decode_limited(bytes, MAX_MANIFEST_BYTES)?;
        let value = value.object().ok_or(Failure::Fields)?;
        exact(value, &KEYS)?;
        if !matches!(&value["schemaVersion"], Json::Integer(v) if v == "1")
            || value["artifactType"].string() != Some("simplestchat-ops")
        {
            return Err(Failure::Schema);
        }
        let source = SourceIdentity::parse(value)?;
        let binary = value["binary"].object().ok_or(Failure::Binary)?;
        exact(binary, &["name", "sha256", "bytes"]).map_err(|_| Failure::Binary)?;
        let sha256 = string(binary, "sha256", Failure::Binary)?;
        let bytes_count = binary["bytes"]
            .integer()
            .and_then(|v| u64::try_from(v).ok())
            .filter(|v| (1..=super::MAX_BYTES).contains(v))
            .ok_or(Failure::Binary)?;
        if binary["name"].string() != Some("simplestchat-ops") || !hex(sha256, 64) {
            return Err(Failure::Binary);
        }
        Ok(Self {
            source,
            binary: Binary {
                sha256: sha256.into(),
                bytes: bytes_count,
            },
            input_digests: digests(&value["inputDigests"], 1, 64)?,
            evidence_digests: digests(&value["evidenceDigests"], 0, 16)?,
            original: bytes.into(),
            sha256: digest(bytes),
        })
    }
    pub fn read(path: &Path) -> Result<Self, Failure> {
        Self::decode(&files::read(path, MAX_MANIFEST_BYTES)?)
    }
    pub fn as_json(&self) -> &[u8] {
        &self.original
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn source_identity(&self) -> &SourceIdentity {
        &self.source
    }
    /// Declared digest lookup for a future authenticated build-evidence owner.
    /// This function does not read, locate or hash a named input on disk.
    pub fn input_digest(&self, name: &str) -> Option<&str> {
        self.input_digests.get(name).map(String::as_str)
    }
    pub fn evidence_digest(&self, name: &str) -> Option<&str> {
        self.evidence_digests.get(name).map(String::as_str)
    }
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn exact(value: &BTreeMap<String, Json>, names: &[&str]) -> Result<(), Failure> {
    if value.len() == names.len() && names.iter().all(|name| value.contains_key(*name)) {
        Ok(())
    } else {
        Err(Failure::Fields)
    }
}
fn string<'a>(
    value: &'a BTreeMap<String, Json>,
    key: &str,
    failure: Failure,
) -> Result<&'a str, Failure> {
    value.get(key).and_then(Json::string).ok_or(failure)
}
fn hex(value: &str, size: usize) -> bool {
    value.len() == size
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn repository(value: &str) -> bool {
    let Some((owner, repo)) = value.split_once('/') else {
        return false;
    };
    let name = |value: &str, maximum, extra: &[u8]| {
        !value.is_empty()
            && value.len() <= maximum
            && value.as_bytes()[0].is_ascii_alphanumeric()
            && value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || extra.contains(&b))
    };
    name(owner, 39, b"-") && name(repo, 100, b"_.-")
}
fn package_version(value: &str) -> bool {
    if value.len() > 32 {
        return false;
    }
    let mut pieces = value.split('.');
    (0..3).all(|_| {
        pieces.next().is_some_and(|piece| {
            !piece.is_empty()
                && piece.len() <= 10
                && piece.bytes().all(|b| b.is_ascii_digit())
                && (piece.len() == 1 || !piece.starts_with('0'))
                && piece.parse::<u32>().is_ok()
        })
    }) && pieces.next().is_none()
}
fn validate_source(
    repository_: &str,
    revision: &str,
    version: &str,
    protocol: u8,
    target: &str,
    run: i64,
    attempt: i64,
) -> Result<(), Failure> {
    if !repository(repository_) || !hex(revision, 40) || run <= 0 || attempt <= 0 {
        return Err(Failure::Source);
    }
    if !package_version(version) || protocol != 1 {
        return Err(Failure::Version);
    }
    if target != TARGET {
        return Err(Failure::Target);
    }
    Ok(())
}
fn digest_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
}
fn digests(
    value: &Json,
    minimum: usize,
    maximum: usize,
) -> Result<BTreeMap<String, String>, Failure> {
    let value = value.object().ok_or(Failure::Digests)?;
    if !(minimum..=maximum).contains(&value.len()) {
        return Err(Failure::Digests);
    }
    value
        .iter()
        .map(|(name, digest)| {
            let digest = digest
                .string()
                .filter(|value| hex(value, 64))
                .ok_or(Failure::Digests)?;
            if !digest_name(name) {
                return Err(Failure::Digests);
            }
            Ok((name.clone(), digest.into()))
        })
        .collect()
}
