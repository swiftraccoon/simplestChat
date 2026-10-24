//! Private retained nofollow custody; no directory membership or authentication claim.

use super::control::{self, Cause, Owner, Result, Window};
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::{Mode, fchmod},
};
use sha2::{Digest, Sha256};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
    path::{Component, Path},
    rc::Rc,
};

const READ: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
fn mapped<T>(owner: &mut Owner, result: nix::Result<T>) -> Result<T> {
    result.map_err(|_| {
        owner.record(Cause::Io);
        Cause::Io
    })
}
#[cfg(test)]
#[derive(Default)]
struct Probe {
    active: bool,
    closes: usize,
    fallback: usize,
    fail: Option<usize>,
    copy: Option<(String, bool, u64)>,
    hash_ready: bool,
    hash_clock: Option<std::time::Instant>,
    seal_mode: Option<String>,
}
#[cfg(test)]
thread_local! {
    static PROBE: std::cell::RefCell<Probe> = const {
        std::cell::RefCell::new(Probe {
            active: false,
            closes: 0,
            fallback: 0,
            fail: None,
            copy: None,
            hash_ready: false,
            hash_clock: None,
            seal_mode: None,
        })
    };
}
#[cfg(test)]
pub(super) fn start_probe(fail: Option<usize>) {
    PROBE.with(|p| {
        *p.borrow_mut() = Probe {
            active: true,
            fail,
            ..Probe::default()
        }
    });
}
#[cfg(test)]
pub(super) fn announce_next_hash() {
    PROBE.with(|p| p.borrow_mut().hash_ready = true);
}
#[cfg(test)]
pub(super) fn expire_hash_at(instant: std::time::Instant) {
    PROBE.with(|p| p.borrow_mut().hash_clock = Some(instant));
}
#[cfg(test)]
pub(super) fn copy_fault(name: String, read: bool, after: u64) {
    PROBE.with(|p| p.borrow_mut().copy = Some((name, read, after)));
}
#[cfg(test)]
fn copy_limit(
    name: &OsStr,
    read: bool,
    offset: u64,
    wanted: usize,
    owner: &mut Owner,
) -> Result<usize> {
    let limit = PROBE.with(|p| {
        p.borrow()
            .copy
            .as_ref()
            .and_then(|(target, reading, after)| {
                (OsStr::new(target) == name && *reading == read).then_some(*after)
            })
    });
    if let Some(limit) = limit {
        if offset >= limit {
            return control::failed(owner, Cause::Io);
        }
        Ok(wanted.min((limit - offset).try_into().unwrap_or(usize::MAX)))
    } else {
        Ok(wanted)
    }
}
#[cfg(test)]
pub(super) fn seal_mode_fault(name: String) {
    PROBE.with(|p| p.borrow_mut().seal_mode = Some(name));
}
#[cfg(test)]
pub(super) fn clear_copy_fault() {
    PROBE.with(|p| p.borrow_mut().copy = None);
}
#[cfg(test)]
pub(super) fn take_probe() -> (usize, usize) {
    PROBE.with(|p| {
        let mut p = p.borrow_mut();
        p.active = false;
        (p.closes, p.fallback)
    })
}
struct Handle(Option<File>);
impl Handle {
    fn new(file: File) -> Self {
        Self(Some(file))
    }
    fn file(&self) -> &File {
        self.0.as_ref().expect("retained descriptor before finish")
    }
    fn close(mut self, owner: &mut Owner) -> bool {
        let result = nix::unistd::close(self.0.take().unwrap());
        #[cfg(test)]
        let synthetic = PROBE.with(|p| {
            let mut p = p.borrow_mut();
            if !p.active {
                return false;
            }
            p.closes += 1;
            p.fail == Some(p.closes)
        });
        #[cfg(not(test))]
        let synthetic = false;
        let closed = result.is_ok() && !synthetic;
        if !closed {
            owner.record(Cause::Close);
        }
        closed
    }
}
impl Drop for Handle {
    fn drop(&mut self) {
        if let Some(file) = self.0.take() {
            #[cfg(test)]
            PROBE.with(|p| {
                let mut p = p.borrow_mut();
                if p.active {
                    p.fallback += 1;
                }
            });
            let _ = nix::unistd::close(file);
        }
    }
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn mismatch(owner: &mut Owner) -> Result<()> {
    control::failed(owner, Cause::Changed)
}
fn leaf(name: &OsStr, owner: &mut Owner) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.as_encoded_bytes().contains(&b'/') {
        control::failed(owner, Cause::Input)
    } else {
        Ok(())
    }
}
struct Edge {
    handle: Handle,
    stamp: Metadata,
    name: Option<OsString>,
}
struct Chain {
    edges: Vec<Edge>,
}
#[derive(Clone)]
pub(super) struct Dir(Rc<Chain>);
impl Dir {
    pub fn open(path: &Path, owner: &mut Owner) -> Result<Self> {
        owner.check(Window::Body)?;
        if !canonical(path) {
            return control::failed(owner, Cause::Input);
        }
        let root = mapped(
            owner,
            open(Path::new("/"), READ | OFlag::O_DIRECTORY, Mode::empty()),
        )?;
        let root = Handle::new(File::from(root));
        let stamp = match control::io(owner, root.file().metadata()) {
            Ok(stamp) => stamp,
            Err(error) => {
                root.close(owner);
                return Err(error);
            }
        };
        let mut edges = vec![Edge {
            handle: root,
            stamp,
            name: None,
        }];
        let opened: Result<()> = (|| {
            for component in path.components() {
                let Component::Normal(name) = component else {
                    continue;
                };
                owner.check(Window::Body)?;
                let descriptor = mapped(
                    owner,
                    openat(
                        edges.last().unwrap().handle.file(),
                        name,
                        READ | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    ),
                )?;
                let child = Handle::new(File::from(descriptor));
                let stamp = match control::io(owner, child.file().metadata()) {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        child.close(owner);
                        return Err(error);
                    }
                };
                edges.push(Edge {
                    handle: child,
                    stamp,
                    name: Some(name.to_owned()),
                });
            }
            Ok(())
        })();
        if let Err(error) = opened {
            for edge in edges.into_iter().rev() {
                edge.handle.close(owner);
            }
            return Err(error);
        }
        let dir = Self(Rc::new(Chain { edges }));
        if let Err(error) = dir.check(owner, Window::Body) {
            dir.finish(owner);
            return Err(error);
        }
        Ok(dir)
    }
    fn file(&self) -> &File {
        self.0.edges.last().unwrap().handle.file()
    }
    pub fn protected(&self, uid: u32, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        let stamp = control::io(owner, self.file().metadata())?;
        if stamp.uid() != uid || stamp.mode() & 0o7777 != 0o700 {
            return control::failed(owner, Cause::Policy);
        }
        Ok(())
    }
    pub fn exists(&self, name: &OsStr, owner: &mut Owner) -> Result<bool> {
        leaf(name, owner)?;
        self.check(owner, Window::Body)?;
        match nix::sys::stat::fstatat(self.file(), name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW) {
            Ok(_) => Ok(true),
            Err(nix::errno::Errno::ENOENT) => Ok(false),
            Err(_) => control::failed(owner, Cause::Io),
        }
    }
    pub fn protected_file(&self, name: &OsStr, uid: u32, owner: &mut Owner) -> Result<()> {
        leaf(name, owner)?;
        self.check(owner, Window::Body)?;
        let stamp = mapped(
            owner,
            nix::sys::stat::fstatat(self.file(), name, nix::fcntl::AtFlags::AT_SYMLINK_NOFOLLOW),
        )?;
        if stamp.st_uid != uid
            || stamp.st_mode & 0o7777 != 0o600
            || stamp.st_mode & nix::sys::stat::SFlag::S_IFMT.bits()
                != nix::sys::stat::SFlag::S_IFREG.bits()
        {
            return control::failed(owner, Cause::Policy);
        }
        self.check(owner, Window::Body)
    }
    pub fn child(&self, name: &OsStr, owner: &mut Owner) -> Result<Self> {
        if self.exists(name, owner)? {
            Self::open(&self.path().join(name), owner)
        } else {
            self.create_child(name, owner)
        }
    }
    pub fn sync(&self, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        control::io(owner, self.file().sync_all())?;
        self.check(owner, Window::Body)
    }
    /// Retire this owned empty CA child; receiver evidence never uses this.
    /// All original descriptors are closed even if unlink or a witness refuses.
    pub fn retire_empty(self, parent: &Self, owner: &mut Owner) -> Result<()> {
        let result = (|| {
            parent.final_namespace(owner)?;
            self.final_namespace(owner)?;
            if self.path().parent() != Some(parent.path().as_path()) {
                return control::failed(owner, Cause::Changed);
            }
            mapped(
                owner,
                nix::unistd::unlinkat(
                    parent.file(),
                    self.path().file_name().ok_or(Cause::Input)?,
                    nix::unistd::UnlinkatFlags::RemoveDir,
                ),
            )
        })();
        match Rc::try_unwrap(self.0) {
            Ok(chain) => {
                for edge in chain.edges.into_iter().rev() {
                    edge.handle.close(owner);
                }
            }
            Err(_) => owner.record(Cause::Close),
        }
        result
    }
    pub fn path(&self) -> std::path::PathBuf {
        let mut path = std::path::PathBuf::from("/");
        for edge in &self.0.edges {
            if let Some(name) = &edge.name {
                path.push(name);
            }
        }
        path
    }
    pub fn create_child(&self, name: &OsStr, owner: &mut Owner) -> Result<Self> {
        leaf(name, owner)?;
        self.check(owner, Window::Body)?;
        mapped(
            owner,
            nix::sys::stat::mkdirat(self.file(), name, Mode::from_bits_truncate(0o700)),
        )?;
        let result = Self::open(&self.path().join(name), owner)?;
        let accepted = (|| {
            self.check(owner, Window::Body)?;
            if self.0.edges.len() + 1 != result.0.edges.len()
                || !self
                    .0
                    .edges
                    .iter()
                    .zip(&result.0.edges)
                    .all(|(a, b)| binding(&a.stamp, &b.stamp))
            {
                return mismatch(owner);
            }
            let stamp = control::io(owner, result.file().metadata())?;
            if stamp.uid() != rustix::process::getuid().as_raw() || stamp.mode() & 0o7777 != 0o700 {
                return mismatch(owner);
            }
            Ok(())
        })();
        if let Err(cause) = accepted {
            result.finish(owner);
            return Err(cause);
        }
        Ok(result)
    }
    /// The curl capath role admits only an empty directory at dispatch.
    pub fn empty(&self, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        let mut visits = 0_u8;
        let mut accept = |name: &[u8], owner: &mut Owner| {
            owner.check(Window::Body)?;
            visits = visits.checked_add(1).ok_or(Cause::Bounds)?;
            if visits > 2 || (name != b"." && name != b"..") {
                return control::failed(owner, Cause::Changed);
            }
            Ok(())
        };
        #[cfg(target_os = "linux")]
        {
            use std::io::{Seek, SeekFrom};
            let mut original = self.file();
            control::io(owner, original.seek(SeekFrom::Start(0)))?;
            let mut buffer = vec![std::mem::MaybeUninit::<u8>::uninit(); 8192];
            let mut cursor = rustix::fs::RawDir::new(original, &mut buffer);
            loop {
                owner.check(Window::Body)?;
                let Some(entry) = cursor.next() else {
                    break;
                };
                let entry = entry.map_err(|_| {
                    owner.record(Cause::Io);
                    Cause::Io
                })?;
                accept(entry.file_name().to_bytes(), owner)?;
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            // The portable rustix cursor owns an opaque RAII-close descriptor;
            // only the original retained File has a checked-close claim.
            let cursor = rustix::fs::Dir::read_from(self.file()).map_err(|_| {
                owner.record(Cause::Io);
                Cause::Io
            })?;
            for entry in cursor {
                let entry = entry.map_err(|_| {
                    owner.record(Cause::Io);
                    Cause::Io
                })?;
                accept(entry.file_name().to_bytes(), owner)?;
            }
        }
        self.check(owner, Window::Body)
    }
    pub fn free_bytes(&self, owner: &mut Owner) -> Result<u64> {
        self.check(owner, Window::Body)?;
        let stat = mapped(owner, nix::sys::statvfs::fstatvfs(self.file()))?;
        u64::try_from(
            u128::from(stat.blocks_available())
                .checked_mul(u128::from(stat.fragment_size()))
                .ok_or(Cause::Bounds)?,
        )
        .map_err(|_| Cause::Bounds)
    }

    pub fn check(&self, owner: &mut Owner, window: Window) -> Result<()> {
        owner.check(window)?;
        for (at, edge) in self.0.edges.iter().enumerate() {
            owner.check(window)?;
            let stamp = control::io(owner, edge.handle.file().metadata())?;
            if !stamp.is_dir() || !binding(&edge.stamp, &stamp) {
                return mismatch(owner);
            }
            if let Some(name) = &edge.name {
                let probe = mapped(
                    owner,
                    openat(
                        self.0.edges[at - 1].handle.file(),
                        name.as_os_str(),
                        READ | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    ),
                )?;
                let probe = Handle::new(File::from(probe));
                let observed = control::io(owner, probe.file().metadata());
                if let Ok(stamp) = &observed
                    && (!stamp.is_dir() || !binding(&edge.stamp, stamp))
                {
                    owner.record(Cause::Changed);
                }
                probe.close(owner);
                observed?;
                owner.check(window)?;
            }
        }
        Ok(())
    }
    /// Final-outcome-only namespace checks remain available after a primary
    /// refusal. They retain the same end/signals and never authorize body work.
    fn final_namespace(&self, owner: &mut Owner) -> Result<()> {
        owner.final_work()?;
        for (at, edge) in self.0.edges.iter().enumerate() {
            owner.final_work()?;
            let stamp = control::io(owner, edge.handle.file().metadata())?;
            if !stamp.is_dir() || !binding(&edge.stamp, &stamp) {
                return mismatch(owner);
            }
            if let Some(name) = &edge.name {
                let fd = mapped(
                    owner,
                    openat(
                        self.0.edges[at - 1].handle.file(),
                        name.as_os_str(),
                        READ | OFlag::O_DIRECTORY,
                        Mode::empty(),
                    ),
                )?;
                let probe = Handle::new(File::from(fd));
                let observed = control::io(owner, probe.file().metadata());
                let matches = observed
                    .as_ref()
                    .is_ok_and(|stamp| stamp.is_dir() && binding(&edge.stamp, stamp));
                if observed.is_ok() && !matches {
                    owner.record(Cause::Changed);
                }
                let closed = probe.close(owner);
                observed?;
                if !closed {
                    return Err(Cause::Close);
                }
                if !matches {
                    return Err(Cause::Changed);
                }
                owner.final_work()?;
            }
        }
        Ok(())
    }
    /// Write one bounded exclusive final outcome, even after a primary refusal.
    /// Every new original descriptor is explicitly closed before returning.
    pub fn final_journal(&self, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        self.journal("outcome.json", bytes, owner)
    }
    pub fn receiver_journal(&self, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        self.journal("fetch-outcome.json", bytes, owner)
    }
    fn journal(&self, name: &str, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        if bytes.is_empty() || bytes.len() > 131072 {
            return control::failed(owner, Cause::Bounds);
        }
        self.final_namespace(owner)?;
        let fd = mapped(
            owner,
            openat(
                self.file(),
                name,
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            ),
        )?;
        let handle = Handle::new(File::from(fd));
        let result = (|| {
            mapped(
                owner,
                fchmod(handle.file(), Mode::from_bits_truncate(0o600)),
            )?;
            let first = control::io(owner, handle.file().metadata())?;
            if !first.is_file()
                || first.len() != 0
                || first.nlink() != 1
                || first.mode() & 0o7777 != 0o600
                || first.uid() != rustix::process::getuid().as_raw()
            {
                return control::failed(owner, Cause::Changed);
            }
            let mut at = 0;
            while at < bytes.len() {
                owner.final_work()?;
                self.final_namespace(owner)?;
                let count = control::io(
                    owner,
                    handle
                        .file()
                        .write_at(&bytes[at..], u64::try_from(at).map_err(|_| Cause::Bounds)?),
                )?;
                if count == 0 {
                    return control::failed(owner, Cause::Io);
                }
                at += count;
                owner.final_work()?;
            }
            control::io(owner, handle.file().sync_all())?;
            owner.final_work()?;
            let stamp = control::io(owner, handle.file().metadata())?;
            if !binding(&first, &stamp)
                || stamp.nlink() != 1
                || stamp.len() != u64::try_from(bytes.len()).map_err(|_| Cause::Bounds)?
            {
                return control::failed(owner, Cause::Changed);
            }
            let mut observed = vec![0; bytes.len() + 1];
            let mut read = 0;
            loop {
                owner.final_work()?;
                let count = control::io(
                    owner,
                    handle.file().read_at(
                        &mut observed[read..],
                        u64::try_from(read).map_err(|_| Cause::Bounds)?,
                    ),
                )?;
                if count == 0 {
                    break;
                }
                read += count;
                if read == observed.len() {
                    break;
                }
            }
            if observed.get(..read) != Some(bytes) {
                return control::failed(owner, Cause::Changed);
            }
            self.final_namespace(owner)?;
            let fd = mapped(owner, openat(self.file(), name, READ, Mode::empty()))?;
            let probe = Handle::new(File::from(fd));
            let readback = control::io(owner, probe.file().metadata());
            let matched = readback.as_ref().is_ok_and(|value| same(&stamp, value));
            if readback.is_ok() && !matched {
                owner.record(Cause::Changed);
            }
            let closed = probe.close(owner);
            readback?;
            if !closed {
                return Err(Cause::Close);
            }
            if !matched {
                return Err(Cause::Changed);
            }
            owner.final_work()
        })();
        if let Err(cause) = &result {
            owner.record(*cause);
        }
        let closed = handle.close(owner);
        if !closed && result.is_ok() {
            Err(Cause::Close)
        } else {
            result
        }
    }
    pub fn final_sync(&self, owner: &mut Owner) -> Result<()> {
        self.final_namespace(owner)?;
        control::io(owner, self.file().sync_all())?;
        owner.final_work()
    }
    pub fn finish(self, owner: &mut Owner) {
        let _ = self.check(owner, Window::Final);
        match Rc::try_unwrap(self.0) {
            Ok(chain) => {
                for edge in chain.edges.into_iter().rev() {
                    edge.handle.close(owner);
                }
            }
            Err(_) => owner.record(Cause::Close),
        }
    }
}
struct Witness {
    directory: Dir,
    name: OsString,
    stamp: Metadata,
}
impl Witness {
    fn namespace(&self, owner: &mut Owner, window: Window, growing: bool) -> Result<()> {
        self.directory.check(owner, window)?;
        let probe = mapped(
            owner,
            openat(
                self.directory.file(),
                self.name.as_os_str(),
                READ,
                Mode::empty(),
            ),
        )?;
        let probe = Handle::new(File::from(probe));
        let observed = control::io(owner, probe.file().metadata());
        if let Ok(stamp) = &observed {
            let valid = if growing {
                binding(&self.stamp, stamp) && self.stamp.nlink() == stamp.nlink()
            } else {
                same(&self.stamp, stamp)
            };
            if !stamp.is_file() || !valid {
                owner.record(Cause::Changed);
            }
        }
        probe.close(owner);
        observed?;
        owner.check(window)
    }
    fn check(&self, file: &File, owner: &mut Owner, window: Window, growing: bool) -> Result<()> {
        owner.check(window)?;
        let observed = control::io(owner, file.metadata())?;
        let valid = if growing {
            binding(&self.stamp, &observed) && self.stamp.nlink() == observed.nlink()
        } else {
            same(&self.stamp, &observed)
        };
        if !observed.is_file() || !valid {
            return mismatch(owner);
        }
        self.namespace(owner, window, growing)
    }
}
pub(super) fn canonical(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
        && path
            .components()
            .collect::<std::path::PathBuf>()
            .as_os_str()
            == path.as_os_str()
}
pub(super) struct Retained {
    handle: Handle,
    witness: Witness,
    created: bool,
    sealed: bool,
}
impl Retained {
    pub fn open(directory: Dir, name: impl AsRef<OsStr>, owner: &mut Owner) -> Result<Self> {
        let name = name.as_ref();
        leaf(name, owner)?;
        directory.check(owner, Window::Body)?;
        let descriptor = mapped(owner, openat(directory.file(), name, READ, Mode::empty()))?;
        let handle = Handle::new(File::from(descriptor));
        let stamp = match control::io(owner, handle.file().metadata()) {
            Ok(stamp) if stamp.is_file() => stamp,
            Ok(_) => {
                owner.record(Cause::Input);
                handle.close(owner);
                return Err(Cause::Input);
            }
            Err(error) => {
                handle.close(owner);
                return Err(error);
            }
        };
        Ok(Self {
            handle,
            witness: Witness {
                directory,
                name: name.to_owned(),
                stamp,
            },
            created: false,
            sealed: true,
        })
    }
    pub fn create(directory: Dir, name: &str, owner: &mut Owner) -> Result<Self> {
        leaf(OsStr::new(name), owner)?;
        directory.check(owner, Window::Body)?;
        let descriptor = mapped(
            owner,
            openat(
                directory.file(),
                name,
                OFlag::O_RDWR
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            ),
        )?;
        let handle = Handle::new(File::from(descriptor));
        let admitted: Result<Metadata> = (|| {
            let original = control::io(owner, handle.file().metadata())?;
            mapped(
                owner,
                fchmod(handle.file(), Mode::from_bits_truncate(0o600)),
            )?;
            let stamp = control::io(owner, handle.file().metadata())?;
            if !stamp.is_file()
                || stamp.len() != 0
                || stamp.nlink() != 1
                || stamp.uid() != rustix::process::getuid().as_raw()
                || stamp.mode() & 0o7777 != 0o600
                || original.dev() != stamp.dev()
                || original.ino() != stamp.ino()
            {
                owner.record(Cause::Changed);
                return Err(Cause::Changed);
            }
            Ok(stamp)
        })();
        match admitted {
            Ok(stamp) => Ok(Self {
                handle,
                witness: Witness {
                    directory,
                    name: name.into(),
                    stamp,
                },
                created: true,
                sealed: false,
            }),
            Err(error) => {
                handle.close(owner);
                Err(error)
            }
        }
    }
    pub fn file(&self) -> &File {
        self.handle.file()
    }
    pub fn protected(&self, uid: u32, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        let stamp = control::io(owner, self.file().metadata())?;
        if stamp.uid() != uid || stamp.mode() & 0o7777 != 0o600 {
            return control::failed(owner, Cause::Policy);
        }
        Ok(())
    }
    pub fn lock(&self, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        rustix::fs::flock(
            self.file(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        self.check(owner, Window::Body)
    }
    /// Accept only the exact verified target alias of this original source.
    /// A foreign equal-byte inode never changes the source's metadata witness.
    pub fn publication_alias(&mut self, target: &Self, owner: &mut Owner) -> Result<()> {
        target.check(owner, Window::Body)?;
        let current = control::io(owner, self.file().metadata())?;
        let alias = control::io(owner, target.file().metadata())?;
        if current.dev() != alias.dev() || current.ino() != alias.ino() {
            return self.check(owner, Window::Body);
        }
        let before = &self.witness.stamp;
        if !binding(before, &current)
            || current.len() != before.len()
            || (current.mtime(), current.mtime_nsec()) != (before.mtime(), before.mtime_nsec())
            || !(same(before, &current)
                || current.nlink() == before.nlink().checked_add(1).ok_or(Cause::Bounds)?)
        {
            return mismatch(owner);
        }
        self.witness.stamp = current;
        self.check(owner, Window::Body)
    }
    /// Retain publication after a successful own hardlink and its exact nlink
    /// transition. Existing equal-byte files never enter this ownership path.
    pub fn publish(&mut self, destination: &Dir, owner: &mut Owner) -> Result<bool> {
        self.check(owner, Window::Body)?;
        destination.check(owner, Window::Body)?;
        match nix::unistd::linkat(
            self.witness.directory.file(),
            self.name(),
            destination.file(),
            self.name(),
            nix::fcntl::AtFlags::empty(),
        ) {
            Ok(()) => {
                let stamp = control::io(owner, self.file().metadata())?;
                let before = &self.witness.stamp;
                if !binding(before, &stamp)
                    || stamp.len() != before.len()
                    || stamp.nlink() != before.nlink().checked_add(1).ok_or(Cause::Bounds)?
                    || (stamp.mtime(), stamp.mtime_nsec()) != (before.mtime(), before.mtime_nsec())
                {
                    return mismatch(owner).map(|()| false);
                }
                self.witness.stamp = stamp;
                self.check(owner, Window::Body)?;
                Ok(true)
            }
            Err(nix::errno::Errno::EEXIST) => Ok(false),
            Err(_) => control::failed(owner, Cause::Io),
        }
    }
    pub fn name(&self) -> &OsStr {
        &self.witness.name
    }
    pub fn len(&self) -> u64 {
        self.witness.stamp.len()
    }
    pub fn bound(&self, maximum: u64, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        if self.len() == 0 || self.len() > maximum {
            control::failed(owner, Cause::Policy)
        } else {
            Ok(())
        }
    }
    pub fn check(&self, owner: &mut Owner, window: Window) -> Result<()> {
        self.witness
            .check(self.handle.file(), owner, window, !self.sealed)
    }
    pub fn hash(&self, owner: &mut Owner) -> Result<String> {
        Ok(self.read_hash(false, owner)?.0)
    }
    pub fn bytes(&self, maximum: usize, owner: &mut Owner) -> Result<Vec<u8>> {
        self.bound(maximum.try_into().map_err(|_| Cause::Input)?, owner)?;
        Ok(self.read_hash(true, owner)?.1)
    }
    fn read_hash(&self, collect: bool, owner: &mut Owner) -> Result<(String, Vec<u8>)> {
        if !self.sealed {
            return control::failed(owner, Cause::Changed);
        }
        let mut buffer = vec![0_u8; 65536];
        let mut bytes = Vec::new();
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        loop {
            self.check(owner, Window::Body)?;
            let size = control::io(owner, self.handle.file().read_at(&mut buffer, count))?;
            #[cfg(test)]
            if let Some(instant) = PROBE.with(|p| p.borrow_mut().hash_clock.take()) {
                owner.observe_at(instant);
            }
            self.check(owner, Window::Body)?;
            if size == 0 {
                break;
            }
            count = count
                .checked_add(size.try_into().map_err(|_| Cause::Input)?)
                .ok_or(Cause::Input)?;
            if count > self.len() {
                return control::failed(owner, Cause::Changed);
            }
            digest.update(&buffer[..size]);
            #[cfg(test)]
            if PROBE.with(|p| std::mem::take(&mut p.borrow_mut().hash_ready)) {
                control::io(owner, super::prepare::stdout(b"PREPARE-HASH-READY\n"))?;
            }
            if collect {
                bytes.try_reserve(size).map_err(|_| {
                    owner.record(Cause::Io);
                    Cause::Io
                })?;
                bytes.extend_from_slice(&buffer[..size]);
            }
        }
        if count != self.len() {
            return control::failed(owner, Cause::Changed);
        }
        Ok((hex(&digest.finalize()), bytes))
    }
    pub fn write_chunk(&mut self, bytes: &[u8], offset: u64, owner: &mut Owner) -> Result<()> {
        if !self.created || self.sealed {
            return control::failed(owner, Cause::Changed);
        }
        let mut at = 0;
        while at < bytes.len() {
            self.check(owner, Window::Body)?;
            let position = offset
                .checked_add(at.try_into().map_err(|_| Cause::Input)?)
                .ok_or(Cause::Input)?;
            #[cfg(test)]
            let wanted = copy_limit(self.name(), false, position, bytes.len() - at, owner)?;
            #[cfg(not(test))]
            let wanted = bytes.len() - at;
            let count = control::io(
                owner,
                self.handle
                    .file()
                    .write_at(&bytes[at..at + wanted], position),
            )?;
            if count == 0 {
                return control::failed(owner, Cause::Io);
            }
            at += count;
            self.check(owner, Window::Body)?;
        }
        Ok(())
    }
    pub fn seal(&mut self, expected: &str, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        control::io(owner, self.handle.file().sync_all())?;
        #[cfg(test)]
        if PROBE.with(|p| {
            let mut p = p.borrow_mut();
            if p.seal_mode
                .as_ref()
                .is_some_and(|name| OsStr::new(name) == self.name())
            {
                p.seal_mode.take().is_some()
            } else {
                false
            }
        }) {
            mapped(owner, fchmod(self.file(), Mode::from_bits_truncate(0o644)))?;
        }
        let stamp = control::io(owner, self.handle.file().metadata())?;
        if !stamp.is_file()
            || !binding(&self.witness.stamp, &stamp)
            || stamp.nlink() != self.witness.stamp.nlink()
        {
            return mismatch(owner);
        }
        self.witness.stamp = stamp;
        self.sealed = true;
        let actual = self.hash(owner)?;
        if actual != expected {
            return control::failed(owner, Cause::Changed);
        }
        Ok(())
    }
    pub fn write(&mut self, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        self.write_chunk(bytes, 0, owner)?;
        self.seal(&hex(&Sha256::digest(bytes)), owner)
    }
    pub fn copy(&self, output: &mut Self, owner: &mut Owner) -> Result<()> {
        let mut buffer = vec![0_u8; 65536];
        let mut digest = Sha256::new();
        let mut count = 0_u64;
        loop {
            self.check(owner, Window::Body)?;
            #[cfg(test)]
            let wanted = copy_limit(output.name(), true, count, buffer.len(), owner)?;
            #[cfg(not(test))]
            let wanted = buffer.len();
            let size = control::io(
                owner,
                self.handle.file().read_at(&mut buffer[..wanted], count),
            )?;
            self.check(owner, Window::Body)?;
            if size == 0 {
                break;
            }
            let next = count
                .checked_add(size.try_into().map_err(|_| Cause::Input)?)
                .ok_or(Cause::Input)?;
            if next > self.len() {
                return control::failed(owner, Cause::Changed);
            }
            output.write_chunk(&buffer[..size], count, owner)?;
            digest.update(&buffer[..size]);
            count = next;
        }
        if count != self.len() {
            return control::failed(owner, Cause::Changed);
        }
        output.seal(&hex(&digest.finalize()), owner)
    }
    pub fn archive(
        &mut self,
        manifest: &crate::release::manifest::Manifest,
        owner: &mut Owner,
    ) -> Result<crate::release::archive::VerifiedArchive> {
        self.check(owner, Window::Body)?;
        let witness = &self.witness;
        let file = self.handle.0.as_mut().unwrap();
        let result = crate::release::archive::verify_retained_archive(file, manifest, || {
            witness.namespace(owner, Window::Body, false).is_ok()
        });
        match result {
            Ok(value) => {
                self.check(owner, Window::Body)?;
                Ok(value)
            }
            Err(_) => {
                owner.record(Cause::Archive);
                Err(owner.error().unwrap())
            }
        }
    }
    pub fn finish(self, owner: &mut Owner) {
        let _ = self.check(owner, Window::Final);
        self.handle.close(owner);
    }
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
