//! Descriptor-relative managed namespace. Directories and file witnesses remain
//! bound across rename/link operations; every acquired File gets a close attempt.

use super::{Result, fail, require};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
};

pub(super) const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| fail("asset_operation_failed"))
}
pub(super) fn sys<T>(value: rustix::io::Result<T>) -> Result<T> {
    value.map_err(|_| fail("asset_operation_failed"))
}
fn convert<T: TryInto<U>, U>(value: T) -> Result<U> {
    value.try_into().map_err(|_| fail("asset_operation_failed"))
}
#[cfg(test)]
pub(super) type Hook = std::rc::Rc<dyn Fn(&'static str, &str) -> Result<()>>;
#[derive(Clone, Default)]
pub(super) struct Access {
    #[cfg(test)]
    pub owner: Option<u32>,
    #[cfg(test)]
    pub hook: Option<Hook>,
}
impl Access {
    pub fn owner(&self, value: u32) -> u32 {
        #[cfg(test)]
        if self.owner == Some(value) {
            return 0;
        }
        value
    }
    pub fn event(&self, _event: &'static str, _path: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            return hook(_event, _path);
        }
        Ok(())
    }
    pub fn close(&self, file: File) -> Result<()> {
        let result = nix::unistd::close(file).map_err(|_| fail("asset_operation_failed"));
        let observed = self.event("close", "");
        result.and(observed)
    }
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn stable(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn link_change(a: &Metadata, b: &Metadata, delta: i64) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec()) == (b.mtime(), b.mtime_nsec())
        && a.nlink().checked_add_signed(delta) == Some(b.nlink())
}
pub(super) fn component(name: &str) -> Result<()> {
    require(
        !name.is_empty()
            && name.len() <= 255
            && name != "."
            && name != ".."
            && !name.bytes().any(|byte| byte == b'/' || byte == 0),
        "asset_operation_failed",
    )
}
struct Directory {
    file: Option<File>,
    parent: Option<usize>,
    name: String,
    stamp: Metadata,
}
struct Record {
    parent: usize,
    name: String,
    stamp: Metadata,
    active: bool,
}
pub(super) type Dir = usize;
pub(super) type Entry = usize;
#[derive(Clone, PartialEq, Eq)]
struct LinkStamp {
    dev: u64,
    ino: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    size: u64,
    nlink: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}
fn same_stat(value: &LinkStamp, metadata: &Metadata) -> bool {
    value.dev == metadata.dev()
        && value.ino == metadata.ino()
        && value.mode == metadata.mode()
        && value.uid == metadata.uid()
        && value.gid == metadata.gid()
        && value.size == metadata.len()
        && value.nlink == metadata.nlink()
        && value.mtime == (metadata.mtime(), metadata.mtime_nsec())
        && value.ctime == (metadata.ctime(), metadata.ctime_nsec())
}
struct Link {
    parent: Dir,
    name: String,
    stamp: LinkStamp,
    target: Vec<u8>,
    active: bool,
}
pub(super) type Pointer = usize;
pub(super) struct Fs {
    pub access: Access,
    directories: Vec<Directory>,
    records: Vec<Record>,
    links: Vec<Link>,
}

use super::scope::Scope;
use std::{
    borrow::{Borrow, BorrowMut},
    ops::{Deref, DerefMut},
};

/// A lexical direct-owner scope over the same retained arena. No clock is stored in Fs.
pub(super) struct ScopedFs<'flow, 'owner, T> {
    inner: T,
    scope: &'flow Scope<'owner>,
}
impl<'flow, 'owner, T> ScopedFs<'flow, 'owner, T> {
    pub fn scope(&self) -> &'flow Scope<'owner> {
        self.scope
    }
    pub fn new(inner: T, scope: &'flow Scope<'owner>) -> Self {
        Self { inner, scope }
    }
}
impl<T: Borrow<Fs>> Deref for ScopedFs<'_, '_, T> {
    type Target = Fs;
    fn deref(&self) -> &Fs {
        self.inner.borrow()
    }
}
impl<T: BorrowMut<Fs>> DerefMut for ScopedFs<'_, '_, T> {
    fn deref_mut(&mut self) -> &mut Fs {
        self.inner.borrow_mut()
    }
}

impl Fs {
    pub fn native() -> Result<Self> {
        let file = File::from(sys(rustix::fs::open(
            "/",
            FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        ))?);
        Self::open(file, Access::default())
    }
    pub fn open(file: File, access: Access) -> Result<Self> {
        let stamp = io(file.metadata());
        let admitted = stamp.and_then(|stamp| {
            require(
                stamp.is_dir() && access.owner(stamp.uid()) == 0 && stamp.mode() & 0o022 == 0,
                "asset_operation_failed",
            )?;
            Ok(stamp)
        });
        match admitted {
            Ok(stamp) => Ok(Self {
                access,
                directories: vec![Directory {
                    file: Some(file),
                    parent: None,
                    name: String::new(),
                    stamp,
                }],
                records: vec![],
                links: vec![],
            }),
            Err(error) => {
                let _ = access.close(file);
                Err(error)
            }
        }
    }
    pub fn open_scoped(file: File, access: Access, scope: &Scope<'_>) -> Result<Self> {
        let admitted = (|| {
            scope.check()?;
            let stamp = scope.io(|| file.metadata())?;
            require(
                stamp.is_dir() && access.owner(stamp.uid()) == 0 && stamp.mode() & 0o022 == 0,
                "asset_operation_failed",
            )?;
            scope.check()?;
            Ok(stamp)
        })();
        scope.observed(&admitted);
        match admitted {
            Ok(stamp) => Ok(Self {
                access,
                directories: vec![Directory {
                    file: Some(file),
                    parent: None,
                    name: String::new(),
                    stamp,
                }],
                records: vec![],
                links: vec![],
            }),
            Err(first) => {
                let _ = scope.closed(access.close(file));
                Err(first)
            }
        }
    }
    pub fn fd(&self, directory: Dir) -> &File {
        self.directories[directory].file.as_ref().unwrap()
    }
    #[cfg(test)]
    pub fn owners(&self) -> Vec<(i32, Metadata)> {
        use std::os::fd::AsRawFd;
        self.directories
            .iter()
            .filter_map(|directory| directory.file.as_ref())
            .map(|file| (file.as_raw_fd(), file.metadata().unwrap()))
            .collect()
    }
    pub fn stamp(&self, entry: Entry) -> &Metadata {
        &self.records[entry].stamp
    }
    pub fn mode(&self, entry: Entry) -> u32 {
        self.stamp(entry).mode() & 0o7777
    }
    pub fn entry_at(&self, entry: Entry, parent: Dir, name: &str) -> bool {
        let record = &self.records[entry];
        record.active && record.parent == parent && record.name == name
    }
    fn path(&self, directory: Dir, name: &str) -> String {
        let mut parts = vec![name.to_owned()];
        let mut current = directory;
        while let Some(parent) = self.directories[current].parent {
            parts.push(self.directories[current].name.clone());
            current = parent;
        }
        parts.reverse();
        parts.join("/")
    }
    pub fn finish(self) -> Result<()> {
        self.finish_scoped(&Scope::standalone())
    }
    pub fn finish_scoped(mut self, scope: &Scope<'_>) -> Result<()> {
        let mut result = ScopedFs::new(&self, scope).verify();
        scope.observed(&result);
        for directory in self.directories.iter_mut().rev() {
            if let Some(file) = directory.file.take() {
                let closed = scope.closed(self.access.close(file));
                result = result.and(closed);
            }
        }
        scope.result(result)
    }

    pub fn absent(&self, parent: Dir, name: &str) -> Result<bool> {
        let scope = Scope::standalone();
        ScopedFs::new(self, &scope).absent(parent, name)
    }
    pub fn directory(&mut self, parent: Dir, name: &str, exact: Option<u32>) -> Result<Dir> {
        let scope = Scope::standalone();
        ScopedFs::new(&mut *self, &scope).directory(parent, name, exact)
    }
    pub fn mkdir(&mut self, parent: Dir, name: &str, mode: u32) -> Result<Dir> {
        let scope = Scope::standalone();
        ScopedFs::new(&mut *self, &scope).mkdir(parent, name, mode)
    }
    pub fn check_entry(&self, entry: Entry) -> Result<()> {
        let scope = Scope::standalone();
        ScopedFs::new(self, &scope).check_entry(entry)
    }
    pub fn read(&mut self, parent: Dir, name: &str, maximum: u64) -> Result<(Vec<u8>, Entry)> {
        let scope = Scope::standalone();
        ScopedFs::new(&mut *self, &scope).read(parent, name, maximum)
    }
    pub fn verify(&self) -> Result<()> {
        let scope = Scope::standalone();
        ScopedFs::new(self, &scope).verify()
    }
    pub fn metadata(&mut self, parent: Dir, name: &str) -> Result<Entry> {
        let scope = Scope::standalone();
        ScopedFs::new(&mut *self, &scope).metadata(parent, name)
    }
    pub fn lock_file(&mut self, parent: Dir) -> Result<(File, Entry)> {
        let scope = Scope::standalone();
        ScopedFs::new(&mut *self, &scope).lock_file(parent)
    }
}

impl<T: Borrow<Fs>> ScopedFs<'_, '_, T> {
    fn close_file(&self, file: File) -> Result<()> {
        self.scope.closed(self.access.close(file))
    }
    pub fn absent(&self, parent: Dir, name: &str) -> Result<bool> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            match rustix::fs::statat(self.fd(parent), name, AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => Ok(true),
                Ok(_) => Ok(false),
                Err(_) => {
                    self.scope
                        .record(crate::release::attestation::control::Cause::Io);
                    Err(fail("asset_operation_failed"))
                }
            }
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    fn visible(&self, parent: Dir, name: &str, directory: bool) -> Result<Metadata> {
        self.scope.check()?;
        let result = (|| {
            let admitted = self.link_stamp(parent, name)?;
            require(
                admitted.mode & 0o170000 == if directory { 0o040000 } else { 0o100000 },
                "asset_store_ownership",
            )?;
            let file = File::from(self.scope.sys(|| {
                rustix::fs::openat(
                    self.fd(parent),
                    name,
                    FLAGS
                        | if directory {
                            OFlags::DIRECTORY
                        } else {
                            OFlags::empty()
                        },
                    Mode::empty(),
                )
            })?);
            let stamp = self.scope.io(|| file.metadata()).and_then(|stamp| {
                require(same_stat(&admitted, &stamp), "asset_operation_failed")?;
                Ok(stamp)
            });
            self.scope.observed(&stamp);
            let closed = self.close_file(file);
            let stamp = stamp?;
            closed?;
            Ok(stamp)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn check_dir(&self, directory: Dir) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            let held = &self.directories[directory];
            let retained = self.scope.io(|| self.fd(directory).metadata())?;
            require(binding(&held.stamp, &retained), "asset_operation_failed")?;
            if let Some(parent) = held.parent {
                let visible = self.visible(parent, &held.name, true)?;
                require(binding(&held.stamp, &visible), "asset_operation_failed")?;
            }
            Ok(())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn protected_dir(&self, directory: Dir, exact: Option<u32>) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            self.check_dir(directory)?;
            let stamp = self.scope.io(|| self.fd(directory).metadata())?;
            require(
                stamp.is_dir()
                    && self.access.owner(stamp.uid()) == 0
                    && stamp.mode() & 0o022 == 0
                    && exact.is_none_or(|mode| stamp.mode() & 0o7777 == mode),
                "asset_store_ownership",
            )
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn list(&self, directory: Dir, maximum: usize) -> Result<Vec<String>> {
        self.scope.check()?;
        let result = (|| {
            self.check_dir(directory)?;
            let mut output = vec![];
            let mut entries = self
                .scope
                .sys(|| rustix::fs::Dir::read_from(self.fd(directory)))?;
            loop {
                self.scope.check()?;
                let Some(entry) = entries.next() else { break };
                let entry = self
                    .scope
                    .observed_io(entry.map_err(std::io::Error::from))?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                require(output.len() < maximum, "asset_operation_failed")?;
                let name = std::str::from_utf8(name).map_err(|_| fail("asset_operation_failed"))?;
                component(name)?;
                output.push(name.to_owned());
            }
            self.check_dir(directory)?;
            Ok(output)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn check_entry(&self, entry: Entry) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            let value = &self.records[entry];
            require(value.active, "asset_operation_failed")?;
            let observed = self.visible(value.parent, &value.name, false)?;
            require(stable(&value.stamp, &observed), "asset_operation_failed")
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn sync(&self, directory: Dir) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            self.check_dir(directory)?;
            self.access.event("sync", &self.path(directory, ""))?;
            self.scope.io(|| self.fd(directory).sync_all())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn verify(&self) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            for (index, directory) in self.directories.iter().enumerate() {
                if directory.file.is_some() {
                    self.check_dir(index)?;
                }
            }
            for (index, record) in self.records.iter().enumerate() {
                if record.active {
                    self.check_entry(index)?;
                }
            }
            for (index, link) in self.links.iter().enumerate() {
                if link.active {
                    self.check_link(index)?;
                }
            }
            Ok(())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    fn link_stamp(&self, parent: Dir, name: &str) -> Result<LinkStamp> {
        self.scope.check()?;
        let result = (|| {
            let value = self
                .scope
                .sys(|| rustix::fs::statat(self.fd(parent), name, AtFlags::SYMLINK_NOFOLLOW))?;
            Ok(LinkStamp {
                dev: convert(value.st_dev)?,
                ino: convert(value.st_ino)?,
                mode: convert(value.st_mode)?,
                uid: value.st_uid,
                gid: value.st_gid,
                size: convert(value.st_size)?,
                nlink: convert(value.st_nlink)?,
                mtime: (convert(value.st_mtime)?, convert(value.st_mtime_nsec)?),
                ctime: (convert(value.st_ctime)?, convert(value.st_ctime_nsec)?),
            })
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    fn link_value(&self, parent: Dir, name: &str) -> Result<(LinkStamp, Vec<u8>)> {
        self.scope.check()?;
        let result = (|| {
            let stamp = self.link_stamp(parent, name)?;
            require(
                stamp.mode & 0o170000 == 0o120000 && self.access.owner(stamp.uid) == 0,
                "asset_public_pointer",
            )?;
            require(stamp.size <= 128, "asset_public_target")?;
            let mut buffer = [0u8; 129];
            let length = self
                .scope
                .sys(|| rustix::fs::readlinkat_raw(self.fd(parent), name, &mut buffer[..]))?;
            require(length <= 128, "asset_public_target")?;
            let bytes = buffer[..length].to_vec();
            require(
                self.link_stamp(parent, name)? == stamp,
                "asset_operation_failed",
            )?;
            Ok((stamp, bytes))
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn check_link(&self, link: Pointer) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            let link = &self.links[link];
            require(link.active, "asset_operation_failed")?;
            let (stamp, target) = self.link_value(link.parent, &link.name)?;
            require(
                stamp == link.stamp && target == link.target,
                "asset_operation_failed",
            )
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
}

impl<T: BorrowMut<Fs>> ScopedFs<'_, '_, T> {
    pub fn directory(&mut self, parent: Dir, name: &str, exact: Option<u32>) -> Result<Dir> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            if let Some((index, _)) = self.directories.iter().enumerate().find(|(_, value)| {
                value.file.is_some() && value.parent == Some(parent) && value.name == name
            }) {
                self.protected_dir(index, exact)?;
                return Ok(index);
            }
            require(
                self.directories
                    .iter()
                    .filter(|value| value.file.is_some())
                    .count()
                    < 64,
                "asset_operation_failed",
            )?;
            let file = File::from(self.scope.sys(|| {
                rustix::fs::openat(
                    self.fd(parent),
                    name,
                    FLAGS | OFlags::DIRECTORY,
                    Mode::empty(),
                )
            })?);
            let stamp = self.scope.io(|| file.metadata());
            let admitted = stamp.and_then(|stamp| {
                require(
                    stamp.is_dir()
                        && self.access.owner(stamp.uid()) == 0
                        && stamp.mode() & 0o022 == 0
                        && exact.is_none_or(|mode| stamp.mode() & 0o7777 == mode),
                    "asset_store_ownership",
                )?;
                let visible = self.visible(parent, name, true)?;
                require(binding(&stamp, &visible), "asset_operation_failed")?;
                Ok(stamp)
            });
            let stamp = match admitted {
                Ok(stamp) => stamp,
                Err(error) => {
                    self.scope.observed(&Err::<(), _>(error));
                    let _ = self.close_file(file);
                    return Err(error);
                }
            };
            let value = Directory {
                file: Some(file),
                parent: Some(parent),
                name: name.into(),
                stamp,
            };
            let index = if let Some(index) = self
                .directories
                .iter()
                .position(|value| value.file.is_none())
            {
                self.directories[index] = value;
                index
            } else {
                self.directories.push(value);
                self.directories.len() - 1
            };
            Ok(index)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn mkdir(&mut self, parent: Dir, name: &str, mode: u32) -> Result<Dir> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            self.scope.sys(|| {
                rustix::fs::mkdirat(self.fd(parent), name, Mode::from_bits_truncate(0o700))
            })?;
            // The fresh entry is not deleted if its identity could not be admitted.
            let directory = self.directory(parent, name, None)?;
            let bits = convert(mode)?;
            let changed = self
                .scope
                .sys(|| rustix::fs::fchmod(self.fd(directory), Mode::from_bits_truncate(bits)));
            let stamp = self.scope.io(|| self.fd(directory).metadata());
            changed?;
            let stamp = stamp?;
            require(
                stamp.mode() & 0o7777 == mode && self.access.owner(stamp.uid()) == 0,
                "asset_store_ownership",
            )?;
            self.directories[directory].stamp = stamp;
            self.check_dir(directory)?;
            Ok(directory)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    fn insert(&mut self, parent: Dir, name: &str, stamp: Metadata) -> Result<Entry> {
        self.scope.check()?;
        let result = (|| {
            if let Some(index) = self
                .records
                .iter()
                .position(|value| value.active && value.parent == parent && value.name == name)
            {
                require(
                    stable(&self.records[index].stamp, &stamp),
                    "asset_operation_failed",
                )?;
                return Ok(index);
            }
            let value = Record {
                parent,
                name: name.into(),
                stamp,
                active: true,
            };
            if let Some(index) = self.records.iter().position(|value| !value.active) {
                self.records[index] = value;
                Ok(index)
            } else {
                require(self.records.len() < 4096, "asset_operation_failed")?;
                self.records.push(value);
                Ok(self.records.len() - 1)
            }
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn read(&mut self, parent: Dir, name: &str, maximum: u64) -> Result<(Vec<u8>, Entry)> {
        self.scope.check()?;
        let result = (|| {
            let (bytes, _, entry) = self.consume(parent, name, maximum, true)?;
            Ok((bytes, entry))
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn hash(&mut self, parent: Dir, name: &str, maximum: u64) -> Result<(String, u64, Entry)> {
        self.scope.check()?;
        let result = (|| {
            let (_, hash, entry) = self.consume(parent, name, maximum, false)?;
            Ok((hash, self.records[entry].stamp.len(), entry))
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    fn consume(
        &mut self,
        parent: Dir,
        name: &str,
        maximum: u64,
        collect: bool,
    ) -> Result<(Vec<u8>, String, Entry)> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            let admitted = self.link_stamp(parent, name)?;
            require(
                admitted.mode & 0o170000 == 0o100000,
                "asset_store_ownership",
            )?;
            self.access.event("open-read", &self.path(parent, name))?;
            let mut file = File::from(
                self.scope
                    .sys(|| rustix::fs::openat(self.fd(parent), name, FLAGS, Mode::empty()))?,
            );
            let result = (|| {
                let before = self.scope.io(|| file.metadata())?;
                require(same_stat(&admitted, &before), "asset_operation_failed")?;
                require(
                    before.is_file()
                        && self.access.owner(before.uid()) == 0
                        && before.mode() & 0o022 == 0,
                    "asset_store_ownership",
                )?;
                require(before.len() <= maximum, "asset_operation_failed")?;
                let mut bytes = vec![];
                if collect {
                    bytes
                        .try_reserve_exact(before.len() as usize)
                        .map_err(|_| fail("asset_operation_failed"))?;
                }
                let mut hash = Sha256::new();
                let mut remaining = before.len();
                let mut buffer = vec![0; 65536];
                while remaining != 0 {
                    let length = usize::try_from(remaining.min(buffer.len() as u64))
                        .map_err(|_| fail("asset_operation_failed"))?;
                    self.access.event("read", &self.path(parent, name))?;
                    self.scope.check()?;
                    let length = self.scope.io(|| file.read(&mut buffer[..length]))?;
                    self.scope.check()?;
                    require(length > 0, "asset_operation_failed")?;
                    remaining -= length as u64;
                    hash.update(&buffer[..length]);
                    if collect {
                        bytes.extend_from_slice(&buffer[..length]);
                    }
                }
                require(
                    stable(&before, &self.scope.io(|| file.metadata())?),
                    "asset_operation_failed",
                )?;
                require(
                    stable(&before, &self.visible(parent, name, false)?),
                    "asset_operation_failed",
                )?;
                let hash = hash
                    .finalize()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect();
                Ok((bytes, hash, before))
            })();
            self.scope.observed(&result);
            let closed = self.close_file(file);
            let (bytes, hash, stamp) = result?;
            closed?;
            let entry = self.insert(parent, name, stamp)?;
            Ok((bytes, hash, entry))
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn write(&mut self, parent: Dir, name: &str, bytes: &[u8], mode: u32) -> Result<Entry> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            let mut file = File::from(self.scope.sys(|| {
                rustix::fs::openat(
                    self.fd(parent),
                    name,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::NONBLOCK
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )
            })?);
            let result = (|| {
                let empty = self.scope.io(|| file.metadata())?;
                require(
                    empty.is_file() && empty.len() == 0 && self.access.owner(empty.uid()) == 0,
                    "asset_store_ownership",
                )?;
                require(
                    binding(&empty, &self.visible(parent, name, false)?),
                    "asset_operation_failed",
                )?;
                self.access.event("write", &self.path(parent, name))?;
                let mut offset = 0;
                while offset < bytes.len() {
                    self.scope.check()?;
                    let end = (offset + 65536).min(bytes.len());
                    let written = match file.write(&bytes[offset..end]) {
                        Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                        value => self.scope.observed_io(value)?,
                    };
                    self.scope.check()?;
                    if written == 0 {
                        return self
                            .scope
                            .io(|| Err(std::io::Error::from(std::io::ErrorKind::WriteZero)));
                    }
                    offset += written;
                }
                self.scope.io(|| file.sync_all())?;
                let bits = convert(mode)?;
                self.scope
                    .sys(|| rustix::fs::fchmod(&file, Mode::from_bits_truncate(bits)))?;
                let stamp = self.scope.io(|| file.metadata())?;
                require(
                    stamp.len() == bytes.len() as u64
                        && stamp.mode() & 0o7777 == mode
                        && binding(&stamp, &self.visible(parent, name, false)?),
                    "asset_operation_failed",
                )?;
                Ok(stamp)
            })();
            self.scope.observed(&result);
            let closed = self.close_file(file);
            let stamp = result?;
            closed?;
            let (read, entry) = self.read(parent, name, bytes.len() as u64)?;
            require(
                read == bytes && stable(&stamp, &self.records[entry].stamp),
                "asset_operation_failed",
            )?;
            Ok(entry)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn metadata(&mut self, parent: Dir, name: &str) -> Result<Entry> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            let stamp = self.visible(parent, name, false)?;
            require(
                stamp.is_file() && self.access.owner(stamp.uid()) == 0 && stamp.mode() & 0o022 == 0,
                "asset_store_ownership",
            )?;
            self.insert(parent, name, stamp)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn lock_file(&mut self, parent: Dir) -> Result<(File, Entry)> {
        self.scope.check()?;
        let result = (|| {
            self.check_dir(parent)?;
            let created = rustix::fs::openat(
                self.fd(parent),
                "workload.lock",
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            );
            let (file, admitted) = match created {
                Ok(file) => (File::from(file), None),
                Err(rustix::io::Errno::EXIST) => {
                    let admitted = self.link_stamp(parent, "workload.lock")?;
                    require(
                        admitted.mode & 0o170000 == 0o100000,
                        "asset_operation_failed",
                    )?;
                    (
                        File::from(self.scope.sys(|| {
                            rustix::fs::openat(
                                self.fd(parent),
                                "workload.lock",
                                OFlags::RDWR
                                    | OFlags::NOFOLLOW
                                    | OFlags::NONBLOCK
                                    | OFlags::CLOEXEC,
                                Mode::empty(),
                            )
                        })?),
                        Some(admitted),
                    )
                }
                Err(_) => {
                    self.scope
                        .record(crate::release::attestation::control::Cause::Io);
                    return Err(fail("asset_operation_failed"));
                }
            };
            let result = (|| {
                let before = self.scope.io(|| file.metadata())?;
                require(
                    before.is_file() && self.access.owner(before.uid()) == 0,
                    "asset_operation_failed",
                )?;
                if let Some(admitted) = admitted {
                    require(same_stat(&admitted, &before), "asset_operation_failed")?;
                } else {
                    self.scope
                        .sys(|| rustix::fs::fchmod(&file, Mode::from_bits_truncate(0o600)))?;
                }
                let stamp = self.scope.io(|| file.metadata())?;
                require(
                    stamp.mode() & 0o7777 == 0o600
                        && stable(&stamp, &self.visible(parent, "workload.lock", false)?),
                    "asset_operation_failed",
                )?;
                self.insert(parent, "workload.lock", stamp)
            })();
            match result {
                Ok(entry) => Ok((file, entry)),
                Err(error) => {
                    self.scope.observed(&Err::<(), _>(error));
                    let _ = self.close_file(file);
                    Err(error)
                }
            }
        })();
        self.scope.observed(&result);
        result
    }
    fn refresh(&mut self, old: &Metadata, new: &Metadata, delta: i64) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            require(link_change(old, new, delta), "asset_operation_failed")?;
            for record in &mut self.records {
                if record.active
                    && record.stamp.dev() == old.dev()
                    && record.stamp.ino() == old.ino()
                {
                    require(stable(&record.stamp, old), "asset_operation_failed")?;
                    record.stamp = new.clone();
                }
            }
            Ok(())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn hardlink(&mut self, source: Entry, parent: Dir, name: &str) -> Result<Entry> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_entry(source)?;
            self.check_dir(parent)?;
            let from = &self.records[source];
            let before = from.stamp.clone();
            self.access.event("link", &self.path(parent, name))?;
            self.check_entry(source)?;
            self.check_dir(parent)?;
            self.scope.sys(|| {
                rustix::fs::linkat(
                    self.fd(from.parent),
                    from.name.as_str(),
                    self.fd(parent),
                    name,
                    AtFlags::empty(),
                )
            })?;
            let stamp = self.visible(parent, name, false)?;
            self.refresh(&before, &stamp, 1)?;
            self.insert(parent, name, stamp)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn rename_dir(&mut self, directory: Dir, parent: Dir, name: &str) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(directory)?;
            self.check_dir(parent)?;
            let from = self.directories[directory]
                .parent
                .ok_or_else(|| fail("asset_operation_failed"))?;
            let old = self.directories[directory].name.clone();
            self.access
                .event("rename-directory", &self.path(parent, name))?;
            self.check_dir(directory)?;
            self.check_dir(parent)?;
            self.scope.sys(|| {
                rustix::fs::renameat_with(
                    self.fd(from),
                    old.as_str(),
                    self.fd(parent),
                    name,
                    rustix::fs::RenameFlags::NOREPLACE,
                )
            })?;
            self.directories[directory].parent = Some(parent);
            self.directories[directory].name = name.into();
            self.check_dir(directory)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn rename_entry(
        &mut self,
        source: Entry,
        parent: Dir,
        name: &str,
        destination: Option<Entry>,
    ) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_entry(source)?;
            self.check_dir(parent)?;
            if let Some(destination) = destination {
                require(
                    self.records[destination].parent == parent
                        && self.records[destination].name == name,
                    "asset_operation_failed",
                )?;
                self.check_entry(destination)?;
            } else {
                require(self.absent(parent, name)?, "asset_operation_failed")?;
            }
            let record = &self.records[source];
            let old = record.stamp.clone();
            let from = record.parent;
            let old_name = record.name.clone();
            let replaced = destination.map(|index| self.records[index].stamp.clone());
            require(
                replaced
                    .as_ref()
                    .is_none_or(|stamp| stamp.dev() != old.dev() || stamp.ino() != old.ino()),
                "asset_operation_failed",
            )?;
            let held = File::from(self.scope.sys(|| {
                rustix::fs::openat(self.fd(from), old_name.as_str(), FLAGS, Mode::empty())
            })?);
            let moved = (|| {
                require(
                    stable(&old, &self.scope.io(|| held.metadata())?),
                    "asset_operation_failed",
                )?;
                self.access.event("rename-file", &self.path(parent, name))?;
                self.check_entry(source)?;
                if let Some(destination) = destination {
                    self.check_entry(destination)?;
                } else {
                    require(self.absent(parent, name)?, "asset_operation_failed")?;
                }
                self.scope.sys(|| {
                    rustix::fs::renameat(self.fd(from), old_name.as_str(), self.fd(parent), name)
                })?;
                if let Some(destination) = destination {
                    self.records[destination].active = false;
                }
                // Preserve the new namespace location even if the post-rename probe
                // fails. Cleanup may only use an identity it can still prove.
                self.records[source].parent = parent;
                self.records[source].name = name.into();
                // The retained source descriptor supplies the post-move inode stamp
                // before any fallible pathname probe or its checked close.
                let stamp = self.scope.io(|| held.metadata())?;
                self.refresh(&old, &stamp, 0)?;
                if let Some(before) = replaced
                    && let Some(other) = self.records.iter().find(|record| {
                        record.active
                            && record.stamp.dev() == before.dev()
                            && record.stamp.ino() == before.ino()
                    })
                {
                    let stamp = self.visible(other.parent, &other.name, false)?;
                    self.refresh(&before, &stamp, -1)?;
                }
                self.access
                    .event("renamed-file", &self.path(parent, name))?;
                self.check_entry(source)
            })();
            self.scope.observed(&moved);
            let closed = self.close_file(held);
            moved.and(closed)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn unlink(&mut self, entry: Entry) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            self.check_entry(entry)?;
            let record = &self.records[entry];
            let before = record.stamp.clone();
            self.access
                .event("unlink-file", &self.path(record.parent, &record.name))?;
            self.check_entry(entry)?;
            let record = &self.records[entry];
            self.scope.sys(|| {
                rustix::fs::unlinkat(
                    self.fd(record.parent),
                    record.name.as_str(),
                    AtFlags::empty(),
                )
            })?;
            self.records[entry].active = false;
            if let Some(other) = self.records.iter().find(|record| {
                record.active
                    && record.stamp.dev() == before.dev()
                    && record.stamp.ino() == before.ino()
            }) {
                let stamp = self.visible(other.parent, &other.name, false)?;
                self.refresh(&before, &stamp, -1)?;
            }
            Ok(())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn pointer(&mut self, parent: Dir, name: &str) -> Result<(Vec<u8>, Pointer)> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            let (stamp, target) = self.link_value(parent, name)?;
            if let Some(index) = self
                .links
                .iter()
                .position(|link| link.active && link.parent == parent && link.name == name)
            {
                require(
                    self.links[index].stamp == stamp && self.links[index].target == target,
                    "asset_operation_failed",
                )?;
                return Ok((target, index));
            }
            let link = Link {
                parent,
                name: name.into(),
                stamp,
                target: target.clone(),
                active: true,
            };
            let index = if let Some(index) = self.links.iter().position(|link| !link.active) {
                self.links[index] = link;
                index
            } else {
                require(self.links.len() < 8, "asset_operation_failed")?;
                self.links.push(link);
                self.links.len() - 1
            };
            Ok((target, index))
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn symlink(&mut self, parent: Dir, name: &str, target: &str) -> Result<Pointer> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_dir(parent)?;
            require(target.len() <= 128, "asset_public_target")?;
            self.scope
                .sys(|| rustix::fs::symlinkat(target, self.fd(parent), name))?;
            let (read, link) = self.pointer(parent, name)?;
            require(read == target.as_bytes(), "asset_public_target")?;
            Ok(link)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn rename_link(
        &mut self,
        source: Pointer,
        parent: Dir,
        name: &str,
        destination: Option<Pointer>,
        visible: &mut bool,
    ) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            component(name)?;
            self.check_link(source)?;
            self.check_dir(parent)?;
            if let Some(destination) = destination {
                require(
                    self.links[destination].parent == parent
                        && self.links[destination].name == name,
                    "asset_operation_failed",
                )?;
                self.check_link(destination)?;
            } else {
                require(self.absent(parent, name)?, "asset_operation_failed")?;
            }
            let link = &self.links[source];
            let old = link.stamp.clone();
            let target = link.target.clone();
            let from = link.parent;
            let old_name = link.name.clone();
            self.access
                .event("rename-pointer", &self.path(parent, name))?;
            self.check_link(source)?;
            if let Some(destination) = destination {
                self.check_link(destination)?;
            } else {
                require(self.absent(parent, name)?, "asset_operation_failed")?;
            }
            self.scope.sys(|| {
                rustix::fs::renameat(self.fd(from), old_name.as_str(), self.fd(parent), name)
            })?;
            *visible = true;
            if let Some(destination) = destination {
                self.links[destination].active = false;
            }
            self.links[source].parent = parent;
            self.links[source].name = name.into();
            self.access
                .event("renamed-pointer", &self.path(parent, name))?;
            let (stamp, bytes) = self.link_value(parent, name)?;
            require(
                old.dev == stamp.dev
                    && old.ino == stamp.ino
                    && old.mode == stamp.mode
                    && old.uid == stamp.uid
                    && old.gid == stamp.gid
                    && old.size == stamp.size
                    && target == bytes,
                "asset_operation_failed",
            )?;
            self.links[source].stamp = stamp;
            self.check_link(source)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn remove_pointer(&mut self, index: Pointer) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            self.check_link(index)?;
            let link = &self.links[index];
            self.access
                .event("unlink-pointer", &self.path(link.parent, &link.name))?;
            self.check_link(index)?;
            let link = &self.links[index];
            self.scope.sys(|| {
                rustix::fs::unlinkat(self.fd(link.parent), link.name.as_str(), AtFlags::empty())
            })?;
            self.links[index].active = false;
            Ok(())
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
    pub fn remove_dir(&mut self, directory: Dir, depth: usize) -> Result<()> {
        self.scope.check()?;
        let result = (|| {
            require(depth <= 3, "asset_operation_failed")?;
            self.check_dir(directory)?;
            let listed = self.list(directory, 1280)?;
            for name in listed {
                if let Some(entry) = self.records.iter().position(|value| {
                    value.active && value.parent == directory && value.name == name
                }) {
                    self.unlink(entry)?;
                } else if let Some(link) = self.links.iter().position(|value| {
                    value.active && value.parent == directory && value.name == name
                }) {
                    self.remove_pointer(link)?;
                } else if let Some(child) = self.directories.iter().position(|value| {
                    value.file.is_some() && value.parent == Some(directory) && value.name == name
                }) {
                    self.remove_dir(child, depth + 1)?;
                } else {
                    return Err(fail("asset_store_foreign"));
                }
            }
            self.check_dir(directory)?;
            let parent = self.directories[directory]
                .parent
                .ok_or_else(|| fail("asset_operation_failed"))?;
            let name = self.directories[directory].name.clone();
            self.access
                .event("remove-directory", &self.path(parent, &name))?;
            self.check_dir(directory)?;
            self.scope
                .sys(|| rustix::fs::unlinkat(self.fd(parent), name.as_str(), AtFlags::REMOVEDIR))?;
            let file = self.directories[directory].file.take().unwrap();
            self.close_file(file)
        })();
        self.scope.observed(&result);
        result.and_then(|value| {
            self.scope.check()?;
            Ok(value)
        })
    }
}
