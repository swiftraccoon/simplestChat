//! Replace only the runner's admitted mirror preference list through retained FDs.
//!
//! The replacement is atomic visibility, not a lock or compare-and-swap against
//! other root writers. A late failure leaves the known preferred list in place;
//! it never reports success or rolls back over a later writer.

use super::policy;
use crate::{ci::release_controller::files::Directory, tooling};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

const MIRRORS: &str = "apt-mirrors.txt";
const STAGING: &str = ".simplestchat-apt-mirrors.part";
const GUARD_MAX: u64 = 16 * 1024;
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

#[derive(Clone, Copy)]
struct Owner {
    uid: u32,
    gid: u32,
}

/// Fixed fault boundaries for this one transaction, with no runtime selectors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Point {
    Write,
    FileSync,
    Readback,
    BeforeRename,
    Rename,
    DirectorySync,
    Published,
    Close,
}

fn failure() -> tooling::Failure {
    tooling::Failure::new(1, "APT mirror file operation failed.")
}

fn io<T>(result: std::io::Result<T>) -> tooling::Result<T> {
    result.map_err(|_| failure())
}

fn close(file: File) -> tooling::Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}

fn checked_close(
    file: File,
    hook: &mut impl FnMut(Point) -> tooling::Result<()>,
) -> tooling::Result<()> {
    let closed = close(file);
    let observed = hook(Point::Close);
    closed.and(observed)
}

fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}

fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}

fn profile(metadata: &Metadata, owner: Owner, maximum: u64) -> tooling::Result<()> {
    if metadata.is_file()
        && metadata.uid() == owner.uid
        && metadata.gid() == owner.gid
        && metadata.mode() & 0o7777 == 0o644
        && metadata.nlink() == 1
        && metadata.len() > 0
        && metadata.len() <= maximum
    {
        Ok(())
    } else {
        Err(failure())
    }
}

fn directory_profile(directory: &Directory, owner: Owner) -> tooling::Result<()> {
    let metadata = io(directory.file().metadata())?;
    if metadata.is_dir()
        && metadata.uid() == owner.uid
        && metadata.gid() == owner.gid
        && metadata.mode() & 0o022 == 0
    {
        directory.verify().map_err(|_| failure())
    } else {
        Err(failure())
    }
}

fn directory(root: &Path, names: &[&str], owner: Owner) -> tooling::Result<Directory> {
    let mut value = Directory::open(root).map_err(|_| failure())?;
    for name in names {
        if let Err(error) = directory_profile(&value, owner) {
            let _ = value.finish();
            return Err(error);
        }
        // descend explicitly finishes its original owner on admission failure.
        value = value.descend(name).map_err(|_| failure())?;
    }
    if let Err(error) = directory_profile(&value, owner) {
        let _ = value.finish();
        return Err(error);
    }
    Ok(value)
}

fn open(directory: &Directory, name: &str) -> tooling::Result<File> {
    directory.verify().map_err(|_| failure())?;
    rustix::fs::openat(directory.file(), name, FLAGS, Mode::empty())
        .map(File::from)
        .map_err(|_| failure())
}

fn visible(directory: &Directory, name: &str, expected: &Metadata) -> tooling::Result<()> {
    let file = open(directory, name)?;
    let metadata = io(file.metadata());
    let closed = close(file);
    let metadata = metadata?;
    closed?;
    if same(expected, &metadata) {
        Ok(())
    } else {
        Err(failure())
    }
}

fn data(file: &mut File, maximum: u64) -> tooling::Result<Vec<u8>> {
    io(file.seek(SeekFrom::Start(0)))?;
    let mut bytes = Vec::new();
    io(Read::by_ref(file).take(maximum + 1).read_to_end(&mut bytes))?;
    if bytes.len() as u64 > maximum {
        return Err(failure());
    }
    Ok(bytes)
}

struct Input {
    directory: usize,
    name: &'static str,
    file: File,
    stamp: Metadata,
    bytes: Vec<u8>,
}

impl Input {
    fn open(
        directory: usize,
        parent: &Directory,
        name: &'static str,
        owner: Owner,
        maximum: u64,
    ) -> tooling::Result<Self> {
        let mut file = open(parent, name)?;
        let admitted = (|| {
            let stamp = io(file.metadata())?;
            profile(&stamp, owner, maximum)?;
            visible(parent, name, &stamp)?;
            let bytes = data(&mut file, maximum)?;
            if bytes.len() as u64 != stamp.len() || !same(&stamp, &io(file.metadata())?) {
                return Err(failure());
            }
            visible(parent, name, &stamp)?;
            Ok((stamp, bytes))
        })();
        match admitted {
            Ok((stamp, bytes)) => Ok(Self {
                directory,
                name,
                file,
                stamp,
                bytes,
            }),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }

    fn check(&mut self, directory: &Directory) -> tooling::Result<()> {
        visible(directory, self.name, &self.stamp)?;
        if !same(&self.stamp, &io(self.file.metadata())?)
            || data(&mut self.file, self.stamp.len())? != self.bytes
            || !same(&self.stamp, &io(self.file.metadata())?)
        {
            return Err(failure());
        }
        visible(directory, self.name, &self.stamp)
    }
}

struct Stage {
    file: Option<File>,
    stamp: Option<Metadata>,
    visible: Option<Metadata>,
    published: bool,
}

struct Owned {
    directories: Vec<Directory>,
    inputs: Vec<Input>,
    stage: Stage,
}

impl Owned {
    fn new() -> Self {
        Self {
            directories: Vec::new(),
            inputs: Vec::new(),
            stage: Stage {
                file: None,
                stamp: None,
                visible: None,
                published: false,
            },
        }
    }

    fn check(&mut self) -> tooling::Result<()> {
        for input in &mut self.inputs {
            input.check(&self.directories[input.directory])?;
        }
        Ok(())
    }

    fn prepare(&mut self, root: &Path, owner: Owner) -> tooling::Result<bool> {
        const CHAINS: &[&[&str]] = &[
            &["etc", "apt"],
            &["etc", "apt", "sources.list.d"],
            &["usr", "lib"],
        ];
        for names in CHAINS {
            self.directories.push(directory(root, names, owner)?);
        }
        for (index, name, maximum) in [
            (0, MIRRORS, policy::BEFORE.len() as u64),
            (1, "ubuntu.sources", GUARD_MAX),
            (2, "os-release", GUARD_MAX),
        ] {
            self.inputs.push(Input::open(
                index,
                &self.directories[index],
                name,
                owner,
                maximum,
            )?);
        }
        policy::sources(&self.inputs[1].bytes)?;
        policy::os_release(&self.inputs[2].bytes)?;
        policy::admit(&self.inputs[0].bytes)
    }

    fn replace(
        &mut self,
        owner: Owner,
        hook: &mut impl FnMut(Point) -> tooling::Result<()>,
    ) -> tooling::Result<()> {
        let parent = &self.directories[0];
        parent.verify().map_err(|_| failure())?;
        self.stage.file = Some(File::from(
            rustix::fs::openat(
                parent.file(),
                STAGING,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o644),
            )
            .map_err(|_| failure())?,
        ));
        let file = self.stage.file.as_mut().ok_or_else(failure)?;
        // Retain identity before any later metadata or write failure.
        self.stage.stamp = Some(io(file.metadata())?);
        rustix::fs::fchmod(&*file, Mode::from_bits_truncate(0o644)).map_err(|_| failure())?;
        hook(Point::Write)?;
        io(file.write_all(policy::AFTER))?;
        hook(Point::FileSync)?;
        io(file.sync_all())?;
        self.stage.stamp = Some(io(file.metadata())?);
        let stamp = self.stage.stamp.as_ref().ok_or_else(failure)?;
        profile(stamp, owner, policy::AFTER.len() as u64)?;
        hook(Point::Readback)?;
        if data(file, policy::AFTER.len() as u64)? != policy::AFTER
            || !same(stamp, &io(file.metadata())?)
        {
            return Err(failure());
        }
        visible(parent, STAGING, stamp)?;
        checked_close(self.stage.file.take().ok_or_else(failure)?, hook)?;
        hook(Point::BeforeRename)?;
        self.check()?;
        let parent = &self.directories[0];
        visible(
            parent,
            STAGING,
            self.stage.stamp.as_ref().ok_or_else(failure)?,
        )?;
        hook(Point::Rename)?;
        rustix::fs::renameat(parent.file(), STAGING, parent.file(), MIRRORS)
            .map_err(|_| failure())?;
        // Adopt the visible effect before any fallible observation or finalizer.
        self.stage.published = true;
        hook(Point::DirectorySync)?;
        io(parent.file().sync_all())?;
        let fresh = Input::open(0, parent, MIRRORS, owner, policy::AFTER.len() as u64)?;
        let staged = self.stage.stamp.as_ref().ok_or_else(failure)?;
        let valid = binding(staged, &fresh.stamp)
            && fresh.stamp.len() == staged.len()
            && fresh.stamp.mtime() == staged.mtime()
            && fresh.stamp.mtime_nsec() == staged.mtime_nsec()
            && fresh.bytes == policy::AFTER;
        if !valid {
            let _ = checked_close(fresh.file, hook);
            return Err(failure());
        }
        self.stage.visible = Some(fresh.stamp.clone());
        self.inputs.push(fresh);
        hook(Point::Published)
    }

    fn finish(
        mut self,
        hook: &mut impl FnMut(Point) -> tooling::Result<()>,
    ) -> tooling::Result<()> {
        let mut result = Ok(());
        if let Some(file) = self.stage.file.take() {
            result = result.and(checked_close(file, hook));
        }
        if !self.stage.published
            && let Some(stamp) = &self.stage.stamp
        {
            // A changed staging binding grants no authority over the new entry.
            let retired = (|| {
                let parent = &self.directories[0];
                let visible = open(parent, STAGING)?;
                let metadata = io(visible.metadata());
                let closed = close(visible);
                let metadata = metadata?;
                closed?;
                if metadata.dev() != stamp.dev() || metadata.ino() != stamp.ino() {
                    return Err(failure());
                }
                rustix::fs::unlinkat(parent.file(), STAGING, AtFlags::empty())
                    .map_err(|_| failure())?;
                io(parent.file().sync_all())
            })();
            result = result.and(retired);
        }
        for (index, mut input) in self.inputs.into_iter().enumerate() {
            // Replacing the target intentionally unlinks the original inode.
            if !(self.stage.published && index == 0) {
                result = result.and(input.check(&self.directories[input.directory]));
            }
            result = result.and(checked_close(input.file, hook));
        }
        if self.stage.published {
            // Other input closes may fail or reveal a changed published name.
            // Re-admit the new inode after all their checked-close observations.
            let checked = (|| {
                let parent = &self.directories[0];
                let fresh = Input::open(
                    0,
                    parent,
                    MIRRORS,
                    owner_of(&self.stage)?,
                    policy::AFTER.len() as u64,
                )?;
                let expected = self.stage.visible.as_ref().ok_or_else(failure);
                let valid =
                    expected.map(|stamp| same(stamp, &fresh.stamp) && fresh.bytes == policy::AFTER);
                let closed = close(fresh.file);
                if !valid? {
                    return Err(failure());
                }
                closed
            })();
            result = result.and(checked);
        }
        for directory in self.directories.into_iter().rev() {
            let closed = directory.finish().map_err(|_| failure());
            let observed = hook(Point::Close);
            result = result.and(closed.and(observed));
        }
        result
    }
}

fn owner_of(stage: &Stage) -> tooling::Result<Owner> {
    let stamp = stage.stamp.as_ref().ok_or_else(failure)?;
    Ok(Owner {
        uid: stamp.uid(),
        gid: stamp.gid(),
    })
}

fn apply_at(
    root: &Path,
    owner: Owner,
    hook: &mut impl FnMut(Point) -> tooling::Result<()>,
) -> tooling::Result<()> {
    let mut owned = Owned::new();
    let result = (|| {
        if owned.prepare(root, owner)? {
            owned.replace(owner, hook)?;
        }
        Ok(())
    })();
    let finished = owned.finish(hook);
    result.and(finished)
}

/// Called only after the parent admits the fixed hosted/root context. No path,
/// owner or mirror selector is exposed through the command interface.
pub(super) fn apply() -> tooling::Result<()> {
    apply_at(Path::new("/"), Owner { uid: 0, gid: 0 }, &mut |_| Ok(()))
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
