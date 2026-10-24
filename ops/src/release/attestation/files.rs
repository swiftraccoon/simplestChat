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
    fn close(mut self, owner: &mut Owner) {
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
        if result.is_err() || synthetic {
            owner.record(Cause::Close);
        }
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
    fn write_chunk(&mut self, bytes: &[u8], offset: u64, owner: &mut Owner) -> Result<()> {
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
    fn seal(&mut self, expected: &str, owner: &mut Owner) -> Result<()> {
        self.check(owner, Window::Body)?;
        control::io(owner, self.handle.file().sync_all())?;
        let stamp = control::io(owner, self.handle.file().metadata())?;
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
