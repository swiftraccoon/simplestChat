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

/// Verify the existing signed-server archive profile through a caller-retained
/// descriptor. The caller keeps and checks original namespace ownership and must
/// explicitly close the descriptor after this borrowed operation. `check` is the
/// same original owner: it is polled before/after every bounded read and seek.
/// A deadline does not bound a kernel read that itself blocks. No pathname is
/// reopened and no signature or deployment authority is created.
///
/// The caller retains the descriptor on success and error and must perform its
/// own checked close. The file cursor is moved and is not restored. See
/// [`super::build::run`] for the complete local controller using this interface;
/// [`verify_archive`] supplies the separate anchored pathname-inspection API.
///
/// # Errors
///
/// Returns an [`ArchiveError`] for invalid or changed input, digest/runtime
/// disagreement, I/O or a negative owner checkpoint. A caller whose checkpoint
/// records a more specific deadline/signal cause must retain that original cause.
///
/// # Panics
///
/// A panic from the caller's checkpoint propagates. It does not close the file.
///
/// # Example
///
/// This helper assumes the caller has already admitted the original file and
/// retains its namespace witnesses. It starts no process or service.
///
/// ```no_run
/// use simplestchat_ops::release::{
///     archive::{ArchiveError, VerifiedArchive, verify_retained_archive},
///     manifest::Manifest,
/// };
/// use std::{fs::File, time::Instant};
///
/// fn inspect_original(
///     archive: &mut File,
///     manifest: &Manifest,
///     original_end: Instant,
///     mut original_namespace: impl FnMut() -> bool,
/// ) -> Result<VerifiedArchive, ArchiveError> {
///     let verified = verify_retained_archive(archive, manifest, || {
///         Instant::now() < original_end && original_namespace()
///     })?;
///     Ok(verified)
/// }
/// ```
pub fn verify_retained_archive(
    file: &mut std::fs::File,
    manifest: &Manifest,
    mut check: impl FnMut() -> bool,
) -> Result<VerifiedArchive> {
    use std::os::unix::fs::MetadataExt;
    let before = file.metadata().map_err(|_| ArchiveError::Read)?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_ARCHIVE_BYTES {
        return Err(ArchiveError::Input);
    }
    fn same(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.mode() == b.mode()
            && a.uid() == b.uid()
            && a.gid() == b.gid()
            && a.nlink() == b.nlink()
            && a.len() == b.len()
            && a.mtime() == b.mtime()
            && a.mtime_nsec() == b.mtime_nsec()
            && a.ctime() == b.ctime()
            && a.ctime_nsec() == b.ctime_nsec()
    }
    struct Reader<'a, F> {
        file: &'a mut std::fs::File,
        before: &'a std::fs::Metadata,
        check: &'a mut F,
    }
    impl<F: FnMut() -> bool> Reader<'_, F> {
        fn check(&mut self) -> std::io::Result<()> {
            if !(self.check)() || !same(self.before, &self.file.metadata()?) {
                Err(std::io::Error::other("retained_archive_changed_or_expired"))
            } else {
                Ok(())
            }
        }
    }
    impl<F: FnMut() -> bool> Read for Reader<'_, F> {
        fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
            self.check()?;
            let maximum = bytes.len().min(65536);
            let count = self.file.read(&mut bytes[..maximum])?;
            self.check()?;
            Ok(count)
        }
    }
    impl<F: FnMut() -> bool> Seek for Reader<'_, F> {
        fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
            self.check()?;
            let offset = self.file.seek(position)?;
            self.check()?;
            Ok(offset)
        }
    }
    let mut reader = Reader {
        file,
        before: &before,
        check: &mut check,
    };
    reader.rewind().map_err(|_| ArchiveError::Read)?;
    let mut hash = Sha256::new();
    let mut total = 0;
    let mut buffer = [0; 65536];
    loop {
        let count = reader.read(&mut buffer).map_err(|_| ArchiveError::Read)?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > before.len() {
            return Err(ArchiveError::Changed);
        }
        hash.update(&buffer[..count]);
    }
    let archive_sha256: String = hash
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if total != before.len() {
        return Err(ArchiveError::Changed);
    }
    if archive_sha256 != manifest.fields().archive_sha256 {
        return Err(ArchiveError::Digest);
    }
    reader.rewind().map_err(|_| ArchiveError::Read)?;
    let index = index::Index::read_with_checkpoint(
        &mut reader,
        before.len(),
        Limits::default(),
        &mut || true,
    )?;
    let config = docker::verify(&mut reader, &index, manifest)?;
    reader.check().map_err(|_| ArchiveError::Changed)?;
    checkpoint(&mut check)?;
    let identity = Identity {
        archive_sha256,
        archive_bytes: before.len(),
        config_sha256: sha256(&config),
        revision: manifest.fields().revision.clone(),
        image_tag: manifest.fields().image_tag.clone(),
        platform: manifest.fields().platform.clone(),
    };
    Ok(VerifiedArchive { identity, config })
}

#[cfg(test)]
mod tests;
