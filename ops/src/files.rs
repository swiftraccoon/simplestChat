//! Bounded reads anchored to retained directory descriptors. The final snapshot
//! detects replacement and in-place modification, not a lock against later writes.

use crate::Error;
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom};
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path};

const FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);

fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.len() == b.len()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

/// One regular file and every directory used to reach it remain owned until
/// `finish`. The immutable initial length bounds streaming and seeking callers;
/// `verify` checks the open inode and visible bindings without following links.
pub(crate) struct Snapshot {
    file: File,
    directories: Vec<File>,
    names: Vec<OsString>,
    before: Metadata,
}

/// Do not hand mutable-file headers to an allocating parser after preflight.
/// The parser receives bytes only when the retained stamp still matches on
/// both sides of the read; seeks are checked too. Names remain bound by verify.
pub(crate) struct CheckedStream<'a> {
    file: &'a mut File,
    before: &'a Metadata,
}
impl CheckedStream<'_> {
    fn check(&self) -> io::Result<()> {
        if same(self.before, &self.file.metadata()?) {
            Ok(())
        } else {
            Err(io::Error::other("input_changed"))
        }
    }
}
impl Read for CheckedStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.check()?;
        let count = self.file.read(bytes)?;
        self.check()?;
        Ok(count)
    }
}
impl Seek for CheckedStream<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.check()?;
        let offset = self.file.seek(position)?;
        self.check()?;
        Ok(offset)
    }
}

impl Snapshot {
    pub(crate) fn open(path: &Path, maximum: u64) -> Result<Self, Error> {
        let mut names: Vec<OsString> = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => names.push(name.to_owned()),
                Component::RootDir | Component::CurDir => {}
                _ => return Err(Error::Input),
            }
        }
        if names.is_empty() || names.len() > 256 {
            return Err(Error::Input);
        }
        let root = if path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        };
        let mut directories = vec![File::from(
            open(root, FLAGS | OFlag::O_DIRECTORY, Mode::empty()).map_err(|_| Error::Input)?,
        )];
        for name in &names[..names.len() - 1] {
            let next = openat(
                directories.last().unwrap(),
                name.as_os_str(),
                FLAGS | OFlag::O_DIRECTORY,
                Mode::empty(),
            )
            .map_err(|_| Error::Input)?;
            directories.push(File::from(next));
        }
        let file = File::from(
            openat(
                directories.last().unwrap(),
                names.last().unwrap().as_os_str(),
                FLAGS,
                Mode::empty(),
            )
            .map_err(|_| Error::Input)?,
        );
        let before = file.metadata().map_err(|_| Error::Input)?;
        if !before.is_file() || before.len() == 0 || before.len() > maximum {
            return Err(Error::Input);
        }
        Ok(Self {
            file,
            directories,
            names,
            before,
        })
    }

    pub(crate) fn file_mut(&mut self) -> &mut File {
        &mut self.file
    }
    pub(crate) fn length(&self) -> u64 {
        self.before.len()
    }
    pub(crate) fn checked_stream(&mut self) -> CheckedStream<'_> {
        CheckedStream {
            file: &mut self.file,
            before: &self.before,
        }
    }

    pub(crate) fn verify(&self) -> Result<(), Error> {
        if !same(
            &self.before,
            &self.file.metadata().map_err(|_| Error::Input)?,
        ) {
            return Err(Error::Changed);
        }
        for (index, name) in self.names.iter().enumerate() {
            let visible = File::from(
                openat(
                    &self.directories[index],
                    name.as_os_str(),
                    FLAGS,
                    Mode::empty(),
                )
                .map_err(|_| Error::Changed)?,
            );
            let metadata = visible.metadata().map_err(|_| Error::Changed)?;
            let unchanged = if index + 1 == self.names.len() {
                metadata.is_file() && same(&self.before, &metadata)
            } else {
                let retained = self.directories[index + 1]
                    .metadata()
                    .map_err(|_| Error::Changed)?;
                metadata.is_dir()
                    && retained.dev() == metadata.dev()
                    && retained.ino() == metadata.ino()
                    && retained.mode() == metadata.mode()
            };
            nix::unistd::close(visible).map_err(|_| Error::Input)?;
            if !unchanged {
                return Err(Error::Changed);
            }
        }
        Ok(())
    }

    /// Success includes binding checks and explicit closes. Even a close failure
    /// does not skip closing the other owned descriptors.
    pub(crate) fn finish(self) -> Result<(), Error> {
        self.finish_with(|file| nix::unistd::close(file).map_err(|_| Error::Input))
    }

    fn finish_with(self, mut close: impl FnMut(File) -> Result<(), Error>) -> Result<(), Error> {
        self.verify()?;
        let mut failed = close(self.file).is_err();
        for directory in self.directories.into_iter().rev() {
            failed |= close(directory).is_err();
        }
        if failed { Err(Error::Input) } else { Ok(()) }
    }
}

pub(crate) fn read(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    read_after(path, maximum, || {})
}

fn read_after(path: &Path, maximum: usize, after: impl FnOnce()) -> Result<Vec<u8>, Error> {
    let mut snapshot = Snapshot::open(path, maximum as u64)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(snapshot.length() as usize)
        .map_err(|_| Error::Input)?;
    let limit = (maximum as u64).checked_add(1).ok_or(Error::Input)?;
    snapshot
        .file_mut()
        .take(limit)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Input)?;
    after();
    if bytes.len() as u64 != snapshot.length() {
        return Err(Error::Changed);
    }
    snapshot.finish()?;
    Ok(bytes)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
