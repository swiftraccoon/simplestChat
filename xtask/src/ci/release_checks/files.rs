//! Read-only, retained identity snapshots for PATH Node and discovered Chromium.
//!
//! Resolve ordinary setup-node symlinks once, then execute the canonical Node
//! path. Rechecks detect replacement or mutation; they are not FD-exec or a lock
//! against a same-UID writer. No received path appears in diagnostics.

use crate::tooling::{Environment, Failure, Result};
use rustix::fs::{Access, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const MAX_PATH: usize = 4096;

pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}
fn invalid() -> Failure {
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
pub(in crate::ci) struct Executable {
    selected: PathBuf,
    canonical: PathBuf,
    file: File,
    metadata: Metadata,
}
impl Executable {
    pub(in crate::ci) fn open(path: &Path) -> Result<Self> {
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

    pub(in crate::ci) fn validate(&self) -> Result<&Path> {
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

    pub(in crate::ci) fn close(self) -> Result<()> {
        close(self.file)
    }

    /// Preserve invocation-path semantics (for example Python virtualenvs)
    /// after validating the same retained canonical target and selected binding.
    pub(in crate::ci) fn selected_path(&self) -> Result<&Path> {
        self.validate()?;
        Ok(&self.selected)
    }
}

/// Match the shell's first executable PATH candidate; no ambient PATH fallback.
pub(super) fn node(root: &Path, environment: &Environment) -> Result<Executable> {
    select(root, environment, std::ffi::OsStr::new("node"))
}

/// Select the first executable exactly as PATH lookup does; an explicit path
/// is resolved relative to the captured caller directory. No shell evaluation.
pub(in crate::ci) fn select(
    root: &Path,
    environment: &Environment,
    program: &std::ffi::OsStr,
) -> Result<Executable> {
    if program.as_encoded_bytes().contains(&b'/') {
        return Executable::open(&root.join(program));
    }
    let path = environment
        .get(std::ffi::OsStr::new("PATH"))
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

pub(super) fn chromium(bytes: &[u8]) -> Result<Executable> {
    if bytes.is_empty() || bytes.len() > MAX_PATH {
        return Err(invalid());
    }
    let path = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    if path.chars().any(char::is_control) {
        return Err(invalid());
    }
    Executable::open(Path::new(path))
}

#[cfg(test)]
mod resolver_tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn node_delegation_keeps_first_executable_path_and_ordinary_symlink_selection() {
        let root = std::env::temp_dir()
            .canonicalize()
            .unwrap()
            .join(format!("shared-node-resolution-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        let executable = second.join("actual node");
        fs::write(&executable, b"owned-inert").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(first.join("node"), b"not executable").unwrap();
        fs::set_permissions(first.join("node"), fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&executable, second.join("node")).unwrap();
        let environment = Environment::from([(
            "PATH".into(),
            std::env::join_paths([first, second]).unwrap(),
        )]);
        let selected = node(&root, &environment).unwrap();
        assert_eq!(selected.validate().unwrap(), executable);
        selected.close().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
