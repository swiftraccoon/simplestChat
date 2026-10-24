//! Fixed descriptor-relative installer namespace. Every acquired File is
//! registered before fallible work and receives an explicit close attempt.

use super::{
    Failure, Result,
    control::{Owner, Phase},
    request::{self, Pin},
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, RenameFlags};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const MAX_FILES: usize = 96;
const TEMP: &str = ".simplestchat-ops.installing";

pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| Failure::Unproven)
}
fn sys<T>(value: rustix::io::Result<T>) -> Result<T> {
    value.map_err(|_| Failure::Unproven)
}
fn convert<T: TryInto<U>, U>(value: T) -> Result<U> {
    value.try_into().map_err(|_| Failure::Unproven)
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
#[cfg(test)]
pub(super) fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    identity(a, b)
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn identity(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.is_file() == b.is_file()
        && a.is_dir() == b.is_dir()
}
fn changed_time_only(a: &Metadata, b: &Metadata) -> bool {
    identity(a, b)
        && a.mode() == b.mode()
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec()) == (b.mtime(), b.mtime_nsec())
}

#[cfg(test)]
pub(super) type Hook = std::rc::Rc<dyn Fn(&'static str, &str) -> Result<()>>;
#[cfg(test)]
#[derive(Default)]
pub(super) struct Ledger {
    pub opened: usize,
    pub closed: usize,
    pub live: std::collections::BTreeSet<i32>,
}
#[derive(Clone, Default)]
pub(super) struct Access {
    #[cfg(test)]
    pub root: Option<PathBuf>,
    #[cfg(test)]
    pub hook: Option<Hook>,
    #[cfg(test)]
    pub ledger: Option<std::rc::Rc<std::cell::RefCell<Ledger>>>,
}
impl Access {
    fn opened(&self, file: File) -> File {
        #[cfg(test)]
        if let Some(ledger) = &self.ledger {
            use std::os::fd::AsRawFd;
            let mut ledger = ledger.borrow_mut();
            assert!(ledger.live.insert(file.as_raw_fd()));
            ledger.opened += 1;
        }
        file
    }
    pub fn event(&self, _event: &'static str, _path: &str) -> Result<()> {
        #[cfg(test)]
        if let Some(hook) = &self.hook {
            return hook(_event, _path);
        }
        Ok(())
    }
    pub fn physical(&self, path: &Path) -> PathBuf {
        #[cfg(test)]
        if let Some(root) = &self.root {
            return root.join(path.strip_prefix("/").unwrap());
        }
        path.into()
    }
    fn owner(&self, stamp: &Metadata) -> bool {
        #[cfg(test)]
        if self.root.is_some() {
            return stamp.uid() == rustix::process::getuid().as_raw()
                && stamp.gid() == rustix::process::getgid().as_raw();
        }
        stamp.uid() == 0 && stamp.gid() == 0
    }
    fn close(&self, file: File) -> Result<()> {
        #[cfg(test)]
        if let Some(ledger) = &self.ledger {
            use std::os::fd::AsRawFd;
            let mut ledger = ledger.borrow_mut();
            assert!(ledger.live.remove(&file.as_raw_fd()));
            ledger.closed += 1;
        }
        let result = nix::unistd::close(file).map_err(|_| Failure::Unproven);
        result.and(self.event("close", ""))
    }
}
struct Directory {
    file: Option<File>,
    before: Metadata,
    parent: usize,
    name: String,
    path: String,
}
struct Entry {
    file: Option<File>,
    before: Metadata,
    parent: usize,
    name: String,
    path: PathBuf,
    pin: Pin,
    active: bool,
}
struct Temporary {
    entry: usize,
    published: bool,
    executable: bool,
    copied: bool,
}
pub(super) struct Tree {
    pub access: Access,
    directories: Vec<Directory>,
    entries: Vec<Entry>,
    temporary: Option<Temporary>,
}
impl Tree {
    pub fn open(access: Access) -> Result<Self> {
        let path = access.physical(Path::new("/"));
        let file = access.opened(File::from(sys(rustix::fs::open(
            &path,
            FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        ))?));
        let before = match io(file.metadata()) {
            Ok(value) => value,
            Err(error) => {
                let _ = access.close(file);
                return Err(error);
            }
        };
        if !before.is_dir() || !access.owner(&before) || before.mode() & 0o022 != 0 {
            let _ = access.close(file);
            return Err(Failure::Unproven);
        }
        Ok(Self {
            access,
            directories: vec![Directory {
                file: Some(file),
                before,
                parent: 0,
                name: String::new(),
                path: String::new(),
            }],
            entries: Vec::new(),
            temporary: None,
        })
    }
    fn capacity(&self) -> Result<()> {
        if self.entries.len() + self.directories.len() + 3 > MAX_FILES {
            Err(Failure::Unproven)
        } else {
            Ok(())
        }
    }
    fn dir_file(&self, id: usize) -> &File {
        self.directories[id].file.as_ref().unwrap()
    }
    fn file(&self, id: usize) -> &File {
        self.entries[id].file.as_ref().unwrap()
    }
    fn directory(&mut self, path: &str) -> Result<usize> {
        if let Some(id) = self.directories.iter().position(|d| d.path == path) {
            return Ok(id);
        }
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let parent = self.directory(parent)?;
        self.capacity()?;
        let before = sys(rustix::fs::statat(
            self.dir_file(parent),
            name,
            AtFlags::SYMLINK_NOFOLLOW,
        ))?;
        if FileType::from_raw_mode(before.st_mode) != FileType::Directory {
            return Err(Failure::Unproven);
        }
        let file = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name,
            FLAGS | OFlags::DIRECTORY,
            Mode::empty(),
        ))?));
        let checked = (|| {
            let stamp = io(file.metadata())?;
            if !stamp.is_dir()
                || !self.access.owner(&stamp)
                || stamp.mode() & 0o022 != 0
                || convert::<_, u64>(before.st_dev)? != stamp.dev()
                || convert::<_, u64>(before.st_ino)? != stamp.ino()
            {
                return Err(Failure::Unproven);
            }
            Ok(stamp)
        })();
        let before = match checked {
            Ok(before) => before,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        let id = self.directories.len();
        self.directories.push(Directory {
            file: Some(file),
            before,
            parent,
            name: name.into(),
            path: path.into(),
        });
        Ok(id)
    }
    fn ordinary(&self, parent: usize, name: &str) -> Result<rustix::fs::Stat> {
        let before = sys(rustix::fs::statat(
            self.dir_file(parent),
            name,
            AtFlags::SYMLINK_NOFOLLOW,
        ))?;
        if FileType::from_raw_mode(before.st_mode) != FileType::RegularFile {
            return Err(Failure::Unproven);
        }
        Ok(before)
    }
    fn regular(&self, stamp: &Metadata, mode: u32) -> bool {
        stamp.is_file()
            && self.access.owner(stamp)
            && stamp.nlink() == 1
            && stamp.mode() & 0o7777 == mode
    }
    pub fn input(&mut self, pin: Pin, mode: u32, owner: &mut Owner) -> Result<usize> {
        owner.check(Phase::Body)?;
        request::path(&pin.path)?;
        let logical = pin.path.strip_prefix("/").map_err(|_| Failure::Unproven)?;
        let parent = logical
            .parent()
            .and_then(Path::to_str)
            .ok_or(Failure::Unproven)?;
        let name = logical
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Failure::Unproven)?
            .to_owned();
        let parent = self.directory(parent)?;
        self.capacity()?;
        let admitted = self.ordinary(parent, &name)?;
        let file = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            name.as_str(),
            FLAGS,
            Mode::empty(),
        ))?));
        let checked = (|| {
            let before = io(file.metadata())?;
            if !self.regular(&before, mode)
                || before.len() != pin.bytes
                || convert::<_, u64>(admitted.st_dev)? != before.dev()
                || convert::<_, u64>(admitted.st_ino)? != before.ino()
                || self.entries.iter().any(|e| identity(&e.before, &before))
            {
                return Err(Failure::Unproven);
            }
            Ok(before)
        })();
        let before = match checked {
            Ok(value) => value,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        let path = pin.path.clone();
        let id = self.entries.len();
        self.entries.push(Entry {
            file: Some(file),
            before,
            parent,
            name,
            path,
            pin,
            active: true,
        });
        self.visible(id)?;
        Ok(id)
    }
    pub fn request(
        &mut self,
        path: &Path,
        selected: &str,
        owner: &mut Owner,
    ) -> Result<(usize, Vec<u8>)> {
        request::path(path)?;
        if !request::digest(selected) {
            return Err(Failure::Unproven);
        }
        let logical = path.strip_prefix("/").map_err(|_| Failure::Unproven)?;
        let parent = self.directory(
            logical
                .parent()
                .and_then(Path::to_str)
                .ok_or(Failure::Unproven)?,
        )?;
        let name = logical
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or(Failure::Unproven)?;
        let before = self.ordinary(parent, name)?;
        let bytes = u64::try_from(before.st_size).map_err(|_| Failure::Unproven)?;
        if bytes == 0 || bytes > request::MAXIMUM {
            return Err(Failure::Unproven);
        }
        let id = self.input(
            Pin {
                path: path.into(),
                sha256: selected.into(),
                bytes,
            },
            0o600,
            owner,
        )?;
        let bytes = self.read(id, owner, Phase::Body, true)?;
        Ok((id, bytes))
    }
    pub fn visible(&self, id: usize) -> Result<()> {
        let entry = &self.entries[id];
        self.ordinary(entry.parent, &entry.name)?;
        let file = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(entry.parent),
            entry.name.as_str(),
            FLAGS,
            Mode::empty(),
        ))?));
        let checked = io(file.metadata()).and_then(|stamp| {
            if same(&entry.before, &stamp) {
                Ok(())
            } else {
                Err(Failure::Unproven)
            }
        });
        checked.and(self.access.close(file))
    }
    pub fn read(
        &self,
        id: usize,
        owner: &mut Owner,
        phase: Phase,
        collect: bool,
    ) -> Result<Vec<u8>> {
        let entry = &self.entries[id];
        self.visible(id)?;
        if !same(&entry.before, &io(self.file(id).metadata())?) {
            return Err(Failure::Unproven);
        }
        if collect && entry.pin.bytes > 64 * 1024 {
            return Err(Failure::Unproven);
        }
        let mut reader = self.file(id);
        io(reader.seek(SeekFrom::Start(0)))?;
        let mut left = entry.pin.bytes;
        let mut hash = Sha256::new();
        let mut bytes = Vec::new();
        let mut buffer = [0; 65536];
        while left != 0 {
            owner.check(phase)?;
            self.access.event("read", entry.path.to_str().unwrap())?;
            let count = io(reader.read(&mut buffer[..left.min(65536) as usize]))?;
            if count == 0 {
                return Err(Failure::Unproven);
            }
            left -= count as u64;
            hash.update(&buffer[..count]);
            if collect {
                bytes.extend_from_slice(&buffer[..count]);
            }
        }
        owner.check(phase)?;
        if io(reader.read(&mut buffer[..1]))? != 0
            || hex(&hash.finalize()) != entry.pin.sha256
            || !same(&entry.before, &io(reader.metadata())?)
        {
            return Err(Failure::Unproven);
        }
        self.visible(id)?;
        Ok(bytes)
    }
    pub fn physical(&self, id: usize) -> PathBuf {
        self.access.physical(&self.entries[id].path)
    }
    pub fn destination(&mut self) -> Result<usize> {
        for name in [
            "usr",
            "usr/local",
            "usr/local/libexec",
            request::DESTINATION,
        ] {
            let id = self.directory(name)?;
            if self.directories[id].before.mode() & 0o7777 != 0o755 {
                return Err(Failure::Unproven);
            }
        }
        let destination = self.directory(request::DESTINATION)?;
        let original = &self.directories[destination].before;
        for entry in &self.entries {
            let mut parent = entry.parent;
            loop {
                let candidate = &self.directories[parent].before;
                if original.dev() == candidate.dev() && original.ino() == candidate.ino() {
                    return Err(Failure::Unproven);
                }
                if parent == 0 {
                    break;
                }
                parent = self.directories[parent].parent;
            }
        }
        Ok(destination)
    }
    fn absent(&self, parent: usize, name: &str) -> Result<bool> {
        match rustix::fs::statat(self.dir_file(parent), name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(true),
            Ok(_) => Ok(false),
            Err(_) => Err(Failure::Unproven),
        }
    }
    pub fn existing(&mut self, parent: usize, pin: &Pin, owner: &mut Owner) -> Result<bool> {
        if !self.absent(parent, TEMP)? {
            return Err(Failure::Unproven);
        }
        if self.absent(parent, request::NAME)? {
            return Ok(false);
        }
        // Destination is the only managed input; source path validation above
        // intentionally cannot select it. Admit it directly with the same pin.
        self.capacity()?;
        self.ordinary(parent, request::NAME)?;
        let file = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            request::NAME,
            FLAGS,
            Mode::empty(),
        ))?));
        let before = match io(file.metadata()).and_then(|value| {
            if self.regular(&value, 0o755) && value.len() == pin.bytes {
                Ok(value)
            } else {
                Err(Failure::Unproven)
            }
        }) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        let path = Path::new("/")
            .join(request::DESTINATION)
            .join(request::NAME);
        let mut pin = pin.clone();
        pin.path = path.clone();
        let id = self.entries.len();
        self.entries.push(Entry {
            file: Some(file),
            before,
            parent,
            name: request::NAME.into(),
            path,
            pin,
            active: true,
        });
        self.read(id, owner, Phase::Body, false)?;
        Ok(true)
    }
    pub fn verify_directories(&self) -> Result<()> {
        for (id, entry) in self.directories.iter().enumerate() {
            let stamp = io(self.dir_file(id).metadata())?;
            if !identity(&entry.before, &stamp) || entry.before.mode() != stamp.mode() {
                return Err(Failure::Unproven);
            }
            if id != 0 {
                let file = self.access.opened(File::from(sys(rustix::fs::openat(
                    self.dir_file(entry.parent),
                    entry.name.as_str(),
                    FLAGS | OFlags::DIRECTORY,
                    Mode::empty(),
                ))?));
                let checked = io(file.metadata()).and_then(|stamp| {
                    if identity(&entry.before, &stamp) && stamp.mode() == entry.before.mode() {
                        Ok(())
                    } else {
                        Err(Failure::Unproven)
                    }
                });
                checked.and(self.access.close(file))?;
            }
        }
        Ok(())
    }
    pub fn publish(
        &mut self,
        parent: usize,
        source: usize,
        owner: &mut Owner,
        guard: &crate::release::assets::Workload,
    ) -> Result<()> {
        owner.check(Phase::Body)?;
        self.verify_directories()?;
        if !self.absent(parent, TEMP)? || !self.absent(parent, request::NAME)? {
            return Err(Failure::Unproven);
        }
        self.capacity()?;
        owner.check(Phase::Body)?;
        let file = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(parent),
            TEMP,
            OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        ))?));
        // Register even fchmod/metadata failure as an owned temporary. An
        // unadmitted inode may only be closed, never unlinked by assumption.
        let before = match io(file.metadata()) {
            Ok(value) => value,
            Err(error) => {
                let _ = self.access.close(file);
                return Err(error);
            }
        };
        let id = self.entries.len();
        let mut pin = self.entries[source].pin.clone();
        pin.path = Path::new("/").join(request::DESTINATION).join(TEMP);
        self.entries.push(Entry {
            file: Some(file),
            before,
            parent,
            name: TEMP.into(),
            path: pin.path.clone(),
            pin,
            active: true,
        });
        self.temporary = Some(Temporary {
            entry: id,
            published: false,
            executable: false,
            copied: false,
        });
        self.access.event("created", "")?;
        sys(rustix::fs::fchmod(
            self.file(id),
            Mode::from_bits_truncate(0o600),
        ))?;
        let stamp = io(self.file(id).metadata())?;
        if !identity(&self.entries[id].before, &stamp)
            || !self.regular(&stamp, 0o600)
            || stamp.len() != 0
        {
            return Err(Failure::Unproven);
        }
        self.entries[id].before = stamp;
        self.copy(source, id, owner)?;
        self.temporary.as_mut().unwrap().copied = true;
        self.make_executable(id, owner)?;
        owner.check(Phase::Body)?;
        self.verify_directories()?;
        self.visible(id)?;
        self.access.event("before-publish", "")?;
        self.verify_directories()?;
        self.visible(id)?;
        guard.validate().map_err(|_| Failure::Unproven)?;
        owner.check(Phase::Body)?;
        sys(rustix::fs::renameat_with(
            self.dir_file(parent),
            TEMP,
            self.dir_file(parent),
            request::NAME,
            RenameFlags::NOREPLACE,
        ))?;
        // This publication is known BEFORE every fallible post-rename probe.
        self.temporary.as_mut().unwrap().published = true;
        self.entries[id].name = request::NAME.into();
        self.entries[id].path = Path::new("/")
            .join(request::DESTINATION)
            .join(request::NAME);
        self.access.event("after-publish", "")?;
        let stamp = io(self.file(id).metadata())?;
        if !changed_time_only(&self.entries[id].before, &stamp) {
            return Err(Failure::Unproven);
        }
        self.entries[id].before = stamp;
        self.read(id, owner, Phase::Body, false)?;
        self.access.event("fsync-parent", "")?;
        io(self.dir_file(parent).sync_all())?;
        owner.check(Phase::Body)
    }
    fn copy(&mut self, source: usize, output: usize, owner: &mut Owner) -> Result<()> {
        self.read(source, owner, Phase::Body, false)?;
        let mut reader = self.file(source);
        let mut writer = self.file(output);
        io(reader.seek(SeekFrom::Start(0)))?;
        let mut left = self.entries[source].pin.bytes;
        let mut digest = Sha256::new();
        let mut buffer = [0; 65536];
        while left != 0 {
            owner.check(Phase::Body)?;
            let count = io(reader.read(&mut buffer[..left.min(65536) as usize]))?;
            if count == 0 {
                return Err(Failure::Unproven);
            }
            self.access.event("write", "")?;
            io(writer.write_all(&buffer[..count]))?;
            digest.update(&buffer[..count]);
            left -= count as u64;
        }
        if io(reader.read(&mut buffer[..1]))? != 0
            || hex(&digest.finalize()) != self.entries[source].pin.sha256
            || !same(&self.entries[source].before, &io(reader.metadata())?)
        {
            return Err(Failure::Unproven);
        }
        self.visible(source)?;
        self.access.event("fsync-file", "")?;
        io(writer.sync_all())?;
        let after = io(writer.metadata())?;
        if !identity(&self.entries[output].before, &after)
            || !self.regular(&after, 0o600)
            || after.len() != self.entries[output].pin.bytes
        {
            return Err(Failure::Unproven);
        }
        // Only writes to the original exclusive inode authorize size/mtime/ctime
        // movement. Independent readback below still requires the original pin.
        self.entries[output].before = after;
        self.read(output, owner, Phase::Body, false)?;
        Ok(())
    }
    fn make_executable(&mut self, id: usize, owner: &mut Owner) -> Result<()> {
        let entry = &self.entries[id];
        self.ordinary(entry.parent, &entry.name)?;
        let readonly = self.access.opened(File::from(sys(rustix::fs::openat(
            self.dir_file(entry.parent),
            entry.name.as_str(),
            FLAGS,
            Mode::empty(),
        ))?));
        let checked = io(readonly.metadata()).and_then(|stamp| {
            if same(&entry.before, &stamp) {
                Ok(())
            } else {
                Err(Failure::Unproven)
            }
        });
        if let Err(error) = checked {
            let _ = self.access.close(readonly);
            return Err(error);
        }
        let writer = self.entries[id].file.replace(readonly).unwrap();
        self.access.close(writer)?;
        self.access.event("writers-closed", "")?;
        self.visible(id)?;
        owner.check(Phase::Body)?;
        sys(rustix::fs::fchmod(
            self.file(id),
            Mode::from_bits_truncate(0o755),
        ))?;
        self.temporary.as_mut().unwrap().executable = true;
        self.access.event("after-chmod", "")?;
        let after = io(self.file(id).metadata())?;
        let before = &self.entries[id].before;
        if !identity(before, &after)
            || !self.regular(&after, 0o755)
            || before.len() != after.len()
            || before.nlink() != after.nlink()
            || (before.mtime(), before.mtime_nsec()) != (after.mtime(), after.mtime_nsec())
        {
            return Err(Failure::Unproven);
        }
        self.entries[id].before = after;
        self.read(id, owner, Phase::Body, false)?;
        Ok(())
    }
    fn cleanup_temporary(&mut self) -> Result<()> {
        let Some(temp) = &self.temporary else {
            return Ok(());
        };
        if temp.published {
            return Ok(());
        }
        let id = temp.entry;
        let mode = if temp.executable { 0o755 } else { 0o600 };
        self.verify_directories()?;
        let stamp = io(self.file(id).metadata())?;
        // A failed own write may have made partial progress. Only that bounded
        // size/mtime/ctime effect is allowed, never a new inode/owner/mode/link.
        if !identity(&self.entries[id].before, &stamp)
            || !self.regular(&stamp, mode)
            || stamp.len() > self.entries[id].pin.bytes
        {
            return Err(Failure::Unproven);
        }
        if temp.copied
            && (stamp.len() != self.entries[id].pin.bytes
                || (stamp.mtime(), stamp.mtime_nsec())
                    != (
                        self.entries[id].before.mtime(),
                        self.entries[id].before.mtime_nsec(),
                    ))
        {
            return Err(Failure::Unproven);
        }
        self.entries[id].before = stamp;
        self.visible(id)?;
        self.access.event("before-unlink", "")?;
        self.verify_directories()?;
        self.visible(id)?;
        let entry = &self.entries[id];
        sys(rustix::fs::unlinkat(
            self.dir_file(entry.parent),
            entry.name.as_str(),
            AtFlags::empty(),
        ))?;
        self.entries[id].active = false;
        let after = io(self.file(id).metadata())?;
        let before = &self.entries[id].before;
        if !identity(before, &after)
            || before.mode() != after.mode()
            || before.len() != after.len()
            || after.nlink() != 0
            || (before.mtime(), before.mtime_nsec()) != (after.mtime(), after.mtime_nsec())
        {
            return Err(Failure::Unproven);
        }
        Ok(())
    }
    pub fn finish(mut self, owner: &mut Owner) -> Result<()> {
        let mut primary = self.cleanup_temporary().err();
        for id in 0..self.entries.len() {
            if self.entries[id].active
                && let Err(error) = self.read(id, owner, Phase::Finalize, false)
            {
                primary.get_or_insert(error);
            }
        }
        for (id, entry) in self.entries.iter().enumerate() {
            if entry.active {
                let checked = io(self.file(id).metadata()).and_then(|stamp| {
                    if same(&entry.before, &stamp) {
                        self.visible(id)
                    } else {
                        Err(Failure::Unproven)
                    }
                });
                if let Err(error) = checked {
                    primary.get_or_insert(error);
                }
            }
        }
        if let Err(error) = self.verify_directories() {
            primary.get_or_insert(error);
        }
        for entry in self.entries.iter_mut().rev() {
            if let Some(file) = entry.file.take()
                && let Err(error) = self.access.close(file)
            {
                primary.get_or_insert(error);
            }
        }
        for directory in self.directories.iter_mut().rev() {
            if let Some(file) = directory.file.take()
                && let Err(error) = self.access.close(file)
            {
                primary.get_or_insert(error);
            }
        }
        primary.map_or(Ok(()), Err)
    }
}
