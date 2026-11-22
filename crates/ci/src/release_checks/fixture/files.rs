//! Fixture files: exclusive private creation that never replaces anything,
//! bounded reads that follow no final link, the fresh evidence directory and
//! digests. Every created file is new; a partial write is left for inspection.

use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{self, Read, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// Create `path` as a new empty file of mode 0600, whatever the umask.
pub(super) fn create_private(path: &Path) -> io::Result<File> {
    let file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(file)
}

/// Create `path` with exactly `bytes`, refusing any existing entry.
pub(super) fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    create_private(path)?.write_all(bytes)
}

/// The pretty JSON of `value` with a final newline, created as [`write_new`] does.
pub(super) fn write_json(path: &Path, value: &impl serde::Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    write_new(path, &bytes)
}

/// Create one private directory of `mode`, whatever the umask.
pub(super) fn directory(path: &Path, mode: u32) -> io::Result<()> {
    fs::DirBuilder::new().mode(mode).create(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

/// At most `maximum` bytes of the regular file at `path`, which must not be
/// a link; a longer file is refused.
pub(super) fn read_bounded(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(maximum + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum {
        return Err(io::Error::other("file exceeds its bound"));
    }
    Ok(bytes)
}

/// The first `maximum` bytes of the regular file at `path`, which must not be
/// a link; a longer file is read only that far.
pub(super) fn read_prefix(path: &Path, maximum: u64) -> io::Result<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(maximum).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The SHA-256 of the regular file at `path`, read without following a link.
pub(super) fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(rustix::fs::OFlags::NOFOLLOW.bits() as i32)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("not a regular file"));
    }
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// Whether anything, a dangling link included, exists at `path`.
pub(super) fn present(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

/// The release attempts under `results`, by name.
pub(super) fn attempts(results: &Path) -> io::Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    for entry in fs::read_dir(results)? {
        let entry = entry?;
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(b"release.")
        {
            found.push(entry.path());
        }
    }
    found.sort();
    Ok(found)
}

/// Reserve the new private evidence directory `value` and its `private`
/// child, outside every service path. Its parent must be an existing
/// canonical directory; `value` must not exist, not even as a link.
pub(super) fn fresh_output(value: &Path, service: &[&Path]) -> Result<PathBuf, &'static str> {
    let parent = value
        .parent()
        .filter(|_| value.is_absolute())
        .ok_or("Output must have an existing absolute, non-symlink parent")?;
    let canonical = parent.canonicalize().ok();
    if !parent.is_dir() || canonical.as_deref() != Some(parent) {
        return Err("Output must have an existing absolute, non-symlink parent");
    }
    if value
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .any(|byte| *byte < 32 || *byte == 127)
    {
        return Err("Invalid output path");
    }
    if present(value) {
        return Err("Output must be a fresh directory");
    }
    if service.iter().any(|fixed| value.starts_with(fixed)) {
        return Err("Output must be separate from fixture service paths");
    }
    directory(value, 0o700).map_err(|_| "Output must be a fresh directory")?;
    directory(&value.join("private"), 0o700).map_err(|_| "Output must be a fresh directory")?;
    Ok(value.to_path_buf())
}
