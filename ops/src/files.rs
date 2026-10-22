//! Bounded reads anchored to retained directory descriptors. The final snapshot
//! detects replacement and in-place modification, not a lock against later writes.

use crate::Error;
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use std::ffi::OsString;
use std::fs::{File, Metadata};
use std::io::Read;
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

pub(crate) fn read(path: &Path, maximum: usize) -> Result<Vec<u8>, Error> {
    read_after(path, maximum, || {})
}

fn read_after(path: &Path, maximum: usize, after: impl FnOnce()) -> Result<Vec<u8>, Error> {
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
    let mut file = File::from(
        openat(
            directories.last().unwrap(),
            names.last().unwrap().as_os_str(),
            FLAGS,
            Mode::empty(),
        )
        .map_err(|_| Error::Input)?,
    );
    let before = file.metadata().map_err(|_| Error::Input)?;
    if !before.is_file() || before.len() == 0 || before.len() > maximum as u64 {
        return Err(Error::Input);
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(before.len() as usize)
        .map_err(|_| Error::Input)?;
    Read::by_ref(&mut file)
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Input)?;
    after();
    if bytes.len() as u64 != before.len()
        || !same(&before, &file.metadata().map_err(|_| Error::Input)?)
    {
        return Err(Error::Changed);
    }
    for (index, name) in names.iter().enumerate() {
        let visible = File::from(
            openat(&directories[index], name.as_os_str(), FLAGS, Mode::empty())
                .map_err(|_| Error::Changed)?,
        );
        let metadata = visible.metadata().map_err(|_| Error::Changed)?;
        let unchanged = if index + 1 == names.len() {
            metadata.is_file() && same(&before, &metadata)
        } else {
            let retained = directories[index + 1]
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
    nix::unistd::close(file).map_err(|_| Error::Input)?;
    for directory in directories.into_iter().rev() {
        nix::unistd::close(directory).map_err(|_| Error::Input)?;
    }
    Ok(bytes)
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
