//! Bounded generator artifacts. Archive metadata never selects a host path.
//!
//! Validate all members before creating any result. Copy only the three fixed
//! basenames through a fresh private directory descriptor; never unpack archive
//! paths, modes, ownership or links. A collection manifest is published last.

use anyhow::{Result, ensure};
use nix::fcntl::{AtFlags, OFlag, open, openat};
use nix::sys::stat::{Mode, SFlag, fstat, fstatat, mkdirat};
use nix::unistd::{UnlinkatFlags, unlinkat};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
#[cfg(test)]
use std::fs::DirBuilder;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(test)]
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

pub(crate) const MAX_RESULT: u64 = 128 * 1024 * 1024;
pub(crate) const MAX_ARCHIVE: u64 = 144 * 1024 * 1024;
const MAX_SUMMARY: u64 = 8 * 1024 * 1024;
const MAX_TAR_METADATA: u64 = 8192;
const TAR_BLOCK: u64 = 512;
const SUMMARY: &str = "load_test_summary.json";
const RESULTS: &str = "load_test_results.json";
const TIMEOUT: &str = "load_test_timeout.json";
const CAPTURE: &str = "handshakes.pcap";

/// Fixed collectors share parsing policy, never archive-selected host paths.
#[derive(Clone, Copy)]
enum Collection {
    Capacity,
    Benchmark,
    Capture,
}

impl Collection {
    fn maximum_archive(self) -> u64 {
        match self {
            Self::Capacity | Self::Benchmark => MAX_ARCHIVE,
            Self::Capture => 65 * 1024 * 1024,
        }
    }

    fn maximum_file(self) -> u64 {
        match self {
            Self::Capacity | Self::Benchmark => MAX_RESULT,
            Self::Capture => 64 * 1024 * 1024,
        }
    }

    fn name(self, name: &str) -> Result<&'static str> {
        match (self, name) {
            (Self::Capacity | Self::Benchmark, SUMMARY) => Ok(SUMMARY),
            (Self::Capacity | Self::Benchmark, RESULTS) => Ok(RESULTS),
            (Self::Capacity | Self::Benchmark, TIMEOUT) => Ok(TIMEOUT),
            (Self::Capture, CAPTURE) => Ok(CAPTURE),
            _ => anyhow::bail!("unsafe_result_entry"),
        }
    }
}

pub(crate) fn regular(path: &Path, maximum: u64) -> Result<File> {
    let file = File::from(open(
        path,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "result_not_regular");
    ensure!(metadata.len() <= maximum, "result_too_large");
    Ok(file)
}

pub(crate) fn read_regular(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    let mut file = regular(path, maximum)?;
    let size = file.metadata()?.len();
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(maximum + 1)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() as u64 == size, "result_size_changed");
    nix::unistd::close(file)?;
    Ok(bytes)
}

pub(crate) fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let descriptor = open(
        path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?;
    let mut file = File::from(descriptor);
    file.write_all(bytes)?;
    nix::unistd::close(file)?;
    Ok(())
}

/// Bound the physical metadata before the mature tar reader parses PAX bodies.
fn check_headers(stream: &mut File, maximum_headers: usize, maximum_file: u64) -> Result<()> {
    let total = stream.seek(SeekFrom::End(0))?;
    ensure!(total <= MAX_ARCHIVE, "result_archive_too_large");
    stream.rewind()?;
    let mut headers = 0;
    while stream.stream_position()? < total {
        let mut bytes = [0_u8; TAR_BLOCK as usize];
        stream
            .read_exact(&mut bytes)
            .map_err(|_| anyhow::anyhow!("truncated_tar_header"))?;
        if bytes.iter().all(|byte| *byte == 0) {
            let mut marker = [0_u8; TAR_BLOCK as usize];
            ensure!(
                stream.read_exact(&mut marker).is_ok() && marker.iter().all(|byte| *byte == 0),
                "missing_tar_end_marker"
            );
            let mut buffer = [0; 65536];
            loop {
                let count = stream.read(&mut buffer)?;
                ensure!(stream.stream_position()? <= total, "result_archive_changed");
                if count == 0 {
                    break;
                }
                ensure!(
                    buffer[..count].iter().all(|byte| *byte == 0),
                    "trailing_tar_data"
                );
            }
            stream.rewind()?;
            return Ok(());
        }
        headers += 1;
        ensure!(headers <= maximum_headers, "too_many_tar_headers");
        let header = tar::Header::from_byte_slice(&bytes);
        let kind = header.entry_type().as_byte();
        ensure!([0, b'0', b'5', b'x'].contains(&kind), "unsafe_tar_header");
        let size = header
            .size()
            .map_err(|_| anyhow::anyhow!("invalid_tar_header"))?;
        let maximum = if kind == b'x' {
            MAX_TAR_METADATA
        } else {
            maximum_file
        };
        ensure!(size <= maximum, "oversized_tar_body");
        let following = stream
            .stream_position()?
            .checked_add(size.div_ceil(TAR_BLOCK) * TAR_BLOCK)
            .ok_or_else(|| anyhow::anyhow!("truncated_tar_body"))?;
        ensure!(following <= total, "truncated_tar_body");
        stream.seek(SeekFrom::Start(following))?;
    }
    anyhow::bail!("missing_tar_end_marker")
}

#[derive(PartialEq, Eq)]
struct Identity {
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
    changed: (i64, i64),
}

fn identity(file: &File) -> Result<Identity> {
    let value = file.metadata()?;
    Ok(Identity {
        device: value.dev(),
        inode: value.ino(),
        bytes: value.len(),
        modified: (value.mtime(), value.mtime_nsec()),
        changed: (value.ctime(), value.ctime_nsec()),
    })
}

struct Selected {
    name: &'static str,
    offset: u64,
    size: u64,
}

struct Output {
    name: &'static str,
    identity: Identity,
}

impl Output {
    fn verify(&self, directory: &File) -> Result<()> {
        let file = File::from(openat(
            directory,
            self.name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        ensure!(
            file.metadata()?.is_file() && identity(&file)? == self.identity,
            "result_output_changed"
        );
        nix::unistd::close(file)?;
        Ok(())
    }
}

struct Extracted {
    directory: File,
    outputs: Vec<Output>,
    manifest: Value,
}

impl Extracted {
    /// Reopen only the fixed basenames under the retained directory. A link,
    /// replacement or in-place write cannot inherit a prior successful hash.
    fn verify(&self) -> Result<()> {
        for output in &self.outputs {
            output.verify(&self.directory)?;
        }
        Ok(())
    }
}

fn directory(path: &Path) -> Result<File> {
    Ok(File::from(open(
        path,
        OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?))
}

fn visible_directory(path: &Path, directory: &File) -> Result<()> {
    let visible = fs::symlink_metadata(path)?;
    let owned = directory.metadata()?;
    ensure!(
        visible.is_dir() && visible.dev() == owned.dev() && visible.ino() == owned.ino(),
        "result_directory_changed"
    );
    Ok(())
}

/// Cleanup may remove only the inode created by this collection, even when a
/// later identity/content check failed. A foreign replacement is retained.
fn owned_name(directory: &File, name: &std::ffi::OsStr, expected: &Identity) -> Result<()> {
    let file = File::from(openat(
        directory,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    let current = identity(&file)?;
    ensure!(
        file.metadata()?.is_file()
            && current.device == expected.device
            && current.inode == expected.inode,
        "result_output_replaced"
    );
    nix::unistd::close(file)?;
    Ok(())
}

fn visible_results(root: &File, extracted: &Extracted, collection: Collection) -> Result<()> {
    if matches!(collection, Collection::Capacity) {
        let visible = fstatat(root, "generator", AtFlags::AT_SYMLINK_NOFOLLOW)?;
        let owned = fstat(&extracted.directory)?;
        ensure!(
            SFlag::from_bits_truncate(visible.st_mode) == SFlag::S_IFDIR
                && visible.st_dev == owned.st_dev
                && visible.st_ino == owned.st_ino,
            "result_directory_changed"
        );
    }
    extracted.verify()
}

fn selected(file: &mut File, collection: Collection) -> Result<Vec<Selected>> {
    let archive_bytes = file.metadata()?.len();
    let mut archive = tar::Archive::new(file);
    let mut seen = BTreeSet::new();
    let mut selected = Vec::new();
    let mut total = 0_u64;
    let entries = archive
        .entries_with_seek()
        .map_err(|_| anyhow::anyhow!("invalid_result_archive"))?;
    for entry in entries {
        let mut entry = entry.map_err(|_| anyhow::anyhow!("invalid_result_archive"))?;
        if let Some(extensions) = entry
            .pax_extensions()
            .map_err(|_| anyhow::anyhow!("invalid_result_archive"))?
        {
            let mut keys = BTreeSet::new();
            for extension in extensions {
                let extension = extension.map_err(|_| anyhow::anyhow!("invalid_result_archive"))?;
                // Python takes the last duplicate; tar takes the first. Refuse
                // ambiguous metadata rather than give it competing meanings.
                ensure!(
                    !extension.key_bytes().starts_with(b"GNU.sparse")
                        && keys.insert(extension.key_bytes().to_vec()),
                    "unsafe_result_entry"
                );
            }
        }
        let path = entry.path_bytes();
        let name =
            std::str::from_utf8(&path).map_err(|_| anyhow::anyhow!("unsafe_result_entry"))?;
        if entry.header().entry_type().is_dir() && [".", "./"].contains(&name) {
            ensure!(seen.insert("."), "duplicate_result_entry");
            continue;
        }
        let name = name.strip_prefix("./").unwrap_or(name);
        let name = collection.name(name)?;
        ensure!(
            entry.header().entry_type().is_file() && entry.link_name_bytes().is_none(),
            "unsafe_result_entry"
        );
        ensure!(seen.insert(name), "duplicate_result_entry");
        let size = entry.size();
        ensure!(
            size <= if name == SUMMARY || name == TIMEOUT {
                MAX_SUMMARY
            } else {
                collection.maximum_file()
            },
            "result_file_too_large"
        );
        total = total
            .checked_add(size)
            .ok_or_else(|| anyhow::anyhow!("result_archive_too_large"))?;
        ensure!(
            total <= collection.maximum_archive(),
            "result_archive_too_large"
        );
        let offset = entry.raw_file_position();
        ensure!(
            offset
                .checked_add(size)
                .is_some_and(|end| end <= archive_bytes),
            "truncated_result_body"
        );
        selected.push(Selected { name, offset, size });
    }
    let required = match collection {
        Collection::Capacity | Collection::Benchmark => {
            seen.contains(SUMMARY) && seen.contains(RESULTS)
        }
        Collection::Capture => seen.contains(CAPTURE),
    };
    ensure!(required, "missing_required_results");
    ensure!(!seen.contains(TIMEOUT), "generator_timeout_marker");
    Ok(selected)
}

fn extract_into(file: &mut File, directory: File, collection: Collection) -> Result<Extracted> {
    let original = identity(file)?;
    ensure!(
        original.bytes <= collection.maximum_archive(),
        "result_archive_too_large"
    );
    check_headers(file, 8, collection.maximum_file())?;
    let entries = selected(file, collection)?;
    ensure!(identity(file)? == original, "result_archive_changed");
    let mut files = Map::new();
    let mut outputs = Vec::new();
    for entry in entries {
        file.seek(SeekFrom::Start(entry.offset))?;
        let mut output = File::from(openat(
            &directory,
            entry.name,
            OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let mut digest = Sha256::new();
        let mut remaining = entry.size;
        let mut buffer = [0; 65536];
        while remaining > 0 {
            let count = file.read(&mut buffer[..usize::try_from(remaining.min(65536)).unwrap()])?;
            ensure!(count > 0, "truncated_result_body");
            output.write_all(&buffer[..count])?;
            digest.update(&buffer[..count]);
            remaining -= count as u64;
        }
        let written = identity(&output)?;
        ensure!(written.bytes == entry.size, "result_output_changed");
        nix::unistd::close(output)?;
        outputs.push(Output {
            name: entry.name,
            identity: written,
        });
        files.insert(
            entry.name.into(),
            json!({"bytes":entry.size,"sha256":hex(&digest.finalize())}),
        );
    }
    ensure!(identity(file)? == original, "result_archive_changed");
    let result = Extracted {
        directory,
        outputs,
        manifest: json!({"schemaVersion":1,"complete":true,"files":files}),
    };
    result.verify()?;
    Ok(result)
}

#[cfg(test)]
pub(crate) fn extract(archive: &Path, destination: &Path) -> Result<Value> {
    let mut file = regular(archive, u64::MAX)?;
    DirBuilder::new().mode(0o700).create(destination)?;
    let result = extract_into(&mut file, directory(destination)?, Collection::Capacity)?;
    nix::unistd::close(file)?;
    visible_directory(destination, &result.directory)?;
    nix::unistd::close(result.directory)?;
    Ok(result.manifest)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Call only after successful bounded engine transfer and closed output files.
pub(crate) fn finish_collection(archive: &Path, directory: &Path) -> Result<Value> {
    finish(archive, directory, "collection.json", Collection::Capacity)
}

/// Publish only fixed JSON result basenames into an existing private run root.
/// Failed transfers, invalid archives and timeout markers retain their archive;
/// the collection manifest is created only after every output is closed.
pub(crate) fn finish_benchmark_results(archive: &Path, directory: &Path) -> Result<Value> {
    finish(archive, directory, "collection.json", Collection::Benchmark)
}

/// A stopped capture sidecar may contribute only its bounded regular pcap.
pub(crate) fn finish_handshake_capture(archive: &Path, directory: &Path) -> Result<Value> {
    finish(
        archive,
        directory,
        "capture-collection.json",
        Collection::Capture,
    )
}

fn finish(
    archive: &Path,
    directory: &Path,
    manifest_name: &'static str,
    collection: Collection,
) -> Result<Value> {
    ensure!(
        archive.parent() == Some(directory),
        "invalid_result_archive_location"
    );
    let name = archive
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("invalid_result_archive_location"))?;
    let root = self::directory(directory)?;
    visible_directory(directory, &root)?;
    let mut file = File::from(openat(
        &root,
        name,
        OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?);
    ensure!(file.metadata()?.is_file(), "result_not_regular");
    let original = identity(&file)?;
    let original_stat = fstat(&file)?;
    let destination = if matches!(collection, Collection::Capacity) {
        mkdirat(&root, "generator", Mode::from_bits_truncate(0o700))?;
        File::from(openat(
            &root,
            "generator",
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?)
    } else {
        root.try_clone()?
    };
    let mut extracted = extract_into(&mut file, destination, collection)?;
    file.rewind()?;
    let mut digest = Sha256::new();
    let mut count = 0_u64;
    let mut buffer = [0; 65536];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        count += read as u64;
        ensure!(
            count <= collection.maximum_archive(),
            "result_archive_too_large"
        );
        digest.update(&buffer[..read]);
    }
    ensure!(
        identity(&file)? == original && count == original.bytes,
        "result_archive_changed"
    );
    extracted.manifest["archiveSha256"] = hex(&digest.finalize()).into();
    extracted.manifest["archiveBytes"] = count.into();
    nix::unistd::close(file)?;
    let current = fstatat(&root, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    ensure!(
        SFlag::from_bits_truncate(current.st_mode) == SFlag::S_IFREG
            && current.st_dev == original_stat.st_dev
            && current.st_ino == original_stat.st_ino,
        "result_archive_changed"
    );
    visible_directory(directory, &root)?;
    visible_results(&root, &extracted, collection)?;
    let mut bytes = serde_json::to_vec_pretty(&extracted.manifest)?;
    bytes.push(b'\n');
    let mut manifest = File::from(openat(
        &root,
        manifest_name,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    manifest.write_all(&bytes)?;
    let published = Output {
        name: manifest_name,
        identity: identity(&manifest)?,
    };
    nix::unistd::close(manifest)?;
    let publication = (|| -> Result<()> {
        visible_directory(directory, &root)?;
        visible_results(&root, &extracted, collection)?;
        published.verify(&root)?;
        let archive = File::from(openat(
            &root,
            name,
            OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        ensure!(
            archive.metadata()?.is_file() && identity(&archive)? == original,
            "result_archive_changed"
        );
        nix::unistd::close(archive)?;
        visible_directory(directory, &root)?;
        // All validation precedes this cleanup. Like every filesystem snapshot,
        // this does not lock out later writes by another process of the owner.
        unlinkat(&root, name, UnlinkatFlags::NoRemoveDir)?;
        Ok(())
    })();
    if let Err(error) = publication {
        owned_name(
            &root,
            std::ffi::OsStr::new(manifest_name),
            &published.identity,
        )?;
        unlinkat(&root, manifest_name, UnlinkatFlags::NoRemoveDir)
            .map_err(|_| anyhow::anyhow!("result_manifest_cleanup_failed"))?;
        return Err(error);
    }
    nix::unistd::close(extracted.directory)?;
    nix::unistd::close(root)?;
    Ok(extracted.manifest)
}

#[cfg(test)]
#[path = "artifacts_tests.rs"]
mod tests;
