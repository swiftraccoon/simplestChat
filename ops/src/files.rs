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
impl Read for CheckedStream<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        read_scoped(self.file, self.before, bytes, &mut |_, value| value)
    }
}
impl Seek for CheckedStream<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        seek_scoped(self.file, self.before, position, &mut |_, value| value)
    }
}

/// Fixed source observations supplied to a lexical release checkpoint. They
/// carry no file authority; the Snapshot retains every admitted descriptor.
#[derive(Clone, Copy)]
pub(crate) enum SnapshotEvent {
    Check,
    Open,
    Metadata,
    Read,
    Seek,
    Close,
}
fn stream_checkpoint(
    event: SnapshotEvent,
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> io::Result<()> {
    checkpoint(event, Ok(())).map_err(|error| io::Error::other(error.code()))
}
fn stream_stamp(
    file: &File,
    before: &Metadata,
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> io::Result<()> {
    stream_checkpoint(SnapshotEvent::Metadata, checkpoint)?;
    let observed = match file.metadata() {
        Ok(value) => value,
        Err(first) => {
            let _ = checkpoint(SnapshotEvent::Metadata, Err(Error::Input));
            return Err(first);
        }
    };
    if !same(before, &observed) {
        let _ = checkpoint(SnapshotEvent::Check, Err(Error::Changed));
        return Err(io::Error::other("input_changed"));
    }
    stream_checkpoint(SnapshotEvent::Metadata, checkpoint)?;
    Ok(())
}
fn read_scoped(
    file: &mut File,
    before: &Metadata,
    bytes: &mut [u8],
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> io::Result<usize> {
    stream_stamp(file, before, checkpoint)?;
    stream_checkpoint(SnapshotEvent::Read, checkpoint)?;
    let count = match file.read(bytes) {
        Ok(value) => value,
        Err(first) if first.kind() == io::ErrorKind::Interrupted => return Err(first),
        Err(first) => {
            let _ = checkpoint(SnapshotEvent::Read, Err(Error::Input));
            return Err(first);
        }
    };
    stream_checkpoint(SnapshotEvent::Read, checkpoint)?;
    stream_stamp(file, before, checkpoint)?;
    Ok(count)
}
fn seek_scoped(
    file: &mut File,
    before: &Metadata,
    position: SeekFrom,
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> io::Result<u64> {
    stream_stamp(file, before, checkpoint)?;
    stream_checkpoint(SnapshotEvent::Seek, checkpoint)?;
    let offset = match file.seek(position) {
        Ok(value) => value,
        Err(first) if first.kind() == io::ErrorKind::Interrupted => return Err(first),
        Err(first) => {
            let _ = checkpoint(SnapshotEvent::Seek, Err(Error::Input));
            return Err(first);
        }
    };
    stream_checkpoint(SnapshotEvent::Seek, checkpoint)?;
    stream_stamp(file, before, checkpoint)?;
    Ok(offset)
}
fn note<T>(
    event: SnapshotEvent,
    result: Result<T, Error>,
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> Result<T, Error> {
    match result {
        Ok(value) => {
            checkpoint(event, Ok(()))?;
            Ok(value)
        }
        Err(first) => {
            let _ = checkpoint(event, Err(first));
            Err(first)
        }
    }
}
fn checked_close(
    file: File,
    close: &mut impl FnMut(File) -> Result<(), Error>,
    checkpoint: &mut impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
) -> Result<(), Error> {
    let before = checkpoint(SnapshotEvent::Close, Ok(()));
    // A refused checkpoint never skips an already admitted original close.
    let closed = close(file);
    let observed = note(SnapshotEvent::Close, closed, checkpoint);
    before.and(observed)
}
impl Snapshot {
    pub(crate) fn open(path: &Path, maximum: u64) -> Result<Self, Error> {
        Self::open_with(path, maximum, |file| {
            nix::unistd::close(file).map_err(|_| Error::Input)
        })
    }
    fn open_with(
        path: &Path,
        maximum: u64,
        close: impl FnMut(File) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        Self::open_scoped(path, maximum, close, |_, value| value)
    }
    pub(crate) fn open_checked(
        path: &Path,
        maximum: u64,
        checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        Self::open_scoped(
            path,
            maximum,
            |file| nix::unistd::close(file).map_err(|_| Error::Input),
            checkpoint,
        )
    }
    fn open_scoped(
        path: &Path,
        maximum: u64,
        mut close: impl FnMut(File) -> Result<(), Error>,
        mut checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<Self, Error> {
        checkpoint(SnapshotEvent::Check, Ok(()))?;
        let mut names: Vec<OsString> = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(name) => names.push(name.to_owned()),
                Component::RootDir | Component::CurDir => {}
                _ => return note(SnapshotEvent::Check, Err(Error::Input), &mut checkpoint),
            }
        }
        if names.is_empty() || names.len() > 256 {
            return note(SnapshotEvent::Check, Err(Error::Input), &mut checkpoint);
        }
        let root = if path.is_absolute() {
            Path::new("/")
        } else {
            Path::new(".")
        };
        checkpoint(SnapshotEvent::Open, Ok(()))?;
        let root = match open(root, FLAGS | OFlag::O_DIRECTORY, Mode::empty()) {
            Ok(root) => File::from(root),
            Err(_) => return note(SnapshotEvent::Open, Err(Error::Input), &mut checkpoint),
        };
        // Register each acquired file before the next fallible checkpoint.
        let mut directories = vec![root];
        let admitted = (|| {
            checkpoint(SnapshotEvent::Open, Ok(()))?;
            for name in &names[..names.len() - 1] {
                checkpoint(SnapshotEvent::Open, Ok(()))?;
                let next = match openat(
                    directories.last().unwrap(),
                    name.as_os_str(),
                    FLAGS | OFlag::O_DIRECTORY,
                    Mode::empty(),
                ) {
                    Ok(next) => next,
                    Err(_) => return note(SnapshotEvent::Open, Err(Error::Input), &mut checkpoint),
                };
                directories.push(File::from(next));
                checkpoint(SnapshotEvent::Open, Ok(()))?;
            }
            checkpoint(SnapshotEvent::Open, Ok(()))?;
            let file = match openat(
                directories.last().unwrap(),
                names.last().unwrap().as_os_str(),
                FLAGS,
                Mode::empty(),
            ) {
                Ok(file) => File::from(file),
                Err(_) => return note(SnapshotEvent::Open, Err(Error::Input), &mut checkpoint),
            };
            let stamp = (|| {
                checkpoint(SnapshotEvent::Open, Ok(()))?;
                checkpoint(SnapshotEvent::Metadata, Ok(()))?;
                let before = note(
                    SnapshotEvent::Metadata,
                    file.metadata().map_err(|_| Error::Input),
                    &mut checkpoint,
                )?;
                if !before.is_file() || before.len() == 0 || before.len() > maximum {
                    return note(SnapshotEvent::Check, Err(Error::Input), &mut checkpoint);
                }
                Ok(before)
            })();
            match stamp {
                Ok(before) => Ok((file, before)),
                Err(first) => {
                    let _ = checked_close(file, &mut close, &mut checkpoint);
                    Err(first)
                }
            }
        })();
        match admitted {
            Ok((file, before)) => Ok(Self {
                file,
                directories,
                names,
                before,
            }),
            Err(first) => {
                for directory in directories.into_iter().rev() {
                    let _ = checked_close(directory, &mut close, &mut checkpoint);
                }
                Err(first)
            }
        }
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
    pub(crate) fn read_checked(
        &mut self,
        bytes: &mut [u8],
        mut checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> io::Result<usize> {
        read_scoped(&mut self.file, &self.before, bytes, &mut checkpoint)
    }
    pub(crate) fn seek_checked(
        &mut self,
        position: SeekFrom,
        mut checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> io::Result<u64> {
        seek_scoped(&mut self.file, &self.before, position, &mut checkpoint)
    }
    pub(crate) fn verify(&self) -> Result<(), Error> {
        self.verify_using(
            |file| file.metadata().map_err(|_| Error::Changed),
            |file| nix::unistd::close(file).map_err(|_| Error::Input),
        )
    }
    fn verify_using(
        &self,
        metadata: impl FnMut(&File) -> Result<Metadata, Error>,
        close: impl FnMut(File) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.verify_scoped(metadata, close, |_, value| value)
    }
    fn verify_scoped(
        &self,
        mut metadata: impl FnMut(&File) -> Result<Metadata, Error>,
        mut close: impl FnMut(File) -> Result<(), Error>,
        mut checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        checkpoint(SnapshotEvent::Metadata, Ok(()))?;
        let retained = note(
            SnapshotEvent::Metadata,
            self.file.metadata().map_err(|_| Error::Input),
            &mut checkpoint,
        )?;
        if !same(&self.before, &retained) {
            return note(SnapshotEvent::Check, Err(Error::Changed), &mut checkpoint);
        }
        for (index, name) in self.names.iter().enumerate() {
            checkpoint(SnapshotEvent::Open, Ok(()))?;
            let visible = match openat(
                &self.directories[index],
                name.as_os_str(),
                FLAGS,
                Mode::empty(),
            ) {
                Ok(visible) => File::from(visible),
                Err(_) => return note(SnapshotEvent::Open, Err(Error::Changed), &mut checkpoint),
            };
            let observed = (|| {
                checkpoint(SnapshotEvent::Open, Ok(()))?;
                checkpoint(SnapshotEvent::Metadata, Ok(()))?;
                let observed = note(SnapshotEvent::Metadata, metadata(&visible), &mut checkpoint)?;
                if index + 1 == self.names.len() {
                    Ok(observed.is_file() && same(&self.before, &observed))
                } else {
                    checkpoint(SnapshotEvent::Metadata, Ok(()))?;
                    let retained = note(
                        SnapshotEvent::Metadata,
                        metadata(&self.directories[index + 1]),
                        &mut checkpoint,
                    )?;
                    Ok(observed.is_dir()
                        && retained.dev() == observed.dev()
                        && retained.ino() == observed.ino()
                        && retained.mode() == observed.mode())
                }
            })();
            // Keep the standalone result/close order, while the direct owner
            // learns an already observed mismatch before a later close fault.
            if matches!(observed, Ok(false)) {
                let _ = checkpoint(SnapshotEvent::Check, Err(Error::Changed));
            }
            let closed = checked_close(visible, &mut close, &mut checkpoint);
            let unchanged = observed?;
            closed?;
            if !unchanged {
                return Err(Error::Changed);
            }
        }
        Ok(())
    }
    /// Binding checks and every admitted close remain part of completion.
    pub(crate) fn finish(self) -> Result<(), Error> {
        self.finish_with(|file| nix::unistd::close(file).map_err(|_| Error::Input))
    }
    fn finish_with(self, close: impl FnMut(File) -> Result<(), Error>) -> Result<(), Error> {
        self.finish_scoped(close, |_, value| value)
    }
    pub(crate) fn finish_checked(
        self,
        checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.finish_scoped(
            |file| nix::unistd::close(file).map_err(|_| Error::Input),
            checkpoint,
        )
    }
    #[cfg(test)]
    pub(crate) fn finish_checked_fault(
        self,
        close: impl FnMut(File) -> Result<(), Error>,
        checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        self.finish_scoped(close, checkpoint)
    }
    fn finish_scoped(
        self,
        mut close: impl FnMut(File) -> Result<(), Error>,
        mut checkpoint: impl FnMut(SnapshotEvent, Result<(), Error>) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let verified = self.verify_scoped(
            |file| file.metadata().map_err(|_| Error::Changed),
            |file| nix::unistd::close(file).map_err(|_| Error::Input),
            &mut checkpoint,
        );
        let mut first = verified.err();
        if let Err(error) = checked_close(self.file, &mut close, &mut checkpoint) {
            first.get_or_insert(error);
        }
        for directory in self.directories.into_iter().rev() {
            if let Err(error) = checked_close(directory, &mut close, &mut checkpoint) {
                first.get_or_insert(error);
            }
        }
        first.map_or(Ok(()), Err)
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

#[cfg(test)]
#[path = "files_scope_tests.rs"]
mod scope_tests;
