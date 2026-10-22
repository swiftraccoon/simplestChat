//! Fixed private local-CI evidence projections, never recursive artifact copies.
//!
//! Roots come from the owned runner environment; the command accepts no paths.
//! An absent, non-directory or symlink source is optional and does not require
//! an evidence root. An admitted source gets a new private destination, even
//! when its optional files are absent. Existing output is never overwritten.

use crate::tooling::Environment;
use anyhow::{Result, ensure};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStringExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci copy-evidence \
image-database|release-container|release-checks";
const MAX_ENTRIES: usize = 4096;
const MAX_FILES: usize = 1024;
const MAX_FILE: u64 = 16 * 1024 * 1024;
const MAX_TOTAL: u64 = 256 * 1024 * 1024;
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

#[derive(Clone, Copy, Debug)]
enum Kind {
    ImageDatabase,
    ReleaseContainer,
    ReleaseChecks,
}

impl Kind {
    fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "image-database" => Self::ImageDatabase,
            "release-container" => Self::ReleaseContainer,
            "release-checks" => Self::ReleaseChecks,
            _ => return None,
        })
    }
    fn source(self) -> &'static str {
        match self {
            Self::ImageDatabase => "image-database-preparation",
            Self::ReleaseContainer => "release-container",
            Self::ReleaseChecks => "release-checks",
        }
    }
    fn destination(self) -> &'static str {
        match self {
            Self::ImageDatabase => "image-database",
            _ => self.source(),
        }
    }
}

fn root(environment: &Environment, name: &str) -> Result<File> {
    let path = environment
        .get(OsStr::new(name))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| anyhow::anyhow!("evidence_root"))?;
    Ok(File::from(rustix::fs::open(
        path,
        DIRECTORY,
        Mode::empty(),
    )?))
}

fn optional_directory(parent: &File, name: &str) -> Result<Option<File>> {
    match rustix::fs::openat(parent, name, DIRECTORY, Mode::empty()) {
        Ok(fd) => Ok(Some(File::from(fd))),
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
            Ok(None)
        }
        Err(error) => Err(error.into()),
    }
}

fn new_directory(parent: &File, name: &str) -> Result<File> {
    rustix::fs::mkdirat(parent, name, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    let file = File::from(rustix::fs::openat(parent, name, DIRECTORY, Mode::empty())?);
    rustix::fs::fchmod(&file, Mode::RUSR | Mode::WUSR | Mode::XUSR)?;
    Ok(file)
}

fn private_directory_is_at(
    parent: impl std::os::fd::AsFd,
    name: &OsStr,
    directory: &File,
) -> Result<()> {
    let current = rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)?;
    let owned = rustix::fs::fstat(directory)?;
    ensure!(
        FileType::from_raw_mode(current.st_mode) == FileType::Directory
            && current.st_dev == owned.st_dev
            && current.st_ino == owned.st_ino
            && current.st_mode & 0o077 == 0,
        "evidence_destination_changed"
    );
    Ok(())
}

fn names(directory: &File, prefix: &[u8], suffix: &[u8]) -> Result<Vec<OsString>> {
    let mut names = Vec::new();
    let mut count = 0;
    for entry in Dir::read_from(directory)? {
        let entry = entry?;
        let name = entry.file_name().to_bytes();
        if [b".".as_slice(), b".."].contains(&name) {
            continue;
        }
        count += 1;
        ensure!(count <= MAX_ENTRIES, "evidence_directory_limit");
        if name.starts_with(prefix) && name.ends_with(suffix) {
            names.push(OsString::from_vec(name.to_vec()));
        }
    }
    names.sort();
    Ok(names)
}

#[derive(Default)]
struct Budget {
    files: usize,
    bytes: u64,
}

impl Budget {
    fn admit(&mut self, length: u64) -> Result<()> {
        ensure!(length <= MAX_FILE, "evidence_file_limit");
        self.files += 1;
        self.bytes += length;
        ensure!(
            self.files <= MAX_FILES && self.bytes <= MAX_TOTAL,
            "evidence_copy_limit"
        );
        Ok(())
    }
}

/// Admission and reads use the source descriptor; publication is exclusive and
/// relative to the destination descriptor. Raced links/special files cannot
/// redirect a read or an output, and changed-size/mtime sources fail closed.
fn copy(source: &File, destination: &File, name: &OsStr, budget: &mut Budget) -> Result<()> {
    let metadata = match rustix::fs::statat(source, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(metadata) => metadata,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    if FileType::from_raw_mode(metadata.st_mode) != FileType::RegularFile {
        return Ok(());
    }
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let descriptor = match rustix::fs::openat(source, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(rustix::io::Errno::NOENT | rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP) => {
            return Ok(());
        }
        Err(error) => return Err(error.into()),
    };
    let mut input = File::from(descriptor);
    let before = input.metadata()?;
    if !before.is_file() {
        return Ok(());
    }
    budget.admit(before.len())?;
    let mut bytes = Vec::new();
    std::io::Read::by_ref(&mut input)
        .take(before.len() + 1)
        .read_to_end(&mut bytes)?;
    let after = input.metadata()?;
    ensure!(
        bytes.len() as u64 == before.len()
            && after.len() == before.len()
            && (before.mtime(), before.mtime_nsec()) == (after.mtime(), after.mtime_nsec()),
        "evidence_source_changed"
    );
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut output = File::from(rustix::fs::openat(
        destination,
        name,
        flags,
        Mode::RUSR | Mode::WUSR,
    )?);
    rustix::fs::fchmod(&output, Mode::RUSR | Mode::WUSR)?;
    output.write_all(&bytes)?;
    let output_closed = nix::unistd::close(output);
    let input_closed = nix::unistd::close(input);
    output_closed?;
    input_closed?;
    Ok(())
}

fn project(kind: Kind, environment: &Environment) -> Result<()> {
    let runner = root(environment, "RUNNER_TEMP")?;
    let Some(source) = optional_directory(&runner, kind.source())? else {
        return Ok(());
    };
    let evidence = root(environment, "LOCAL_CI_EVIDENCE")?;
    ensure!(
        evidence.metadata()?.mode() & 0o077 == 0,
        "evidence_root_private"
    );
    let destination = new_directory(&evidence, kind.destination())?;
    let mut budget = Budget::default();
    let fixed: &[&str] = match kind {
        Kind::ImageDatabase => &["database-preparation.json", "database-status.json"],
        Kind::ReleaseContainer => &["report.json"],
        Kind::ReleaseChecks => &[
            "summary.json",
            "release-fixture.stdout",
            "release-fixture.stderr",
            "scanner-tools.stdout",
            "scanner-tools.stderr",
        ],
    };
    let private = if matches!(kind, Kind::ReleaseContainer) {
        Some(new_directory(&destination, "private")?)
    } else {
        None
    };
    for name in fixed {
        copy(&source, &destination, OsStr::new(name), &mut budget)?;
    }
    if matches!(kind, Kind::ImageDatabase) {
        for name in names(&source, b"scanner-result-", b".json")? {
            copy(&source, &destination, &name, &mut budget)?;
        }
    }
    if let Some(destination) = &private
        && let Some(source) = optional_directory(&source, "private")?
    {
        for suffix in [b".json".as_slice(), b".stdout", b".stderr"] {
            for name in names(&source, b"command-", suffix)? {
                copy(&source, destination, &name, &mut budget)?;
            }
        }
    }
    if let Some(private) = &private {
        private_directory_is_at(&destination, OsStr::new("private"), private)?;
    }
    private_directory_is_at(&evidence, OsStr::new(kind.destination()), &destination)?;
    private_directory_is_at(
        rustix::fs::CWD,
        environment
            .get(OsStr::new("LOCAL_CI_EVIDENCE"))
            .expect("validated evidence root"),
        &evidence,
    )?;
    Ok(())
}

pub(super) fn run(arguments: &[String]) -> Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(kind) = arguments
        .first()
        .filter(|_| arguments.len() == 1)
        .and_then(|value| Kind::parse(value))
    else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    match project(kind, &crate::tooling::inherited_env()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(_) => {
            eprintln!("Local CI evidence could not be preserved.");
            Ok(ExitCode::FAILURE)
        }
    }
}

#[cfg(test)]
#[path = "evidence_copy_tests.rs"]
mod tests;
