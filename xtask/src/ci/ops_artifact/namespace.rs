//! Descriptor-accounted nofollow owners. Directory cursor closes are RAII;
//! acquired File owners attempt explicit closes on success and failure paths.

use super::{failure, io};
use crate::tooling::Result;
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

pub(super) const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const DESCRIPTORS: usize = 128;

#[derive(Clone, Default)]
pub(super) struct Budget(Arc<AtomicUsize>);
pub(super) struct Permit(Budget);
impl Budget {
    pub fn reserve(&self, count: usize) -> Result<Vec<Permit>> {
        (0..count).map(|_| self.permit()).collect()
    }
    pub fn permit(&self) -> Result<Permit> {
        self.0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                (n < DESCRIPTORS).then_some(n + 1)
            })
            .map_err(|_| failure(1))?;
        Ok(Permit(self.clone()))
    }
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.0.load(Ordering::SeqCst)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
pub(super) fn path(value: &Path) -> bool {
    value.is_absolute()
        && value.as_os_str().len() <= 4096
        && value.components().collect::<PathBuf>().as_os_str() == value.as_os_str()
        && value.components().count() <= 32
        && value
            .components()
            .all(|c| matches!(c, Component::RootDir | Component::Normal(_)))
        && !value
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|c| matches!(c, 0..=31 | 127))
}
pub(super) struct Held {
    file: Option<File>,
    _permit: Permit,
}
impl Held {
    fn acquire(
        budget: &Budget,
        open: impl FnOnce() -> rustix::io::Result<rustix::fd::OwnedFd>,
    ) -> Result<Self> {
        let permit = budget.permit()?;
        let file = open().map(File::from).map_err(|_| failure(1))?;
        Ok(Self {
            file: Some(file),
            _permit: permit,
        })
    }
    pub fn root(budget: &Budget) -> Result<Self> {
        Self::acquire(budget, || {
            rustix::fs::open("/", FLAGS | OFlags::DIRECTORY, Mode::empty())
        })
    }
    pub fn open(
        parent: &File,
        name: &std::ffi::OsStr,
        flags: OFlags,
        budget: &Budget,
    ) -> Result<Self> {
        Self::acquire(budget, || {
            rustix::fs::openat(parent, name, flags, Mode::empty())
        })
    }
    pub fn create(parent: &File, name: &str, executable: bool, budget: &Budget) -> Result<Self> {
        let value = Self::acquire(budget, || {
            rustix::fs::openat(
                parent,
                name,
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(if executable { 0o700 } else { 0o600 }),
            )
        })?;
        let metadata = private_file(&value, executable)?;
        if metadata.len() != 0 {
            return Err(failure(1));
        }
        Ok(value)
    }
    pub fn file(&self) -> &File {
        self.file.as_ref().unwrap()
    }
    pub fn finish(mut self) -> Result<()> {
        nix::unistd::close(self.file.take().unwrap()).map_err(|_| failure(1))
    }
}
impl Drop for Held {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = nix::unistd::close(file);
        }
    }
}

pub(super) fn private_directory(value: &Held) -> Result<Metadata> {
    let metadata = io(value.file().metadata())?;
    if !metadata.is_dir()
        || metadata.mode() & 0o7777 != 0o700
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        Err(failure(1))
    } else {
        Ok(metadata)
    }
}

pub(super) fn private_file(value: &Held, executable: bool) -> Result<Metadata> {
    let metadata = io(value.file().metadata())?;
    if !metadata.is_file()
        || metadata.nlink() != 1
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.mode() & 0o7777 != if executable { 0o700 } else { 0o600 }
    {
        Err(failure(1))
    } else {
        Ok(metadata)
    }
}

/// Ancestors retain identity/mode/owner, not sibling-sensitive full timestamps.
pub(super) struct Chain {
    files: Vec<Held>,
    stamps: Vec<Metadata>,
    names: Vec<std::ffi::OsString>,
    pub path: PathBuf,
    budget: Budget,
}
impl Chain {
    pub fn open(pathname: &Path, budget: &Budget) -> Result<Self> {
        if !path(pathname) {
            return Err(failure(1));
        }
        let root = Held::root(budget)?;
        let stamp = io(root.file().metadata())?;
        let mut value = Self {
            files: vec![root],
            stamps: vec![stamp],
            names: vec![],
            path: PathBuf::from("/"),
            budget: budget.clone(),
        };
        for component in pathname.components() {
            if let Component::Normal(name) = component {
                value.push(name)?;
            }
        }
        value.verify()?;
        Ok(value)
    }
    fn push(&mut self, name: &std::ffi::OsStr) -> Result<()> {
        let file = Held::open(self.file(), name, FLAGS | OFlags::DIRECTORY, &self.budget)?;
        let stamp = io(file.file().metadata())?;
        self.files.push(file);
        self.stamps.push(stamp);
        self.names.push(name.into());
        self.path.push(name);
        Ok(())
    }
    pub fn file(&self) -> &File {
        self.files.last().unwrap().file()
    }
    pub fn verify(&self) -> Result<()> {
        for (file, stamp) in self.files.iter().zip(&self.stamps) {
            if !binding(stamp, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
        }
        for (i, name) in self.names.iter().enumerate() {
            let file = Held::open(
                self.files[i].file(),
                name,
                FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let observed = io(file.file().metadata())?;
            let matched = binding(&observed, &self.stamps[i + 1]);
            file.finish()?;
            if !matched {
                return Err(failure(1));
            }
        }
        Ok(())
    }
    /// Every effective outside-ancestor config is refused, including legacy
    /// spelling. Root/current-user authority excludes mutable shared parents.
    pub fn controlled(&self) -> Result<()> {
        self.verify()?;
        for file in &self.files {
            let metadata = io(file.file().metadata())?;
            if metadata.mode() & 0o022 != 0
                || ![0, rustix::process::getuid().as_raw()].contains(&metadata.uid())
            {
                return Err(failure(1));
            }
        }
        self.configuration_absent()
    }
    pub fn configuration_absent(&self) -> Result<()> {
        self.verify()?;
        for file in &self.files {
            let cargo = match Held::open(
                file.file(),
                std::ffi::OsStr::new(".cargo"),
                FLAGS | OFlags::DIRECTORY,
                &self.budget,
            ) {
                Ok(value) => value,
                Err(_) => {
                    match rustix::fs::statat(file.file(), ".cargo", AtFlags::SYMLINK_NOFOLLOW) {
                        Err(rustix::io::Errno::NOENT) => continue,
                        _ => return Err(failure(1)),
                    }
                }
            };
            let before = io(cargo.file().metadata())?;
            if before.mode() & 0o022 != 0
                || ![0, rustix::process::getuid().as_raw()].contains(&before.uid())
            {
                return Err(failure(1));
            }
            for name in ["config", "config.toml"] {
                if !matches!(
                    rustix::fs::statat(cargo.file(), name, AtFlags::SYMLINK_NOFOLLOW),
                    Err(rustix::io::Errno::NOENT)
                ) {
                    return Err(failure(1));
                }
            }
            let visible = Held::open(
                file.file(),
                std::ffi::OsStr::new(".cargo"),
                FLAGS | OFlags::DIRECTORY,
                &self.budget,
            )?;
            let matched = binding(&before, &io(cargo.file().metadata())?)
                && binding(&before, &io(visible.file().metadata())?);
            visible.finish()?;
            cargo.finish()?;
            if !matched {
                return Err(failure(1));
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let verified = self.verify();
        let mut closed = true;
        for file in self.files.drain(..).rev() {
            closed &= file.finish().is_ok();
        }
        verified?;
        if closed { Ok(()) } else { Err(failure(1)) }
    }
}

pub(super) fn read(pathname: &Path, maximum: u64, budget: &Budget) -> Result<Vec<u8>> {
    read_bound(pathname, maximum, budget, None)
}
pub(super) fn read_bound(
    pathname: &Path,
    maximum: u64,
    budget: &Budget,
    selected: Option<&Metadata>,
) -> Result<Vec<u8>> {
    if !path(pathname) {
        return Err(failure(1));
    }
    let parent = Chain::open(pathname.parent().ok_or_else(|| failure(1))?, budget)?;
    let result = (|| {
        let file = Held::open(
            parent.file(),
            pathname.file_name().ok_or_else(|| failure(1))?,
            FLAGS,
            budget,
        )?;
        let result = (|| {
            let before = io(file.file().metadata())?;
            if !before.is_file() || before.nlink() != 1 || before.len() > maximum {
                return Err(failure(1));
            }
            if selected.is_some_and(|stamp| !binding(stamp, &before)) {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            io(file.file().take(maximum + 1).read_to_end(&mut bytes))?;
            if bytes.len() as u64 != before.len() || !same(&before, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
            let visible = Held::open(parent.file(), pathname.file_name().unwrap(), FLAGS, budget)?;
            let matched = same(&before, &io(visible.file().metadata())?);
            visible.finish()?;
            if !matched {
                return Err(failure(1));
            }
            Ok(bytes)
        })();
        let closed = file.finish();
        result.and_then(|value| closed.map(|()| value))
    })();
    let closed = parent.finish();
    result.and_then(|value| closed.map(|()| value))
}

pub(super) struct OwnedRun {
    pub parent: Chain,
    leaf: Held,
    stamp: Metadata,
    pub path: PathBuf,
    budget: Budget,
    controlled: bool,
}
impl OwnedRun {
    pub fn create(pathname: &Path, budget: &Budget) -> Result<Self> {
        Self::create_with(pathname, budget, true)
    }
    #[cfg(test)]
    pub fn fixture(pathname: &Path, budget: &Budget) -> Result<Self> {
        Self::create_with(pathname, budget, false)
    }
    fn create_with(pathname: &Path, budget: &Budget, controlled: bool) -> Result<Self> {
        let parent = Chain::open(pathname.parent().ok_or_else(|| failure(1))?, budget)?;
        if controlled {
            parent.controlled()?;
        } else {
            parent.verify()?;
        }
        let name = pathname.file_name().ok_or_else(|| failure(1))?;
        rustix::fs::mkdirat(parent.file(), name, Mode::from_bits_truncate(0o700))
            .map_err(|_| failure(1))?;
        let leaf = Held::open(parent.file(), name, FLAGS | OFlags::DIRECTORY, budget)?;
        let stamp = io(leaf.file().metadata())?;
        if stamp.mode() & 0o7777 != 0o700 || stamp.uid() != rustix::process::getuid().as_raw() {
            return Err(failure(1));
        }
        Ok(Self {
            parent,
            leaf,
            stamp,
            path: pathname.into(),
            budget: budget.clone(),
            controlled,
        })
    }
    pub fn file(&self) -> &File {
        self.leaf.file()
    }
    pub fn budget(&self) -> &Budget {
        &self.budget
    }
    pub fn check(&self) -> Result<()> {
        if self.controlled {
            self.parent.controlled()?;
        } else {
            self.parent.verify()?;
        }
        let visible = Held::open(
            self.parent.file(),
            self.path.file_name().unwrap(),
            FLAGS | OFlags::DIRECTORY,
            &self.budget,
        )?;
        let matched = binding(&self.stamp, &io(visible.file().metadata())?)
            && binding(&self.stamp, &io(self.file().metadata())?);
        visible.finish()?;
        if matched { Ok(()) } else { Err(failure(1)) }
    }
    pub fn directory(&self, name: &str) -> Result<Held> {
        self.check()?;
        if name.is_empty() || name.contains('/') || name == "." || name == ".." {
            return Err(failure(1));
        }
        rustix::fs::mkdirat(self.file(), name, Mode::from_bits_truncate(0o700))
            .map_err(|_| failure(1))?;
        let value = Held::open(
            self.file(),
            std::ffi::OsStr::new(name),
            FLAGS | OFlags::DIRECTORY,
            &self.budget,
        )?;
        private_directory(&value)?;
        self.check()?;
        Ok(value)
    }
    pub fn record(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.check()?;
        if !matches!(
            name,
            "ops-release.json"
                | "producer-materials.json"
                | "producer-recipe.json"
                | "producer-toolchain.json"
                | "producer-resolution.json"
                | "producer-outcome.json"
        ) || bytes.len() > 1024 * 1024
        {
            return Err(failure(1));
        }
        write(self.file(), name, bytes, &self.budget)?;
        self.check()
    }
    pub fn private_record(&self, name: &str, bytes: &[u8]) -> Result<()> {
        let maximum = match name {
            "cargo-metadata.json" => 16 * 1024 * 1024,
            "cargo-version.txt" | "rustc-version.txt" => 16 * 1024,
            "rustc-sysroot.txt" => 4096,
            _ => return Err(failure(1)),
        };
        if bytes.len() > maximum {
            return Err(failure(1));
        }
        self.check()?;
        write(self.file(), name, bytes, &self.budget)?;
        self.check()
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let leaf = self.leaf.finish();
        let parent = self.parent.finish();
        checked?;
        leaf?;
        parent
    }
}

/// Re-read actual stored bytes through the writer descriptor before admitting
/// its stamp; then recheck the visible same-inode binding before explicit close.
pub(super) fn write(parent: &File, name: &str, bytes: &[u8], budget: &Budget) -> Result<Metadata> {
    let output = Held::create(parent, name, false, budget)?;
    let admitted = private_file(&output, false)?;
    let written = (|| {
        let mut writer = output.file();
        io(writer.write_all(bytes).and_then(|()| writer.sync_all()))?;
        let before = private_file(&output, false)?;
        if !binding(&admitted, &before) {
            return Err(failure(1));
        }
        io(writer.seek(SeekFrom::Start(0)))?;
        let mut actual = Vec::new();
        io(writer.take(bytes.len() as u64 + 1).read_to_end(&mut actual))?;
        if actual != bytes || !same(&before, &io(output.file().metadata())?) {
            return Err(failure(1));
        }
        let visible = Held::open(parent, std::ffi::OsStr::new(name), FLAGS, budget)?;
        let matched = same(&before, &io(visible.file().metadata())?);
        visible.finish()?;
        if !matched {
            return Err(failure(1));
        }
        Ok(before)
    })();
    let closed = output.finish();
    written.and_then(|value| closed.map(|()| value))
}
