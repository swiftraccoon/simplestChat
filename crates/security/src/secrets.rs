//! Sources as a secret scanner must read them.
//!
//! The scanner's defaults skip lock files, binary extensions and dependency
//! directories, and it sniffs content. A copy under neutral names removes the
//! first blind spot; a printable-ASCII projection behind a fixed text prefix
//! removes the second. A private map keeps every projection bound to its
//! original path and bytes, and each reviewed fixture exception is carried
//! over to the one neutral name of its file, never to a value anywhere else.
//!
//! This is printable-string coverage: nothing is decrypted, decompressed or
//! decoded, and a string split by other bytes is not put together again.

use crate::{
    Fault, Result,
    context::Limits,
    digest, files, findings, hexadecimal,
    record::{self, pretty, quoted, safe_name},
    require, scope,
};
use rustix::fs::{Mode, OFlags};
use serde_json::{Map, Value, json};
use sha2::{Digest as _, Sha256};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs::{DirBuilder, File, Metadata, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

/// What every projection starts with.
const TITLE: &[u8] = b"SimplestChat printable secret-scan projection\n";
/// The line ends after the title. The pinned scanner's file-type matcher
/// looks for a signature through offset 32773, so they reach past it.
const PADDING: usize = 32768 + 64;
/// The name of the projection.
pub const FORMAT: &str = "ascii-printable";
/// What a projection does not cover, as the evidence states it.
pub const LIMITATIONS: [&str; 3] = [
    "Only contiguous printable ASCII strings and their scanner-supported encodings are covered",
    "UTF-16, compression, encryption and strings split by nonprintable bytes are not decoded",
    "Lines refer to the projection; original paths and byte hashes remain authoritative",
];
/// What the snapshot adds to them.
const HISTORY: &str =
    "Git history checks cover textual diffs; projection covers the selected current tree";
/// How every translated configuration starts.
const HEAD: &str = "title = 'Reviewed source fixtures mapped to neutral paths'\n\
                    [extend]\nuseDefault = true\n\n";
/// The room that must stay free beyond the projections.
const RESERVE: u64 = 64 * 1024 * 1024;
/// The most bytes of a source manifest.
const MANIFEST: u64 = 4 * 1024 * 1024;

/// One file as the scanner reads it, bound to the file's own bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Projection {
    /// The SHA-256 of the file.
    pub source_sha256: String,
    /// The size of the file.
    pub source_bytes: u64,
    /// The SHA-256 of what the scanner reads.
    pub projection_sha256: String,
    /// The size of what the scanner reads.
    pub projection_bytes: u64,
}

fn prefix() -> Vec<u8> {
    let mut prefix = TITLE.to_vec();
    prefix.resize(TITLE.len() + PADDING, b'\n');
    prefix
}

/// What tells one state of a file from any other.
fn identity(metadata: &Metadata) -> (u64, u64, u64, [i64; 4]) {
    let times = [
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec(),
    ];
    (metadata.dev(), metadata.ino(), metadata.size(), times)
}

/// Write the projection of the regular file `source`, of at most `limit`
/// bytes, to the new private file `destination`: the prefix, then the file
/// with every byte that is not printable ASCII, a tab or a line end turned
/// into a line end. The file must be the same before, after and by its name.
pub fn project(source: &Path, destination: &Path, limit: u64) -> Result<Projection> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let incoming = rustix::fs::open(source, flags, Mode::empty())
        .map(File::from)
        .map_err(|error| files::unusable(&error.into()))?;
    let before = incoming
        .metadata()
        .map_err(|error| files::unusable(&error))?;
    require(before.is_file(), "secret_projection_source_kind")?;
    require(before.len() <= limit, "secret_projection_source_size")?;
    let mut options = OpenOptions::new();
    let mut outgoing = options
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(destination)
        .map_err(|error| files::unusable(&error))?;
    let prefix = prefix();
    let (mut original, mut projected) = (Sha256::new(), Sha256::new());
    projected.update(&prefix);
    outgoing
        .write_all(&prefix)
        .map_err(|error| files::unusable(&error))?;
    let mut total = 0_u64;
    let mut chunk = vec![0_u8; 65536];
    // No more is read than the file held when it was opened, however it grows.
    let mut reader = (&incoming).take(before.len());
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(files::unusable(&error)),
        };
        total += count as u64;
        original.update(&chunk[..count]);
        for byte in &mut chunk[..count] {
            // A line feed is none of these and becomes itself.
            if !matches!(*byte, b'\t' | b'\r' | b' '..=b'~') {
                *byte = b'\n';
            }
        }
        projected.update(&chunk[..count]);
        outgoing
            .write_all(&chunk[..count])
            .map_err(|error| files::unusable(&error))?;
    }
    let after = incoming
        .metadata()
        .map_err(|error| files::unusable(&error))?;
    let named = source
        .symlink_metadata()
        .map_err(|error| files::unusable(&error))?;
    require(
        unchanged(total, &before, &after, &named),
        "secret_projection_source_changed",
    )?;
    Ok(Projection {
        source_sha256: hexadecimal(&original.finalize()),
        source_bytes: total,
        projection_sha256: hexadecimal(&projected.finalize()),
        projection_bytes: total + prefix.len() as u64,
    })
}

/// Whether `total` bytes are all of the file that was opened, and that file
/// is still what it was and still what its name leads to.
fn unchanged(total: u64, before: &Metadata, after: &Metadata, named: &Metadata) -> bool {
    total == before.len()
        && identity(before) == identity(after)
        && identity(before) == identity(named)
}

/// The bytes the filesystem that holds `path` still has for this account.
pub fn free(path: &Path) -> Result<u64> {
    let usage = rustix::fs::statvfs(path).map_err(|error| files::unusable(&error.into()))?;
    Ok(usage.f_bavail.saturating_mul(usage.f_frsize))
}

/// Where the scanner's input and its configuration are, and what the input
/// covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Neutral {
    /// The directory of projections under neutral names.
    pub inputs: PathBuf,
    /// The configuration whose fixture paths are those neutral names.
    pub configuration: PathBuf,
    /// How many files were projected.
    pub files: usize,
    /// The bytes of those files.
    pub source_bytes: u64,
    /// The bytes of their projections.
    pub projection_bytes: u64,
    /// The SHA-256 of the coverage evidence.
    pub coverage_sha256: String,
}

/// Project every selected file of the snapshot `source`, which the manifest
/// in `output` lists with its SHA-256, into `output` under neutral names, and
/// translate the gitleaks `configuration` to those names.
///
/// Every file is measured before any is projected: together with a prefix
/// each they must fit the bounds and leave `free` bytes less a reserve. Each
/// projection must be of the bytes the manifest names. A fixture whose file
/// is gone is left out of the translation; one that finds two files is
/// refused.
pub fn neutral_snapshot(
    output: &Path,
    source: &Path,
    configuration: &Path,
    include_vendor: bool,
    limits: &Limits,
    free: u64,
) -> Result<Neutral> {
    let manifest = files::bounded(&output.join("source-manifest.json"), MANIFEST, false)?;
    let document = record::document(&manifest)?;
    let Value::Object(listed) = &document else {
        return Err(Fault("scanner_object_expected"));
    };
    let chosen = |(name, _): &(&str, &Value)| scope::selected(name, include_vendor);
    let named = listed
        .iter()
        .map(|(name, identity)| (name.as_str(), identity));
    let selected: BTreeMap<&str, &Value> = named.filter(chosen).collect();
    let selected: Vec<(&str, &Value)> = selected.into_iter().collect();
    require(
        !selected.is_empty() && selected.len() <= limits.files,
        "secret_source_count",
    )?;
    let mut total = 0_u64;
    for (name, _) in &selected {
        require(safe_name(name), "security_source_path")?;
        let metadata = source
            .join(name)
            .symlink_metadata()
            .map_err(|error| files::unusable(&error))?;
        require(metadata.is_file(), "secret_source_kind")?;
        require(metadata.len() <= limits.source, "secret_source_size")?;
        total += metadata.len();
        require(total <= limits.tree, "secret_tree_size")?;
    }
    let expected = total + selected.len() as u64 * prefix().len() as u64;
    require(
        free >= expected.saturating_add(RESERVE),
        "secret_projection_disk_space",
    )?;
    let inputs = output.join("secret-inputs");
    DirBuilder::new()
        .mode(0o700)
        .create(&inputs)
        .map_err(|error| files::unusable(&error))?;
    let (mut paths, mut entries) = (Map::new(), Map::new());
    let (mut sources, mut projections) = (0_u64, 0_u64);
    for (index, (name, identity)) in selected.iter().enumerate() {
        let neutral = format!("source{index:06}");
        let projection = project(&source.join(name), &inputs.join(&neutral), limits.source)?;
        require(
            identity.as_str() == Some(&projection.source_sha256),
            "secret_snapshot_changed",
        )?;
        sources += projection.source_bytes;
        projections += projection.projection_bytes;
        let entry = json!({
            "path": name,
            "sha256": projection.source_sha256,
            "sourceBytes": projection.source_bytes,
            "projectionSha256": projection.projection_sha256,
            "projectionBytes": projection.projection_bytes,
            "projectionFormat": FORMAT,
        });
        entries.insert(neutral.clone(), entry);
        paths.insert((*name).to_owned(), neutral.into());
    }
    // A file measured at one size and projected at another, though each time
    // unchanged while it was read.
    require(
        projections == expected,
        "secret_projection_coverage_changed",
    )?;
    let paths_len = paths.len();
    let mapped = pretty(&Value::Object(paths));
    files::write_private(&output.join("secret-paths.json"), mapped.as_bytes(), 0o600)?;
    let [first, second, third] = LIMITATIONS;
    let coverage = json!({
        "schemaVersion": 1,
        "includeVendor": include_vendor,
        "sourceManifestSha256": digest(&manifest),
        "files": entries.len(),
        "sourceBytes": sources,
        "projectionBytes": projections,
        "entries": entries,
        "limitations": [first, second, third, HISTORY],
    });
    let coverage = pretty(&coverage);
    let (files, coverage_sha256) = (paths_len, digest(coverage.as_bytes()));
    files::write_private(
        &output.join("secret-coverage.json"),
        coverage.as_bytes(),
        0o600,
    )?;
    let document = findings::configuration(configuration)?;
    let mut translated = String::from(HEAD);
    for fixture in findings::fixtures(&document)? {
        let fixture = fixture?;
        let place = fixture.place()?;
        let mut found = (0..selected.len()).filter(|index| place.finds(selected[*index].0));
        let Some(index) = found.next() else {
            // A fixture that is gone has no bytes to exempt.
            continue;
        };
        require(found.next().is_none(), "ambiguous_fixture_source")?;
        let _ = write!(
            translated,
            "[[allowlists]]\ndescription = {}\ncondition = \"AND\"\npaths = [{}]\n\
             regexTarget = {}\nregexes = [{}]\n\n",
            quoted(fixture.description),
            quoted(&format!("(^|/)source{index:06}$")),
            quoted(place.target),
            quoted(place.pattern),
        );
    }
    let configuration = output.join("secret-configuration.toml");
    files::write_private(&configuration, translated.as_bytes(), 0o600)?;
    Ok(Neutral {
        inputs,
        configuration,
        files,
        source_bytes: sources,
        projection_bytes: projections,
        coverage_sha256,
    })
}

#[cfg(test)]
mod tests;
