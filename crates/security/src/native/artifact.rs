//! What was built, by its bytes: a file of the build is named with its
//! digest and its size, and the server with the graph of dependencies
//! that it carries.

use super::problem;
use crate::{
    digest,
    document::Document,
    elf, hexadecimal,
    vendor::{Problem, Verdict},
};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest as _, Sha256};
use std::{
    fs::{File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

/// The most bytes a file of the build may have.
const MAX_ARTIFACT: u64 = 512 * 1024 * 1024;
/// How a static library that holds its own members begins.
pub(super) const ARCHIVE: &[u8; 8] = b"!<arch>\n";

/// The record of the regular file `path` of at most `most` bytes, read
/// through no link.
fn hashed(path: &Path, archive: bool, most: u64) -> Verdict<Document> {
    let unreadable = || Problem(format!("Unreadable file: {}", path.display()));
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let opened = rustix::fs::open(path, flags, Mode::empty());
    let mut file = opened.map(File::from).map_err(|_| unreadable())?;
    let before = file.metadata().map_err(|_| unreadable())?;
    if !before.is_file() || before.len() == 0 || before.len() > most {
        return Err(problem("Missing or excessive native artifact"));
    }
    let mut hash = Sha256::new();
    let mut size = 0_u64;
    let mut block = vec![0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut block).map_err(|_| unreadable())?;
        if read == 0 {
            break;
        }
        // The file is regular and has bytes: what it has of its first
        // eight comes with the first read.
        if archive && size == 0 && !block[..read].starts_with(ARCHIVE) {
            return Err(problem(
                "Native library must be a regular self-contained ar archive",
            ));
        }
        size += read as u64;
        if size > most {
            return Err(problem("Native artifact size limit exceeded"));
        }
        hash.update(&block[..read]);
    }
    let after = file.metadata().map_err(|_| unreadable())?;
    let identity = |held: &Metadata| {
        let (modified, changed) = (held.mtime_nsec(), held.ctime_nsec());
        (held.len(), held.mtime(), modified, held.ctime(), changed)
    };
    if identity(&before) != identity(&after) {
        return Err(problem("Native artifact changed while being hashed"));
    }
    Ok(Document::record([
        ("path", path.display().to_string().into()),
        ("sha256", hexadecimal(&hash.finalize()).into()),
        ("size", size.into()),
    ]))
}

/// The record of the regular file `path` of the build, read through no
/// link: where it is, its digest and its size. A static library must hold
/// its own members.
pub fn record_hash(path: &Path, archive: bool) -> Verdict<Document> {
    hashed(path, archive, MAX_ARTIFACT)
}

/// The record of the server at `path` and the graph of dependencies that
/// the same bytes carry.
pub fn binary_dependencies(path: &Path) -> Verdict<(Document, Document)> {
    let binary = record_hash(path, false)?;
    let data = elf::read_binary(path).map_err(|stop| match stop {
        elf::Stop::Refused(refusal) => Problem(refusal.reason),
        _ => Problem(format!("Unreadable file: {}", path.display())),
    })?;
    let same = binary.get("sha256").and_then(Document::text) == Some(&digest(&data))
        && binary.get("size") == Some(&data.len().into());
    if !same {
        return Err(problem(
            "Production binary changed during metadata inspection",
        ));
    }
    let byte = |at: usize| u16::from(data.get(at).copied().unwrap_or(0));
    let platform = match byte(18) | (byte(19) << 8) {
        62 => "linux/amd64",
        183 => "linux/arm64",
        _ => return Err(problem("Unsupported production binary architecture")),
    };
    let code = |fault: crate::Fault| Problem(fault.0.to_owned());
    let parsed = elf::parse(&data, platform).map_err(code)?;
    let metadata = elf::dependency_metadata(parsed.audit.as_deref()).map_err(code)?;
    Ok((binary, metadata))
}

#[cfg(test)]
#[path = "artifact_tests.rs"]
mod tests;
