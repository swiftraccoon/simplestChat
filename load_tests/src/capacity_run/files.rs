//! Fresh private run paths and descriptor-bound journal replacement.

use anyhow::{Result, ensure};
use nix::fcntl::{OFlag, open, openat, renameat};
use nix::sys::stat::Mode;
use std::fs::{DirBuilder, File};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::Path;

pub(super) fn directory(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    DirBuilder::new().mode(0o700).create(path)?;
    Ok(())
}

pub(super) fn private_file(path: &Path) -> Result<File> {
    Ok(File::from(open(
        path,
        OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?))
}

pub(super) fn replace(directory: &Path, name: &str, bytes: &[u8]) -> Result<()> {
    ensure!(
        matches!(name, "ownership.json" | "calibration.json"),
        "invalid journal name"
    );
    let directory = open(
        directory,
        OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_RDONLY | OFlag::O_CLOEXEC,
        Mode::empty(),
    )?;
    let temporary = format!("{name}.{}.tmp", uuid::Uuid::new_v4().simple());
    let mut file = File::from(openat(
        &directory,
        temporary.as_str(),
        OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_NOFOLLOW | OFlag::O_WRONLY | OFlag::O_CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )?);
    file.write_all(bytes)?;
    file.sync_all()?;
    nix::unistd::close(file)?;
    renameat(&directory, temporary.as_str(), &directory, name)?;
    let directory = File::from(directory);
    directory.sync_all()?;
    nix::unistd::close(directory)?;
    Ok(())
}
