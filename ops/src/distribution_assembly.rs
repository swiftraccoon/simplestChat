//! Prepare local unsigned signing inputs, without building or running a candidate.
//! Selection and digest declarations remain caller-owned. Measured bytes and ELF
//! facts do not authenticate a build, source revision, toolchain or runtime.

#[path = "distribution_assembly_files.rs"]
mod files;
#[cfg(test)]
#[path = "distribution_assembly_tests.rs"]
mod tests;

use super::{ArtifactManifest, Selection, SourceIdentity, verify_pair};
use crate::json::{self, Json};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub const PREDICATE_TYPE: &str =
    "https://github.com/swiftraccoon/simplestChat/attestations/ops-artifact/v1";
pub const MAX_EVIDENCE_BYTES: u64 = 1024 * 1024;
pub const MAX_EVIDENCE_MAPPING_BYTES: usize = 16 * 1024;
const MAX_RECORD: usize = 64 * 1024;
const PREREQUISITES: [&str; 5] = [
    "controlledMuslBuildAndCompleteInputClosure",
    "authenticatedSourceToolchainHostAndRegistryInputs",
    "trustedWorkflowAttestationAndFreshControllerVerification",
    "ownedDebianRuntimeEvidence",
    "transportBootstrapAndInstallationOwnership",
];

/// Fixed failures never format input paths, evidence names or external errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    Selection,
    Evidence,
    Input,
    Changed,
    Bounds,
    Output,
    Close,
    Executable,
    Encoding,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Selection => "ops_preparation_selection",
            Self::Evidence => "ops_preparation_evidence",
            Self::Input => "ops_preparation_input",
            Self::Changed => "ops_preparation_changed",
            Self::Bounds => "ops_preparation_bounds",
            Self::Output => "ops_preparation_output",
            Self::Close => "ops_preparation_close",
            Self::Executable => "ops_preparation_executable",
            Self::Encoding => "ops_preparation_encoding",
        })
    }
}
impl std::error::Error for Failure {}

/// This mapping selects local source paths, independently of manifest claims.
/// Its Debug omits both paths and logical names. No entry supplies authority to
/// traverse an arbitrary output path: payload names are fixed by this module.
pub struct EvidenceSources(BTreeMap<String, PathBuf>);
impl std::fmt::Debug for EvidenceSources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EvidenceSources")
            .field("entries", &self.0.len())
            .finish()
    }
}
impl EvidenceSources {
    pub fn new(sources: BTreeMap<String, PathBuf>) -> Result<Self, Failure> {
        if sources.len() > 16
            || sources
                .iter()
                .any(|(name, path)| !super::digest_name(name) || !files::input_path(path))
        {
            return Err(Failure::Evidence);
        }
        Ok(Self(sources))
    }

    /// An exact JSON object maps conservative logical names to absolute local
    /// paths. Shared decoding rejects duplicate keys and limits decoded bytes.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let value = json::decode_limited(bytes, MAX_EVIDENCE_MAPPING_BYTES)
            .map_err(|_| Failure::Evidence)?;
        let object = value.object().ok_or(Failure::Evidence)?;
        if object.len() > 16 {
            return Err(Failure::Evidence);
        }
        let mut sources = BTreeMap::new();
        for (name, value) in object {
            let Json::String(path) = value else {
                return Err(Failure::Evidence);
            };
            sources.insert(name.clone(), PathBuf::from(path));
        }
        Self::new(sources)
    }
    pub fn read(path: &Path) -> Result<Self, Failure> {
        Self::decode(
            &crate::files::read(path, MAX_EVIDENCE_MAPPING_BYTES).map_err(|_| Failure::Input)?,
        )
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Digest {
    sha256: String,
    bytes: u64,
}

/// This is a local preparation result, not a signature, producer success receipt
/// or readiness decision. No source paths or declared digest-map names appear.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedArtifact {
    schema_version: u8,
    kind: &'static str,
    claims: SourceIdentity,
    manifest_sha256: String,
    manifest_bytes: usize,
    preparation_sha256: String,
    preparation_bytes: u64,
    payload_count: usize,
    evidence_count: usize,
    prerequisites: [&'static str; 5],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EvidenceEntry {
    logical_name: String,
    payload: String,
    #[serde(flatten)]
    digest: Digest,
}

fn encoded(value: &impl Serialize) -> Result<Vec<u8>, Failure> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| Failure::Encoding)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_RECORD {
        return Err(Failure::Bounds);
    }
    Ok(bytes)
}

/// Borrow the manifest whose original SHA the caller may already have checked.
/// All seven identities and the exact evidence-name set are checked before any
/// binary/output access. The output must be a fresh absolute nonlink directory.
///
/// Evidence is opaque, nonempty data, at most sixteen 1 MiB files. Every declared
/// evidence SHA is measured; inputDigests remain unmeasured declarations. The
/// closed copied executable is checked by the existing pair/ELF policy. Nothing
/// executes, signs, fetches, unpacks or installs any payload.
pub fn prepare_artifact(
    manifest: &ArtifactManifest,
    binary: &Path,
    selection: &Selection,
    evidence: &EvidenceSources,
    output: &Path,
) -> Result<PreparedArtifact, Failure> {
    prepare_after(manifest, binary, selection, evidence, output, |_| {})
}

fn prepare_after(
    manifest: &ArtifactManifest,
    binary: &Path,
    selection: &Selection,
    evidence: &EvidenceSources,
    output: &Path,
    mut after: impl FnMut(&str),
) -> Result<PreparedArtifact, Failure> {
    if manifest.source != selection.0 {
        return Err(Failure::Selection);
    }
    if !evidence.0.keys().eq(manifest.evidence_digests.keys()) {
        return Err(Failure::Evidence);
    }
    if !files::input_path(binary) {
        return Err(Failure::Input);
    }
    let mut directory = files::Output::create(output)?;
    let result = (|| {
        let mut payloads = BTreeMap::new();
        payloads.insert(
            "ops-release.json".to_owned(),
            directory.write("ops-release.json", manifest.as_json())?,
        );
        payloads.insert(
            "simplestchat-ops".into(),
            directory.copy(
                "simplestchat-ops",
                binary,
                &manifest.binary.sha256,
                Some(manifest.binary.bytes),
                crate::distribution::MAX_BYTES,
            )?,
        );
        after("binaryCopied");
        let mut entries = Vec::new();
        for (index, (logical_name, source)) in evidence.0.iter().enumerate() {
            let payload = format!("ops-evidence-{index:02}.bin");
            let digest = directory.copy(
                &payload,
                source,
                &manifest.evidence_digests[logical_name],
                None,
                MAX_EVIDENCE_BYTES,
            )?;
            payloads.insert(payload.clone(), digest.clone());
            entries.push(EvidenceEntry {
                logical_name: logical_name.clone(),
                payload,
                digest,
            });
        }
        after("evidenceCopied");
        let index = encoded(&serde_json::json!({
            "schemaVersion":1,"kind":"ops-evidence-index","entries":entries,
        }))?;
        payloads.insert(
            "ops-evidence.json".into(),
            directory.write("ops-evidence.json", &index)?,
        );
        directory.verify()?;
        let pair = verify_pair(manifest, &output.join("simplestchat-ops"), selection)
            .map_err(|_| Failure::Executable)?;
        directory.verify()?;
        after("pairVerified");
        let predicate = encoded(&serde_json::json!({
            "schemaVersion":1,"kind":"ops-artifact-identity","predicateType":PREDICATE_TYPE,
            "identity":pair,"fileDigests":payloads,"evidenceCount":evidence.0.len(),
        }))?;
        payloads.insert(
            "ops-predicate.json".into(),
            directory.write("ops-predicate.json", &predicate)?,
        );
        after("beforePreparation");
        directory.verify()?;
        let preparation = encoded(&serde_json::json!({
            "schemaVersion":1,"kind":"ops-unsigned-artifact","claims":selection.identity(),
            "fileDigests":payloads,"evidenceCount":evidence.0.len(),
            "prerequisites":PREREQUISITES,
        }))?;
        let digest = directory.write("ops-preparation.json", &preparation)?;
        after("preparationWritten");
        Ok(PreparedArtifact {
            schema_version: 1,
            kind: "ops-unsigned-artifact",
            claims: selection.0.clone(),
            manifest_sha256: manifest.sha256.clone(),
            manifest_bytes: manifest.original.len(),
            preparation_sha256: digest.sha256,
            preparation_bytes: digest.bytes,
            payload_count: payloads.len() + 1,
            evidence_count: evidence.0.len(),
            prerequisites: PREREQUISITES,
        })
    })();
    if result.is_err() {
        directory.rollback();
    }
    let closed = directory.finish();
    let prepared = result?;
    closed?;
    Ok(prepared)
}
