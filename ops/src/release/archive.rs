//! Offline Docker/OCI archive validation. The signed-server profile reads bounded
//! JSON metadata; the separate generator profile also streams referenced payload
//! hashes. Layers are never unpacked or run. Results describe measured transfers
//! and selected identities, not signatures or deployment authorization.

mod docker;
mod generator;
mod index;
mod oci;

pub use generator::{
    GeneratorIdentity, GeneratorSelection, verify_generator_archive,
    verify_generator_archive_with_checkpoint,
};

use super::manifest::Manifest;
use crate::files::Snapshot;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::io::{Read, Seek};
use std::path::Path;

/// Maximum original, uncompressed transfer size. JSON members have a separate
/// 1 MiB limit; image layers are indexed by offsets without buffering them.
pub const MAX_ARCHIVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Stable non-sensitive failures. Archive paths, member names, JSON values and
/// operating-system messages never become diagnostic text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveError {
    Input,
    Changed,
    Digest,
    Read,
    Path,
    NoncanonicalPath,
    Member,
    MemberCount,
    UnsupportedTar,
    PhysicalCount,
    ExtensionBudget,
    NameBudget,
    Metadata,
    Json,
    AmbiguousJson,
    ImageCount,
    Tag,
    Layers,
    Layer,
    Layout,
    Index,
    IndexCount,
    Annotations,
    AnnotationTag,
    AnnotationMissing,
    DescriptorDigest,
    DescriptorSize,
    DescriptorUrls,
    MetadataDigest,
    ImageManifest,
    ConfigMismatch,
    LayersMismatch,
    IndexDepth,
    Repositories,
    Platform,
    User,
    Revision,
    Command,
    Budget,
}
impl ArchiveError {
    pub fn code(self) -> &'static str {
        match self {
            Self::Input => "archive_input_refused",
            Self::Changed => "archive_input_changed",
            Self::Digest => "archive_digest_mismatch",
            Self::Read => "archive_unreadable",
            Self::Path => "archive_member_path",
            Self::NoncanonicalPath => "archive_member_noncanonical",
            Self::Member => "archive_member_type_or_duplicate",
            Self::MemberCount => "archive_member_count",
            Self::UnsupportedTar => "archive_unsupported_tar",
            Self::PhysicalCount => "archive_header_count",
            Self::ExtensionBudget => "archive_extension_budget",
            Self::NameBudget => "archive_name_budget",
            Self::Metadata => "archive_metadata_missing_or_oversized",
            Self::Json => "invalid_json",
            Self::AmbiguousJson => "ambiguous_json",
            Self::ImageCount => "archive_image_count",
            Self::Tag => "archive_tag",
            Self::Layers => "archive_layers",
            Self::Layer => "archive_layer_member",
            Self::Layout => "oci_layout",
            Self::Index => "oci_index",
            Self::IndexCount => "oci_index_count",
            Self::Annotations => "oci_annotations",
            Self::AnnotationTag => "oci_annotation_tag",
            Self::AnnotationMissing => "oci_annotation_missing",
            Self::DescriptorDigest => "oci_descriptor_digest",
            Self::DescriptorSize => "oci_descriptor_size",
            Self::DescriptorUrls => "oci_descriptor_urls",
            Self::MetadataDigest => "oci_metadata_digest",
            Self::ImageManifest => "oci_image_manifest",
            Self::ConfigMismatch => "oci_config_mismatch",
            Self::LayersMismatch => "oci_layers_mismatch",
            Self::IndexDepth => "oci_index_depth",
            Self::Repositories => "archive_repositories",
            Self::Platform => "archive_platform",
            Self::User => "archive_user",
            Self::Revision => "archive_revision",
            Self::Command => "archive_command",
            Self::Budget => "archive_budget_exhausted",
        }
    }
}
impl std::fmt::Display for ArchiveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ArchiveError {}
impl From<crate::Error> for ArchiveError {
    fn from(error: crate::Error) -> Self {
        match error {
            crate::Error::Changed => Self::Changed,
            crate::Error::Json => Self::Json,
            crate::Error::AmbiguousJson => Self::AmbiguousJson,
            _ => Self::Input,
        }
    }
}
type Result<T> = std::result::Result<T, ArchiveError>;

/// The only public diagnostic projection. Whole configs can contain private
/// environment values and are deliberately excluded from serialization.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Identity {
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub config_sha256: String,
    pub revision: String,
    pub image_tag: String,
    pub platform: String,
}
pub struct VerifiedArchive {
    identity: Identity,
    config: Vec<u8>,
}
impl std::fmt::Debug for VerifiedArchive {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedArchive")
            .field("identity", &self.identity)
            .field("config_bytes", &self.config.len())
            .finish()
    }
}
impl VerifiedArchive {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// Original metadata bytes bound to the validated transfer, for a later
    /// private operation.
    /// These may contain secrets; never include them in public diagnostics.
    pub fn config_bytes(&self) -> &[u8] {
        &self.config
    }
}

#[derive(Clone, Copy)]
struct Limits {
    archive: u64,
    members: usize,
    headers: usize,
    extension: u64,
    name: usize,
    names: usize,
    metadata: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            archive: MAX_ARCHIVE_BYTES,
            members: 100_000,
            headers: 200_000,
            extension: 8192,
            name: 8192,
            names: 16 * 1024 * 1024,
            metadata: 1024 * 1024,
        }
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn checkpoint(check: &mut impl FnMut() -> bool) -> Result<()> {
    if check() {
        Ok(())
    } else {
        Err(ArchiveError::Budget)
    }
}

fn archive_hash(snapshot: &mut Snapshot) -> Result<String> {
    let limit = snapshot
        .length()
        .checked_add(1)
        .ok_or(ArchiveError::Input)?;
    let mut reader = snapshot.file_mut().take(limit);
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    let mut total = 0_u64;
    loop {
        let count = reader.read(&mut buffer).map_err(|_| ArchiveError::Read)?;
        if count == 0 {
            break;
        }
        total = total
            .checked_add(count as u64)
            .ok_or(ArchiveError::Changed)?;
        hash.update(&buffer[..count]);
    }
    if total != snapshot.length() {
        return Err(ArchiveError::Changed);
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Validate one anchored, at most 8 GiB uncompressed Docker/OCI archive against
/// an already validated manifest. Whole-transfer SHA precedes all tar parsing.
/// The retained inode and every visible directory binding are rechecked after
/// hashing, indexing and final validation, then explicitly closed before success.
///
/// Supported extensions and extra resource bounds are documented in the ops
/// guide. This snapshot does not lock out later writes by the filesystem owner.
pub fn verify_archive(path: &Path, manifest: &Manifest) -> Result<VerifiedArchive> {
    verify_after(path, manifest, Limits::default(), |_| {})
}

fn verify_after(
    path: &Path,
    manifest: &Manifest,
    limits: Limits,
    mut after: impl FnMut(&str),
) -> Result<VerifiedArchive> {
    let mut snapshot = Snapshot::open(path, limits.archive)?;
    let archive_sha256 = archive_hash(&mut snapshot)?;
    after("hash");
    snapshot.verify()?;
    if archive_sha256 != manifest.fields().archive_sha256 {
        return Err(ArchiveError::Digest);
    }
    snapshot
        .file_mut()
        .rewind()
        .map_err(|_| ArchiveError::Read)?;
    let length = snapshot.length();
    let indexed = index::Index::read(&mut snapshot.checked_stream(), length, limits);
    after("index");
    snapshot.verify()?;
    let index = indexed?;
    let config = docker::verify(snapshot.file_mut(), &index, manifest)?;
    after("validation");
    snapshot.verify()?;
    snapshot.finish()?;
    let identity = Identity {
        archive_sha256,
        archive_bytes: length,
        config_sha256: sha256(&config),
        revision: manifest.fields().revision.clone(),
        image_tag: manifest.fields().image_tag.clone(),
        platform: manifest.fields().platform.clone(),
    };
    Ok(VerifiedArchive { identity, config })
}

#[cfg(test)]
mod tests;
