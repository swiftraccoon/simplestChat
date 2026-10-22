//! Bounded regular-file copy; every selected source is checked before close.

use super::{
    files::{close, io, same},
    plan::failure,
};
use crate::tooling::Result;
use rustix::fs::{Dir, Mode, OFlags};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

const FILE_LIMIT: u64 = 16 * 1024 * 1024;
const TOTAL_LIMIT: u64 = 64 * 1024 * 1024;
pub(super) const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
pub(super) fn directory(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    rustix::fs::openat(parent, name, DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(|_| failure(1))
}
#[cfg(test)]
type AfterFile = Box<dyn FnMut(&str)>;

#[derive(Default)]
pub(super) struct Snapshot {
    pub files: BTreeMap<String, Vec<u8>>,
    stamps: BTreeMap<String, std::fs::Metadata>,
    #[cfg(test)]
    pub after_file: Option<AfterFile>,
    entries: usize,
    bytes: u64,
}
impl Snapshot {
    pub fn copy(&mut self, source: &File, target: &File, prefix: &str, depth: usize) -> Result<()> {
        if depth > 32 {
            return Err(failure(1));
        }
        let initial = io(source.metadata())?;
        self.stamps.insert(prefix.to_owned(), initial.clone());
        let entries = Dir::read_from(source).map_err(|_| failure(1))?;
        for entry in entries {
            let entry = entry.map_err(|_| failure(1))?;
            let name = entry.file_name();
            if [b".".as_slice(), b".."].contains(&name.to_bytes()) {
                continue;
            }
            self.entries += 1;
            if self.entries > 4096 {
                return Err(failure(1));
            }
            let component = name.to_str().map_err(|_| failure(1))?;
            let relative = format!("{prefix}{component}");
            if relative.len() > 4096 {
                return Err(failure(1));
            }
            let fd = rustix::fs::openat(
                source,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            let admitted = (|| {
                let before = io(fd.metadata())?;
                if before.dev() != initial.dev() {
                    return Err(failure(1));
                }
                if before.is_dir() {
                    if relative.starts_with("test/src/") && relative.ends_with(".cpp") {
                        return Err(failure(1));
                    }
                    rustix::fs::mkdirat(target, name, Mode::from_bits_truncate(0o700))
                        .map_err(|_| failure(1))?;
                    let child = directory(target, name)?;
                    let copied = self.copy(&fd, &child, &(relative.clone() + "/"), depth + 1);
                    let closed = close(child);
                    copied?;
                    closed?;
                } else if before.is_file() {
                    self.bytes = self
                        .bytes
                        .checked_add(before.len())
                        .ok_or_else(|| failure(1))?;
                    if before.len() > FILE_LIMIT || self.bytes > TOTAL_LIMIT {
                        return Err(failure(1));
                    }
                    let mut bytes = Vec::new();
                    (&fd)
                        .take(FILE_LIMIT + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| failure(1))?;
                    if bytes.len() as u64 != before.len() {
                        return Err(failure(1));
                    }
                    let output = rustix::fs::openat(
                        target,
                        name,
                        OFlags::WRONLY
                            | OFlags::CREATE
                            | OFlags::EXCL
                            | OFlags::NOFOLLOW
                            | OFlags::CLOEXEC,
                        Mode::from_bits_truncate(if before.mode() & 0o100 != 0 {
                            0o700
                        } else {
                            0o600
                        }),
                    )
                    .map(File::from)
                    .map_err(|_| failure(1))?;
                    let written = io((&output).write_all(&bytes).and_then(|()| output.sync_all()));
                    let closed = close(output);
                    written?;
                    closed?;
                    self.stamps.insert(relative.clone(), before.clone());
                    self.files.insert(relative.clone(), bytes);
                } else {
                    return Err(failure(1));
                }
                let visible = rustix::fs::openat(
                    source,
                    name,
                    OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                )
                .map(File::from)
                .map_err(|_| failure(1))?;
                let observed = io(visible.metadata());
                let closed = close(visible);
                let retained = io(fd.metadata());
                let unchanged = observed.is_ok_and(|value| same(&value, &before))
                    && retained.as_ref().is_ok_and(|value| same(value, &before));
                closed?;
                if !unchanged {
                    return Err(failure(1));
                }
                Ok(())
            })();
            let closed = close(fd);
            admitted?;
            closed?;
            #[cfg(test)]
            if self.files.contains_key(&relative)
                && let Some(after) = &mut self.after_file
            {
                after(&relative);
            }
        }
        if !same(&initial, &io(source.metadata())?) {
            return Err(failure(1));
        }
        Ok(())
    }

    /// Optimistic final membership/stamp validation; not an atomic snapshot or
    /// protection against a concurrent same-UID writer after this check.
    pub fn verify(&self, source: &File) -> Result<()> {
        let mut count = 0;
        self.verify_directory(source, "", 0, &mut count)?;
        if count + 1 != self.stamps.len() {
            return Err(failure(1));
        }
        Ok(())
    }
    fn verify_directory(
        &self,
        source: &File,
        prefix: &str,
        depth: usize,
        count: &mut usize,
    ) -> Result<()> {
        if depth > 32 {
            return Err(failure(1));
        }
        let before = io(source.metadata())?;
        if !self
            .stamps
            .get(prefix)
            .is_some_and(|value| same(value, &before))
        {
            return Err(failure(1));
        }
        for entry in Dir::read_from(source).map_err(|_| failure(1))? {
            let entry = entry.map_err(|_| failure(1))?;
            let name = entry.file_name();
            if [b".".as_slice(), b".."].contains(&name.to_bytes()) {
                continue;
            }
            *count += 1;
            if *count > 4096 {
                return Err(failure(1));
            }
            let relative = format!("{prefix}{}", name.to_str().map_err(|_| failure(1))?);
            let fd = rustix::fs::openat(
                source,
                name,
                OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            let checked = (|| {
                let observed = io(fd.metadata())?;
                if observed.is_dir() {
                    self.verify_directory(&fd, &(relative + "/"), depth + 1, count)
                } else if observed.is_file()
                    && self
                        .stamps
                        .get(&relative)
                        .is_some_and(|value| same(value, &observed))
                {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            })();
            let closed = close(fd);
            checked?;
            closed?;
        }
        if !same(&before, &io(source.metadata())?) {
            return Err(failure(1));
        }
        Ok(())
    }
}
