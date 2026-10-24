//! Match a pinned original manifest to explicitly selected identities and one
//! local executable. Claims never substitute for inspected or authenticated facts.

use super::{ArtifactManifest, Failure, Selection, SourceIdentity};
use crate::distribution::{self, Architecture, ExecutableIdentity, SelectedExecutable};
use serde::Serialize;
use std::path::Path;

/// Fixed public projection. Map entries/source bytes stay private; counts do not
/// establish that an input/evidence file was present, read, signed or verified.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VerifiedPair {
    manifest_sha256: String,
    manifest_bytes: usize,
    claims: Claims,
    executable: ExecutableIdentity,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Claims {
    schema_version: u8,
    artifact_type: &'static str,
    #[serde(flatten)]
    source: SourceIdentity,
    input_digest_count: usize,
    evidence_digest_count: usize,
}

impl VerifiedPair {
    /// Borrow the measured executable facts after matching the retained original
    /// manifest and explicit selection. These facts do not authenticate the
    /// origin of those inputs or authorize installation by themselves.
    pub fn executable(&self) -> &ExecutableIdentity {
        &self.executable
    }
}

/// Use the already parsed manifest object after any external authentication of
/// its SHA, retaining exactly those original bytes. The manifest path is never
/// reopened. Binary admission reuses the inspector's whole-byte identity,
/// mutation/namespace checks and explicit close success boundary.
///
/// The target declaration is checked against x86-64 ELF facts with no interpreter,
/// DT_NEEDED or loader-hook entries. This does not prove musl, CLI behavior,
/// build provenance, ABI/runtime compatibility, signatures or install authority.
pub fn verify_pair(
    manifest: &ArtifactManifest,
    binary: &Path,
    selection: &Selection,
) -> Result<VerifiedPair, Failure> {
    if manifest.source != selection.0 {
        return Err(Failure::Identity);
    }
    let selected = SelectedExecutable::new(
        &manifest.binary.sha256,
        manifest.binary.bytes,
        Architecture::X86_64,
    )
    .map_err(|_| Failure::Binary)?;
    let executable = distribution::verify(binary, &selected).map_err(|_| Failure::Executable)?;
    if !executable.no_interpreter || !executable.no_needed || !executable.no_loader_hooks {
        return Err(Failure::Linkage);
    }
    Ok(VerifiedPair {
        manifest_sha256: manifest.sha256.clone(),
        manifest_bytes: manifest.original.len(),
        claims: Claims {
            schema_version: 1,
            artifact_type: "simplestchat-ops",
            source: manifest.source.clone(),
            input_digest_count: manifest.input_digests.len(),
            evidence_digest_count: manifest.evidence_digests.len(),
        },
        executable,
    })
}
