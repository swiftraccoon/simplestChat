//! Owned files and directories: nothing is read through a final link, and
//! nothing another account could have written is trusted.

use super::{Fault, Result, require};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

/// An entry that is not there, or one that cannot be used.
pub(super) fn unusable(error: &std::io::Error) -> Fault {
    if error.kind() == std::io::ErrorKind::NotFound {
        Fault("tool_entry_missing")
    } else {
        Fault("tool_entry_unavailable")
    }
}

fn owned(metadata: &std::fs::Metadata) -> bool {
    metadata.uid() == rustix::process::getuid().as_raw() && metadata.mode() & 0o022 == 0
}

/// The whole of a regular, nonempty file of at most `limit` bytes. A `private`
/// file must also be the caller's own and writable by nobody else.
pub(super) fn bounded(path: &Path, limit: u64, private: bool) -> Result<Vec<u8>> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = rustix::fs::open(path, flags, Mode::empty())
        .map(File::from)
        .map_err(|error| unusable(&error.into()))?;
    let metadata = file.metadata().map_err(|error| unusable(&error))?;
    require(metadata.is_file(), "nonregular_tool_file")?;
    require(
        metadata.len() > 0 && metadata.len() <= limit,
        "tool_file_size",
    )?;
    require(!private || owned(&metadata), "unprotected_tool_file")?;
    let mut content = Vec::new();
    file.take(limit.saturating_add(1))
        .read_to_end(&mut content)
        .map_err(|error| unusable(&error))?;
    require(content.len() as u64 == metadata.len(), "tool_file_changed")?;
    Ok(content)
}

/// A real directory of the caller's that nobody else may write.
pub(super) fn private_directory(path: &Path) -> Result<()> {
    let metadata = path.symlink_metadata().map_err(|error| unusable(&error))?;
    require(
        metadata.is_dir() && owned(&metadata),
        "unprotected_tool_directory",
    )
}

/// Create `path` and nothing else, and have its bytes on disk before it is
/// published.
pub(super) fn write_private(path: &Path, content: &[u8], mode: u32) -> Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(path)
        .map_err(|error| unusable(&error))?;
    file.write_all(content)
        .and_then(|()| file.sync_all())
        .map_err(|error| unusable(&error))
}
