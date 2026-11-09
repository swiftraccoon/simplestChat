//! Read-only, retained identity snapshots of selected executables.
//!
//! Resolve ordinary symlinks (such as setup-node's) once, then execute the
//! canonical path. Rechecks detect replacement or mutation; they are not FD-exec
//! or a lock against a same-UID writer. No received path appears in diagnostics.

use crate::tooling::{Environment, Failure, Result};
use rustix::fs::{Access, Mode, OFlags};
use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

/// The longest path, in bytes, an identity admits.
pub const MAX_PATH: usize = 4096;

fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}
/// The refusal for a path or identity that fails admission or a recheck.
pub fn invalid() -> Failure {
    failure("Release-check executable identity invalid.")
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure("Release-check executable close failed."))
}
fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
fn open(path: &Path) -> Result<File> {
    rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| invalid())
}

/// A regular executable with a bounded path and explicit descriptor closes.
#[derive(Debug)]
pub struct Executable {
    selected: PathBuf,
    canonical: PathBuf,
    file: File,
    metadata: Metadata,
}
impl Executable {
    /// Retain an absolute path to a nonempty regular file this user may
    /// execute; refuses anything else and closes what it opened.
    pub fn open(path: &Path) -> Result<Self> {
        if !path.is_absolute()
            || path.as_os_str().as_encoded_bytes().len() > MAX_PATH
            || path.as_os_str().as_encoded_bytes().contains(&0)
        {
            return Err(invalid());
        }
        let canonical = path.canonicalize().map_err(|_| invalid())?;
        let file = open(&canonical)?;
        let admission = (|| {
            let metadata = file.metadata().map_err(|_| invalid())?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.mode() & 0o111 == 0
                || rustix::fs::access(path, Access::EXEC_OK).is_err()
            {
                return Err(invalid());
            }
            Ok(metadata)
        })();
        let metadata = match admission {
            Ok(value) => value,
            Err(error) => {
                close(file)?;
                return Err(error);
            }
        };
        let selected = Self {
            selected: path.into(),
            canonical,
            file,
            metadata,
        };
        if let Err(error) = selected.validate() {
            selected.close()?;
            return Err(error);
        }
        Ok(selected)
    }

    /// The canonical path to execute, after checking that the selected path
    /// still resolves to it and that both it and the retained file are unchanged.
    pub fn validate(&self) -> Result<&Path> {
        if self.selected.canonicalize().map_err(|_| invalid())? != self.canonical {
            return Err(invalid());
        }
        let visible = open(&self.canonical)?;
        let observed = visible.metadata().map_err(|_| invalid());
        let closed = close(visible);
        let retained = self.file.metadata().map_err(|_| invalid());
        if !same(&observed?, &self.metadata) || !same(&retained?, &self.metadata) {
            return Err(invalid());
        }
        closed?;
        Ok(&self.canonical)
    }

    /// Close the retained descriptor; a failed close is an error.
    pub fn close(self) -> Result<()> {
        close(self.file)
    }

    /// Preserve invocation-path semantics (for example Python virtualenvs)
    /// after validating the same retained canonical target and selected binding.
    pub fn selected_path(&self) -> Result<&Path> {
        self.validate()?;
        Ok(&self.selected)
    }
}

/// Select the first executable exactly as PATH lookup does; an explicit path
/// is resolved relative to the captured caller directory. No shell evaluation.
pub fn select(root: &Path, environment: &Environment, program: &OsStr) -> Result<Executable> {
    if program.as_encoded_bytes().contains(&b'/') {
        return Executable::open(&root.join(program));
    }
    let path = environment
        .get(OsStr::new("PATH"))
        .ok_or_else(|| Failure::new(127, "Release-check Node unavailable."))?;
    if path.as_encoded_bytes().len() > 128 * 1024 {
        return Err(invalid());
    }
    for directory in std::env::split_paths(path) {
        let candidate = root.join(directory).join(program);
        if candidate.is_file() && rustix::fs::access(&candidate, Access::EXEC_OK).is_ok() {
            return Executable::open(&candidate);
        }
    }
    Err(Failure::new(127, "Release-check Node unavailable."))
}
