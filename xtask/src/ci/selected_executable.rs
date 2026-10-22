//! A scanner path confined to an owned private namespace and retained by FD.
//! This is an identity snapshot, not FD-exec or a lock against same-UID writers.

use crate::tooling::{Failure, Result};
use rustix::fs::{Mode, OFlags};
use std::{
    fs::{File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
fn fail() -> Failure {
    Failure::new(1, "Selected audit executable identity invalid.")
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| fail())
}
fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
}
fn stamped(left: &Metadata, right: &Metadata) -> bool {
    same(left, right)
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
fn open(parent: &File, name: &str, directory: bool) -> Result<File> {
    let flags = if directory {
        DIRECTORY
    } else {
        OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC
    };
    rustix::fs::openat(parent, name, flags, Mode::empty())
        .map(File::from)
        .map_err(|_| fail())
}
#[derive(Debug)]
struct Directory {
    file: File,
    metadata: Metadata,
}
#[derive(Debug)]
pub(super) struct Executable {
    path: PathBuf,
    root: PathBuf,
    directories: Vec<Directory>,
    file: File,
    metadata: Metadata,
}
impl Executable {
    pub(super) fn open(
        root: &Path,
        root_fd: File,
        returned: &[u8],
        closed: &mut bool,
    ) -> Result<Self> {
        *closed = false;
        let expected = root.join("tools/bin/cargo-audit");
        let mut directories = Vec::<Directory>::new();
        let mut selected = Some(root_fd);
        let admitted = (|| {
            let text = std::str::from_utf8(returned)
                .map_err(|_| fail())?
                .trim_end_matches('\n');
            if text.as_bytes() != expected.as_os_str().as_encoded_bytes()
                || text.contains(['\n', '\r', '\0'])
            {
                return Err(fail());
            }
            let metadata = selected.as_ref().unwrap().metadata().map_err(|_| fail())?;
            if !metadata.is_dir() || metadata.mode() & 0o077 != 0 {
                return Err(fail());
            }
            directories.push(Directory {
                file: selected.take().unwrap(),
                metadata,
            });
            for name in ["tools", "bin"] {
                selected = Some(open(&directories.last().unwrap().file, name, true)?);
                let metadata = selected.as_ref().unwrap().metadata().map_err(|_| fail())?;
                if !metadata.is_dir()
                    || metadata.uid() != directories[0].metadata.uid()
                    || metadata.mode() & 0o022 != 0
                {
                    return Err(fail());
                }
                directories.push(Directory {
                    file: selected.take().unwrap(),
                    metadata,
                });
            }
            selected = Some(open(
                &directories.last().unwrap().file,
                "cargo-audit",
                false,
            )?);
            let metadata = selected.as_ref().unwrap().metadata().map_err(|_| fail())?;
            if !metadata.is_file()
                || metadata.len() == 0
                || metadata.len() > 256 * 1024 * 1024
                || metadata.nlink() != 1
                || metadata.mode() & 0o111 == 0
                || metadata.mode() & 0o022 != 0
                || metadata.uid() != directories[0].metadata.uid()
            {
                return Err(fail());
            }
            Ok(metadata)
        })();
        let metadata = match admitted {
            Ok(metadata) => metadata,
            Err(error) => {
                let mut passed = true;
                if let Some(file) = selected {
                    passed &= close(file).is_ok();
                }
                for directory in directories.into_iter().rev() {
                    passed &= close(directory.file).is_ok();
                }
                *closed = passed;
                return Err(error);
            }
        };
        let executable = Self {
            path: expected,
            root: root.into(),
            directories,
            file: selected.unwrap(),
            metadata,
        };
        if let Err(error) = executable.validate() {
            *closed = executable.close().is_ok();
            return Err(error);
        }
        Ok(executable)
    }

    pub(super) fn validate(&self) -> Result<&Path> {
        let visible = rustix::fs::open(&self.root, DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| fail())?;
        let observed = visible.metadata().map_err(|_| fail());
        let closed = close(visible);
        if !same(&observed?, &self.directories[0].metadata) {
            return Err(fail());
        }
        closed?;
        for (index, name) in ["tools", "bin"].into_iter().enumerate() {
            let visible = open(&self.directories[index].file, name, true)?;
            let observed = visible.metadata().map_err(|_| fail());
            let closed = close(visible);
            if !same(&observed?, &self.directories[index + 1].metadata) {
                return Err(fail());
            }
            closed?;
        }
        let file = open(&self.directories.last().unwrap().file, "cargo-audit", false)?;
        let observed = file.metadata().map_err(|_| fail());
        let closed = close(file);
        if !stamped(&observed?, &self.metadata)
            || !stamped(&self.file.metadata().map_err(|_| fail())?, &self.metadata)
        {
            return Err(fail());
        }
        closed?;
        Ok(&self.path)
    }

    pub(super) fn close(self) -> Result<()> {
        let mut passed = close(self.file).is_ok();
        for directory in self.directories.into_iter().rev() {
            passed &= close(directory.file).is_ok();
        }
        if passed { Ok(()) } else { Err(fail()) }
    }
}
