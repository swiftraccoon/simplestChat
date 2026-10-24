//! Fixed-root descriptor-relative files with explicit all-peer finalization.

use super::{Failure, Result, control::Owner};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const MAX_OWNED: usize = 96;
pub(super) type FileId = usize;

pub(super) fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
pub(super) fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| Failure::Io)
}
fn sys<T>(result: rustix::io::Result<T>) -> Result<T> {
    result.map_err(|_| Failure::Io)
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}

#[derive(Clone, Default)]
pub(super) struct Access {
    #[cfg(test)]
    pub root: Option<PathBuf>,
    #[cfg(test)]
    pub hook: Option<std::rc::Rc<dyn Fn(&'static str) -> Result<()>>>,
}
impl Access {
    fn owner(&self, metadata: &Metadata) -> bool {
        #[cfg(test)]
        if self.root.is_some() {
            return metadata.uid() == rustix::process::getuid().as_raw();
        }
        metadata.uid() == 0
    }
    fn event(&self, _name: &'static str) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            return hook(_name);
        }
        Ok(())
    }
}

struct Directory {
    file: Option<File>,
    before: Metadata,
    parent: usize,
    name: String,
    path: String,
    retired: bool,
}
struct Entry {
    file: Option<File>,
    // A taken original File remains owned by this arena's external borrower.
    // It still consumes the descriptor budget and keeps its namespace owners.
    loaned: bool,
    before: Metadata,
    parent: usize,
    name: String,
    retired: bool,
    digest: Option<String>,
    created: bool,
}
pub(super) struct Fs {
    access: Access,
    directories: Vec<Directory>,
    entries: Vec<Entry>,
    paths: BTreeMap<String, usize>,
}

impl Fs {
    pub fn supported_host(&self) -> bool {
        #[cfg(test)]
        if self.access.root.is_some() {
            return true;
        }
        let uname = rustix::system::uname();
        rustix::process::geteuid().as_raw() == 0
            && uname.sysname() == c"Linux"
            && uname.machine() == c"x86_64"
    }
    #[cfg(test)]
    pub fn alias(&mut self, id: FileId) -> Result<FileId> {
        self.capacity()?;
        let entry = &self.entries[id];
        let file = io(self.file(id).try_clone())?;
        self.entries.push(Entry {
            file: Some(file),
            loaned: false,
            before: entry.before.clone(),
            parent: entry.parent,
            name: entry.name.clone(),
            retired: entry.retired,
            digest: entry.digest.clone(),
            created: entry.created,
        });
        Ok(self.entries.len() - 1)
    }
    #[cfg(test)]
    pub fn owned_descriptors(&self) -> Vec<(i32, Metadata)> {
        use std::os::fd::AsRawFd;
        self.entries
            .iter()
            .filter_map(|entry| {
                entry
                    .file
                    .as_ref()
                    .map(|file| (file.as_raw_fd(), entry.before.clone()))
            })
            .chain(self.directories.iter().filter_map(|entry| {
                entry
                    .file
                    .as_ref()
                    .map(|file| (file.as_raw_fd(), entry.before.clone()))
            }))
            .collect()
    }
    pub fn new(access: Access) -> Result<Self> {
        #[cfg(test)]
        let root = access.root.clone().unwrap_or_else(|| PathBuf::from("/"));
        #[cfg(not(test))]
        let root = PathBuf::from("/");
        let file = File::from(sys(rustix::fs::open(
            &root,
            READ | OFlags::DIRECTORY,
            Mode::empty(),
        ))?);
        let before = match io(file.metadata()) {
            Ok(v) => v,
            Err(e) => {
                let _ = nix::unistd::close(file);
                return Err(e);
            }
        };
        if !before.is_dir() || !access.owner(&before) || before.mode() & 0o022 != 0 {
            let _ = nix::unistd::close(file);
            return Err(Failure::Namespace);
        }
        Ok(Self {
            access,
            directories: vec![Directory {
                file: Some(file),
                before,
                parent: 0,
                name: String::new(),
                path: String::new(),
                retired: false,
            }],
            entries: Vec::new(),
            paths: BTreeMap::from([(String::new(), 0)]),
        })
    }
    fn capacity(&self) -> Result<()> {
        let live = self.directories.iter().filter(|v| v.file.is_some()).count()
            + self
                .entries
                .iter()
                .filter(|v| v.file.is_some() || v.loaned)
                .count();
        // Reserve eight slots for signalfd, the bounded spawn pipe set and
        // procfs cleanup observations outside this file arena.
        if live.checked_add(9).is_none_or(|v| v > MAX_OWNED) {
            Err(Failure::Bounds)
        } else {
            Ok(())
        }
    }
    pub fn dir_file(&self, id: usize) -> &File {
        self.directories[id].file.as_ref().unwrap()
    }
    pub fn file(&self, id: FileId) -> &File {
        self.entries[id].file.as_ref().unwrap()
    }
    pub fn file_mut(&mut self, id: FileId) -> &mut File {
        self.entries[id].file.as_mut().unwrap()
    }
    pub fn stamp(&self, id: FileId) -> &Metadata {
        &self.entries[id].before
    }
    pub fn dir_stamp(&self, id: usize) -> Result<Metadata> {
        io(self.dir_file(id).metadata())
    }
    pub fn names(&mut self, id: usize, maximum: usize, owner: &mut Owner) -> Result<Vec<String>> {
        self.check_dir(id)?;
        io(self.directories[id]
            .file
            .as_mut()
            .unwrap()
            .seek(SeekFrom::Start(0)))?;
        let mut result = Vec::new();
        #[cfg(target_os = "linux")]
        {
            let mut buffer = [std::mem::MaybeUninit::uninit(); 8192];
            let mut cursor = rustix::fs::RawDir::new(self.dir_file(id), &mut buffer);
            while let Some(entry) = cursor.next() {
                owner.check(false)?;
                let entry = entry.map_err(|_| Failure::Io)?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                if result.len() >= maximum {
                    return Err(Failure::Bounds);
                }
                result.push(
                    std::str::from_utf8(name)
                        .map_err(|_| Failure::Namespace)?
                        .into(),
                );
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // Production refuses this target. Portable inert fixtures use Dir's
            // internal RAII cursor; its API provides no checked consuming close.
            let cursor = rustix::fs::Dir::read_from(self.dir_file(id)).map_err(|_| Failure::Io)?;
            for entry in cursor {
                owner.check(false)?;
                let entry = entry.map_err(|_| Failure::Io)?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                if result.len() >= maximum {
                    return Err(Failure::Bounds);
                }
                result.push(
                    std::str::from_utf8(name)
                        .map_err(|_| Failure::Namespace)?
                        .into(),
                );
            }
        }
        result.sort();
        self.check_dir(id)?;
        Ok(result)
    }
    fn normalized(path: &str) -> Result<String> {
        let value = Path::new(path);
        if !value.is_absolute()
            || value
                .components()
                .any(|v| !matches!(v, Component::RootDir | Component::Normal(_)))
            || path.len() > 4096
            || value.components().count() > 32
        {
            return Err(Failure::Namespace);
        }
        Ok(path.trim_start_matches('/').to_owned())
    }
    pub fn directory(&mut self, path: &str, private: bool) -> Result<usize> {
        let path = Self::normalized(path)?;
        self.directory_relative(&path, private)
    }
    fn directory_relative(&mut self, path: &str, private: bool) -> Result<usize> {
        if let Some(id) = self.paths.get(path).copied() {
            if self.directories[id].file.is_none() {
                let parent_path = self.directories[self.directories[id].parent].path.clone();
                let parent = self.directory_relative(&parent_path, false)?;
                self.capacity()?;
                let file = File::from(sys(rustix::fs::openat(
                    self.dir_file(parent),
                    self.directories[id].name.as_str(),
                    READ | OFlags::DIRECTORY,
                    Mode::empty(),
                ))?);
                self.directories[id].file = Some(file);
            }
            self.check_dir(id)?;
            if private && self.directories[id].before.mode() & 0o7777 != 0o700 {
                return Err(Failure::Namespace);
            }
            return Ok(id);
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.directory_relative(parent, false)?;
        self.capacity()?;
        let file = File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name,
            READ | OFlags::DIRECTORY,
            Mode::empty(),
        ))?);
        let before = match io(file.metadata()) {
            Ok(v) => v,
            Err(e) => {
                let _ = nix::unistd::close(file);
                return Err(e);
            }
        };
        self.directories.push(Directory {
            file: Some(file),
            before,
            parent,
            name: name.into(),
            path: path.into(),
            retired: false,
        });
        let id = self.directories.len() - 1;
        self.paths.insert(path.into(), id);
        self.check_dir(id)?;
        if private && self.directories[id].before.mode() & 0o7777 != 0o700 {
            return Err(Failure::Namespace);
        }
        Ok(id)
    }
    pub fn check_dir(&self, id: usize) -> Result<()> {
        let value = &self.directories[id];
        let current = io(self.dir_file(id).metadata())?;
        if !current.is_dir()
            || !self.access.owner(&current)
            || current.mode() & 0o022 != 0
            || !binding(&value.before, &current)
        {
            return Err(Failure::Changed);
        }
        if id != 0 && !value.retired {
            let visible = sys(rustix::fs::statat(
                self.dir_file(value.parent),
                value.name.as_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            ))?;
            if visible.st_dev != current.dev()
                || visible.st_ino != current.ino()
                || FileType::from_raw_mode(visible.st_mode) != FileType::Directory
            {
                return Err(Failure::Changed);
            }
        }
        Ok(())
    }
    fn parent(&mut self, path: &str) -> Result<(usize, String)> {
        let path = Self::normalized(path)?;
        let (parent, name) = path.rsplit_once('/').ok_or(Failure::Namespace)?;
        if name.is_empty() || name.len() > 255 {
            return Err(Failure::Namespace);
        }
        Ok((self.directory_relative(parent, false)?, name.into()))
    }
    pub fn absent(&self, parent: usize, name: &str) -> Result<bool> {
        match rustix::fs::statat(self.dir_file(parent), name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(true),
            Ok(_) => Ok(false),
            Err(_) => Err(Failure::Io),
        }
    }
    pub fn exists(&mut self, path: &str) -> Result<bool> {
        let path = Self::normalized(path)?;
        let mut prefix = String::new();
        let mut parent = 0;
        let parts = path.split('/').collect::<Vec<_>>();
        for (index, name) in parts.iter().enumerate() {
            if self.absent(parent, name)? {
                return Ok(false);
            }
            if index + 1 == parts.len() {
                return Ok(true);
            }
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(name);
            parent = self.directory_relative(&prefix, false)?;
        }
        Ok(true)
    }
    pub fn open(&mut self, path: &str, maximum: u64, mode: Option<u32>) -> Result<FileId> {
        let (parent, name) = self.parent(path)?;
        self.capacity()?;
        let file = File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name.as_str(),
            READ,
            Mode::empty(),
        ))?);
        let before = match io(file.metadata()) {
            Ok(v) => v,
            Err(e) => {
                let _ = nix::unistd::close(file);
                return Err(e);
            }
        };
        self.entries.push(Entry {
            file: Some(file),
            loaned: false,
            before,
            parent,
            name,
            retired: false,
            digest: None,
            created: false,
        });
        let id = self.entries.len() - 1;
        self.check(id)?;
        let stamp = self.stamp(id);
        if !stamp.is_file()
            || stamp.len() > maximum
            || stamp.nlink() != 1
            || !self.access.owner(stamp)
            || stamp.mode() & 0o022 != 0
            || mode.is_some_and(|mode| stamp.mode() & 0o7777 != mode)
        {
            return Err(Failure::Namespace);
        }
        Ok(id)
    }
    /// Fixed kernel image handle, intentionally following only /proc/self/exe.
    pub fn executed(&mut self) -> Result<FileId> {
        self.capacity()?;
        let file = File::from(sys(rustix::fs::open(
            "/proc/self/exe",
            OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        ))?);
        let before = match io(file.metadata()) {
            Ok(v) => v,
            Err(e) => {
                let _ = nix::unistd::close(file);
                return Err(e);
            }
        };
        self.entries.push(Entry {
            file: Some(file),
            loaned: false,
            before,
            parent: 0,
            name: String::new(),
            retired: true,
            digest: None,
            created: false,
        });
        let id = self.entries.len() - 1;
        if !self.stamp(id).is_file()
            || self.stamp(id).nlink() != 1
            || self.stamp(id).mode() & 0o7777 != 0o755
            || !self.access.owner(self.stamp(id))
        {
            return Err(Failure::Identity);
        }
        Ok(id)
    }
    pub fn check(&self, id: FileId) -> Result<()> {
        let value = self.entries.get(id).ok_or(Failure::Unproven)?;
        let current = io(value.file.as_ref().ok_or(Failure::Unproven)?.metadata())?;
        if !same(&value.before, &current) {
            return Err(Failure::Changed);
        }
        if !value.retired {
            self.check_dir(value.parent)?;
            let visible = sys(rustix::fs::statat(
                self.dir_file(value.parent),
                value.name.as_str(),
                AtFlags::SYMLINK_NOFOLLOW,
            ))?;
            if visible.st_dev != current.dev()
                || visible.st_ino != current.ino()
                || FileType::from_raw_mode(visible.st_mode) != FileType::RegularFile
            {
                return Err(Failure::Changed);
            }
        }
        Ok(())
    }
    pub fn read(&mut self, id: FileId, maximum: usize, owner: &mut Owner) -> Result<Vec<u8>> {
        self.check(id)?;
        if self.stamp(id).len() > maximum as u64 {
            return Err(Failure::Bounds);
        }
        io(self.file_mut(id).seek(SeekFrom::Start(0)))?;
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 65536];
        loop {
            owner.check(false)?;
            self.check(id)?;
            let count = io(self.file_mut(id).read(&mut buffer))?;
            if count == 0 {
                break;
            }
            if count > maximum.saturating_sub(bytes.len()) {
                return Err(Failure::Bounds);
            }
            bytes.extend_from_slice(&buffer[..count]);
        }
        self.check(id)?;
        if bytes.len() as u64 != self.stamp(id).len() {
            return Err(Failure::Changed);
        }
        self.entries[id].digest = Some(sha(&bytes));
        io(self.file_mut(id).seek(SeekFrom::Start(0)))?;
        Ok(bytes)
    }
    /// Changing fixed kernel records have no immutable size/content witness.
    /// The anchored nofollow open, bounded read and checked close still apply.
    pub fn kernel_record(
        &mut self,
        path: &str,
        maximum: usize,
        owner: &mut Owner,
    ) -> Result<Vec<u8>> {
        let (parent, name) = self.parent(path)?;
        self.capacity()?;
        self.check_dir(parent)?;
        let mut file = File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name.as_str(),
            READ,
            Mode::empty(),
        ))?);
        let body = (|| {
            owner.check(false)?;
            let stamp = io(file.metadata())?;
            if !stamp.is_file() || !self.access.owner(&stamp) || stamp.mode() & 0o022 != 0 {
                return Err(Failure::Namespace);
            }
            let mut bytes = Vec::new();
            io(Read::by_ref(&mut file)
                .take(maximum as u64 + 1)
                .read_to_end(&mut bytes))?;
            owner.check(false)?;
            self.check_dir(parent)?;
            if bytes.len() > maximum {
                return Err(Failure::Bounds);
            }
            Ok(bytes)
        })();
        let closed = nix::unistd::close(file)
            .map_err(|_| Failure::Close)
            .and(self.access.event("close"));
        body.and_then(|bytes| {
            closed?;
            Ok(bytes)
        })
    }
    pub fn hash(&mut self, id: FileId, owner: &mut Owner) -> Result<String> {
        self.check(id)?;
        io(self.file_mut(id).seek(SeekFrom::Start(0)))?;
        let mut count = 0u64;
        let mut hash = Sha256::new();
        let mut buffer = [0u8; 65536];
        loop {
            owner.check(false)?;
            self.check(id)?;
            let n = io(self.file_mut(id).read(&mut buffer))?;
            if n == 0 {
                break;
            }
            count = count.checked_add(n as u64).ok_or(Failure::Bounds)?;
            if count > self.stamp(id).len() {
                return Err(Failure::Changed);
            }
            hash.update(&buffer[..n]);
        }
        if count != self.stamp(id).len() {
            return Err(Failure::Changed);
        }
        self.check(id)?;
        io(self.file_mut(id).seek(SeekFrom::Start(0)))?;
        let hash = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        self.entries[id].digest = Some(hash);
        Ok(self.entries[id].digest.clone().unwrap())
    }
    pub fn mkdir(&mut self, path: &str) -> Result<usize> {
        let (parent, name) = self.parent(path)?;
        if !self.absent(parent, &name)? {
            return Err(Failure::Namespace);
        }
        sys(rustix::fs::mkdirat(
            self.dir_file(parent),
            name.as_str(),
            Mode::from_raw_mode(0o700),
        ))?;
        // Register the owned directory before any reported after-effect error.
        let id = self.directory(path, true)?;
        self.access.event("mkdir")?;
        sys(rustix::fs::fsync(self.dir_file(parent)))?;
        Ok(id)
    }
    /// Exclusively create a private file; the writer closes before readonly readback.
    pub fn create(&mut self, path: &str, bytes: &[u8], owner: &mut Owner) -> Result<FileId> {
        let (parent, _) = self.parent(path)?;
        let writer = self.writer(path)?;
        let written = (|| {
            owner.check(false)?;
            io(self.file_mut(writer).write_all(bytes))?;
            self.access.event("write")?;
            io(self.file(writer).sync_all())?;
            self.access.event("fsync")
        })();
        let stamped = self.written(writer);
        let expected = self.stamp(writer).clone();
        let closed = self.close(writer);
        written?;
        stamped?;
        closed?;
        let id = self.open(path, bytes.len() as u64, Some(0o600))?;
        if !same(&expected, self.stamp(id)) || self.read(id, bytes.len(), owner)? != bytes {
            return Err(Failure::Changed);
        }
        self.entries[id].created = true;
        sys(rustix::fs::fsync(self.dir_file(parent)))?;
        Ok(id)
    }
    pub fn replace(&mut self, path: &str, bytes: &[u8], owner: &mut Owner) -> Result<FileId> {
        let (parent, name) = self.parent(path)?;
        let old = if self.absent(parent, &name)? {
            None
        } else {
            Some(self.open(path, 8 * 1024 * 1024, Some(0o600))?)
        };
        let temporary = format!("{path}.ops-capacity-tmp");
        let id = match self.create(&temporary, bytes, owner) {
            Ok(id) => id,
            Err(error) => {
                // A failed exclusive create can leave our partial temporary.
                // Only a freshly reopened exact recorded inode may be removed.
                let _ = self.cleanup_created(&temporary);
                return Err(error);
            }
        };
        let published = (|| {
            if let Some(old) = old {
                self.check(old)?;
            } else if !self.absent(parent, &name)? {
                return Err(Failure::Changed);
            }
            let original_digest = self.entries[id].digest.clone().ok_or(Failure::Unproven)?;
            if self.hash(id, owner)? != original_digest {
                return Err(Failure::Changed);
            }
            let temp_name = self.entries[id].name.clone();
            self.access.event("before-rename")?;
            sys(rustix::fs::renameat(
                self.dir_file(parent),
                temp_name.as_str(),
                self.dir_file(parent),
                name.as_str(),
            ))?;
            let created = self.entries[id].before.clone();
            let after = io(self.file(id).metadata())?;
            if !binding(&created, &after)
                || created.nlink() != 1
                || after.nlink() != 1
                || created.len() != after.len()
                || (created.mtime(), created.mtime_nsec()) != (after.mtime(), after.mtime_nsec())
            {
                return Err(Failure::Changed);
            }
            // All aliases follow this owned rename. A closed writer retains no
            // stale claim to the temporary name after successful publication.
            for entry in &mut self.entries {
                if entry.before.dev() == created.dev() && entry.before.ino() == created.ino() {
                    entry.name = name.clone();
                    entry.before = after.clone();
                }
            }
            if let Some(old) = old {
                let before = self.entries[old].before.clone();
                let after = io(self.file(old).metadata())?;
                if before.nlink() != 1
                    || after.nlink() != 0
                    || !binding(&before, &after)
                    || before.len() != after.len()
                    || (before.mtime(), before.mtime_nsec()) != (after.mtime(), after.mtime_nsec())
                {
                    return Err(Failure::Changed);
                }
                for entry in &mut self.entries {
                    if entry.before.dev() == before.dev() && entry.before.ino() == before.ino() {
                        entry.retired = true;
                        entry.before = after.clone();
                    }
                }
            }
            if self.hash(id, owner)? != original_digest {
                return Err(Failure::Changed);
            }
            self.access.event("rename")?;
            sys(rustix::fs::fsync(self.dir_file(parent)))?;
            self.check(id)?;
            Ok(id)
        })();
        if published.is_err() {
            let _ = self.cleanup_created(&temporary);
        }
        published
    }
    fn cleanup_created(&mut self, path: &str) -> Result<()> {
        let (parent, name) = self.parent(path)?;
        let expected = self
            .entries
            .iter()
            .rev()
            .find(|entry| {
                entry.parent == parent && entry.name == name && !entry.retired && entry.created
            })
            .map(|entry| entry.before.clone())
            .ok_or(Failure::Ownership)?;
        if self.absent(parent, &name)? {
            return Ok(());
        }
        let id = self.open(path, expected.len(), Some(0o600))?;
        let owned = if same(&expected, self.stamp(id)) {
            self.unlink(id)
        } else {
            Err(Failure::Changed)
        };
        let closed = self.close(id);
        owned.and(closed)
    }
    pub fn close(&mut self, id: FileId) -> Result<()> {
        let file = self.entries[id].file.take().ok_or(Failure::Unproven)?;
        nix::unistd::close(file)
            .map_err(|_| Failure::Close)
            .and(self.access.event("close"))
    }
    /// Release unused archive traversal owners; reopening is an explicit new
    /// anchored open checked against the original directory identity.
    pub fn trim_directories(&mut self) -> Result<()> {
        let mut primary = None;
        for id in (1..self.directories.len()).rev() {
            if self.directories[id].file.is_none()
                || self
                    .entries
                    .iter()
                    .any(|v| (v.loaned || (v.file.is_some() && !v.retired)) && v.parent == id)
                || self
                    .directories
                    .iter()
                    .enumerate()
                    .any(|(child, v)| child != id && v.file.is_some() && v.parent == id)
            {
                continue;
            }
            let checked = self.check_dir(id);
            let file = self.directories[id].file.take().unwrap();
            let closed = nix::unistd::close(file)
                .map_err(|_| Failure::Close)
                .and(self.access.event("close"));
            if primary.is_none() {
                primary = checked.err().or_else(|| closed.err());
            }
        }
        primary.map_or(Ok(()), Err)
    }
    pub fn writer(&mut self, path: &str) -> Result<FileId> {
        let (parent, name) = self.parent(path)?;
        self.capacity()?;
        let file = File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name.as_str(),
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o600),
        ))?);
        let before = match io(file.metadata()) {
            Ok(v) => v,
            Err(error) => {
                let _ = nix::unistd::close(file);
                return Err(error);
            }
        };
        self.entries.push(Entry {
            file: Some(file),
            loaned: false,
            before,
            parent,
            name,
            retired: false,
            digest: None,
            created: true,
        });
        let id = self.entries.len() - 1;
        if self.stamp(id).mode() & 0o7777 != 0o600
            || self.stamp(id).nlink() != 1
            || !self.access.owner(self.stamp(id))
        {
            return Err(Failure::Namespace);
        }
        Ok(id)
    }
    /// Transfer the original File to the archive writer while retaining its
    /// descriptor budget and parent lease. Return it through put before any
    /// successful arena verification or finalization.
    pub fn take(&mut self, id: FileId) -> Result<File> {
        let entry = self.entries.get(id).ok_or(Failure::Unproven)?;
        if entry.loaned || entry.file.is_none() {
            return Err(Failure::Unproven);
        }
        self.check(id)?;
        let entry = &mut self.entries[id];
        let file = entry.file.take().ok_or(Failure::Unproven)?;
        entry.loaned = true;
        Ok(file)
    }
    /// Consume a returned loan, accepting only its original inode/owner/mode.
    /// Rejected incoming files still receive a close attempt; the refusal stays
    /// primary. written separately checks the intended size/time transition.
    pub fn put(&mut self, id: FileId, file: File) -> Result<()> {
        let admitted = (|| {
            let entry = self.entries.get(id).ok_or(Failure::Unproven)?;
            if !entry.loaned || entry.file.is_some() {
                return Err(Failure::Unproven);
            }
            let current = io(file.metadata())?;
            // Archive writes may grow length and change timestamps. The loan
            // can return only the original inode, type, mode and owner binding;
            // written() separately admits the intended completed data change.
            if !binding(&entry.before, &current) {
                return Err(Failure::Changed);
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = nix::unistd::close(file)
                .map_err(|_| Failure::Close)
                .and(self.access.event("close"));
            return Err(error);
        }
        self.entries[id].file = Some(file);
        self.entries[id].loaned = false;
        Ok(())
    }
    pub fn written(&mut self, id: FileId) -> Result<()> {
        let current = io(self
            .entries
            .get(id)
            .and_then(|entry| entry.file.as_ref())
            .ok_or(Failure::Unproven)?
            .metadata())?;
        if !binding(&self.entries[id].before, &current) || current.nlink() != 1 {
            return Err(Failure::Changed);
        }
        self.entries[id].before = current;
        self.check(id)
    }
    pub fn kind(&self, parent: usize, name: &str) -> Result<FileType> {
        Ok(FileType::from_raw_mode(
            sys(rustix::fs::statat(
                self.dir_file(parent),
                name,
                AtFlags::SYMLINK_NOFOLLOW,
            ))?
            .st_mode,
        ))
    }
    pub fn unlink(&mut self, id: FileId) -> Result<()> {
        self.check(id)?;
        let before = self.entries[id].before.clone();
        let parent = self.entries[id].parent;
        let name = self.entries[id].name.clone();
        let removed = rustix::fs::unlinkat(self.dir_file(parent), name.as_str(), AtFlags::empty())
            .map_err(|_| Failure::Io);
        let after = io(self.file(id).metadata());
        let transition = after.and_then(|after| {
            if before.nlink() != 1
                || after.nlink() != 0
                || !binding(&before, &after)
                || before.len() != after.len()
                || before.mtime() != after.mtime()
                || before.mtime_nsec() != after.mtime_nsec()
                || !self.absent(parent, &name)?
            {
                return Err(Failure::Changed);
            }
            // The executed-image descriptor aliases this same inode. Update
            // only this known link/ctime transition for all retained aliases.
            for entry in &mut self.entries {
                if entry.before.dev() == before.dev() && entry.before.ino() == before.ino() {
                    entry.before = after.clone();
                    entry.retired = true;
                }
            }
            Ok(())
        });
        let event = self.access.event("unlink");
        removed.and(transition).and(event)
    }
    pub fn retired(&self, id: FileId) -> bool {
        self.entries[id].retired
    }
    pub fn remove_directory(&mut self, id: usize, owner: &mut Owner) -> Result<()> {
        self.check_dir(id)?;
        if !self.names(id, 1, owner)?.is_empty() {
            return Err(Failure::Namespace);
        }
        let parent = self.directories[id].parent;
        let name = self.directories[id].name.clone();
        let before = io(self.dir_file(id).metadata())?;
        let removed =
            rustix::fs::unlinkat(self.dir_file(parent), name.as_str(), AtFlags::REMOVEDIR)
                .map_err(|_| Failure::Io);
        let transition = io(self.dir_file(id).metadata()).and_then(|after| {
            if !binding(&before, &after) || after.nlink() != 0 || !self.absent(parent, &name)? {
                return Err(Failure::Changed);
            }
            self.directories[id].before = after;
            self.directories[id].retired = true;
            Ok(())
        });
        let event = self.access.event("rmdir");
        removed.and(transition).and(event)
    }
    pub fn verify(&self) -> Result<()> {
        for id in 0..self.directories.len() {
            if self.directories[id].file.is_some() {
                self.check_dir(id)?;
            }
        }
        for id in 0..self.entries.len() {
            if self.entries[id].file.is_some() {
                self.check(id)?;
            }
        }
        if self.entries.iter().any(|entry| entry.loaned) {
            return Err(Failure::Unproven);
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let mut result = self.verify();
        for entry in self.entries.iter_mut().rev() {
            if let Some(file) = entry.file.take() {
                let closed = nix::unistd::close(file)
                    .map_err(|_| Failure::Close)
                    .and(self.access.event("close"));
                if result.is_ok() {
                    result = closed;
                }
            }
        }
        for dir in self.directories.iter_mut().rev() {
            if let Some(file) = dir.file.take() {
                let closed = nix::unistd::close(file)
                    .map_err(|_| Failure::Close)
                    .and(self.access.event("close"));
                if result.is_ok() {
                    result = closed;
                }
            }
        }
        result
    }
}
