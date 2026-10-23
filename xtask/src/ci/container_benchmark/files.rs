//! Fixed root control namespace, original file bindings and bounded private evidence.
use super::{
    failure,
    io::{Budget, Permit},
    plan::Time,
};
use crate::{
    ci::{
        bounded_log::Log,
        command::{Command, SeparateLogs},
        release_controller::files::Directory,
    },
    tooling::Result,
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat};
use serde::Serialize;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
const FLAGS: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC)
    .union(OFlags::NONBLOCK);
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure(1))
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    (
        a.dev(),
        a.ino(),
        a.mode(),
        a.uid(),
        a.gid(),
        a.nlink(),
        a.len(),
        a.mtime(),
        a.mtime_nsec(),
        a.ctime(),
        a.ctime_nsec(),
    ) == (
        b.dev(),
        b.ino(),
        b.mode(),
        b.uid(),
        b.gid(),
        b.nlink(),
        b.len(),
        b.mtime(),
        b.mtime_nsec(),
        b.ctime(),
        b.ctime_nsec(),
    )
}
fn dir_private(dir: &Directory, uid: u32, gid: u32) -> Result<()> {
    let m = dir.file().metadata().map_err(|_| failure(1))?;
    if !m.is_dir() || m.mode() & 0o7777 != 0o700 || m.uid() != uid || m.gid() != gid {
        return Err(failure(1));
    }
    dir.verify().map_err(|_| failure(1))
}
fn reserve(b: &Budget, path: &Path, extra: usize) -> Result<Permit> {
    b.reserve(path.components().count() + extra)
}
pub(super) fn random<const N: usize>(budget: &Budget) -> Result<[u8; N]> {
    let _permit = budget.reserve(1)?;
    let mut f = File::open("/dev/urandom").map_err(|_| failure(1))?;
    let mut bytes = [0; N];
    let read = f.read_exact(&mut bytes).map_err(|_| failure(1));
    let closed = close(f);
    read?;
    closed?;
    Ok(bytes)
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn bounded_stamp(
    dir: &Directory,
    name: &str,
    max: u64,
    uid: u32,
    modes: &[u32],
) -> Result<Option<(Vec<u8>, Metadata)>> {
    dir.verify().map_err(|_| failure(1))?;
    let gid = dir.file().metadata().map_err(|_| failure(1))?.gid();
    let Some(admitted) = regular_entry(dir, name, max, uid, gid, modes)? else {
        return Ok(None);
    };
    #[cfg(test)]
    record_open();
    let mut f = rustix::fs::openat(dir.file(), name, FLAGS, Mode::empty())
        .map(File::from)
        .map_err(|_| failure(1))?;
    let result = (|| {
        let m = f.metadata().map_err(|_| failure(1))?;
        if !matches_entry(&admitted, &m) {
            return Err(failure(1));
        }
        let mut bytes = vec![0; m.len() as usize];
        f.read_exact(&mut bytes).map_err(|_| failure(1))?;
        if !same(&m, &f.metadata().map_err(|_| failure(1))?) {
            return Err(failure(1));
        }
        dir.check(name, &m).map_err(|_| failure(1))?;
        Ok((bytes, m))
    })();
    let closed = close(f);
    let bytes = result?;
    closed?;
    Ok(Some(bytes))
}
fn regular_entry(
    dir: &Directory,
    name: &str,
    max: u64,
    uid: u32,
    gid: u32,
    modes: &[u32],
) -> Result<Option<Stat>> {
    match rustix::fs::statat(dir.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(s)
            if FileType::from_raw_mode(s.st_mode) == FileType::RegularFile
                && s.st_uid == uid
                && s.st_gid == gid
                && s.st_nlink == 1
                && s.st_size >= 0
                && i128::from(s.st_size) <= i128::from(max)
                && modes
                    .iter()
                    .any(|mode| i128::from(*mode) == i128::from(s.st_mode & 0o7777)) =>
        {
            Ok(Some(s))
        }
        Err(rustix::io::Errno::NOENT) => Ok(None),
        _ => Err(failure(1)),
    }
}
fn matches_entry(s: &Stat, m: &Metadata) -> bool {
    m.is_file()
        && i128::from(s.st_dev) == i128::from(m.dev())
        && i128::from(s.st_ino) == i128::from(m.ino())
        && i128::from(s.st_mode) == i128::from(m.mode())
        && s.st_uid == m.uid()
        && s.st_gid == m.gid()
        && i128::from(s.st_nlink) == i128::from(m.nlink())
        && i128::from(s.st_size) == i128::from(m.len())
}
fn bounded(
    dir: &Directory,
    name: &str,
    max: u64,
    uid: u32,
    modes: &[u32],
) -> Result<Option<Vec<u8>>> {
    bounded_stamp(dir, name, max, uid, modes).map(|v| v.map(|(bytes, _)| bytes))
}
#[cfg(test)]
thread_local! {static FAIL_COPY:RefCell<Option<(u64,u64)>>=const {RefCell::new(None)};}
#[cfg(test)]
thread_local! {
    static WRITE_MODE:RefCell<Option<(u64,u64,Mode)>>=const {RefCell::new(None)};
    static OPENED:RefCell<usize>=const {RefCell::new(0)};
    static REPLACE_ANONYMOUS:RefCell<Option<(u64,u64)>>=const {RefCell::new(None)};
}
#[cfg(test)]
fn record_open() {
    OPENED.with(|v| *v.borrow_mut() += 1);
}
#[cfg(test)]
pub(super) fn opened() -> usize {
    OPENED.with(|v| *v.borrow())
}
#[cfg(test)]
pub(super) fn read_test(dir: &Directory, name: &str) -> Result<Option<Vec<u8>>> {
    let uid = dir.file().metadata().map_err(|_| failure(1))?.uid();
    bounded(dir, name, 16384, uid, &[0o600])
}
#[cfg(test)]
pub(super) fn change_next_write_mode(path: &Path, mode: Mode) {
    let m = std::fs::metadata(path).unwrap();
    WRITE_MODE.with(|v| *v.borrow_mut() = Some((m.dev(), m.ino(), mode)));
}
#[cfg(test)]
pub(super) fn replace_next_anonymous(path: &Path) {
    let m = std::fs::metadata(path).unwrap();
    REPLACE_ANONYMOUS.with(|v| *v.borrow_mut() = Some((m.dev(), m.ino())));
}
#[cfg(test)]
fn anonymous_fault(dir: &Directory, name: &str) -> Result<()> {
    let m = dir.file().metadata().map_err(|_| failure(1))?;
    let replace = REPLACE_ANONYMOUS.with(|v| {
        let mut state = v.borrow_mut();
        if state.as_ref() == Some(&(m.dev(), m.ino())) {
            state.take();
            true
        } else {
            false
        }
    });
    if replace {
        rustix::fs::renameat(dir.file(), name, dir.file(), format!("{name}.original"))
            .map_err(|_| failure(1))?;
        let mut foreign = dir.create(name).map_err(|_| failure(1))?;
        let written = foreign
            .write_all(b"foreign replacement")
            .map_err(|_| failure(1));
        let closed = close(foreign);
        written?;
        closed?;
    }
    Ok(())
}
#[cfg(test)]
fn write_fault(dir: &Directory, file: &File) -> Result<()> {
    let m = dir.file().metadata().map_err(|_| failure(1))?;
    let mode = WRITE_MODE.with(|v| {
        let mut value = v.borrow_mut();
        if value
            .as_ref()
            .is_some_and(|(d, i, _)| *d == m.dev() && *i == m.ino())
        {
            value.take().map(|(_, _, mode)| mode)
        } else {
            None
        }
    });
    if let Some(mode) = mode {
        rustix::fs::fchmod(file, mode).map_err(|_| failure(1))?;
    }
    Ok(())
}
#[cfg(test)]
pub(super) fn fail_next_copy(path: &Path) {
    let m = std::fs::metadata(path).unwrap();
    FAIL_COPY.with(|s| *s.borrow_mut() = Some((m.dev(), m.ino())));
}
#[cfg(test)]
fn copy_fault(dir: &Directory, name: &str) -> bool {
    if !name.starts_with("result-before-emergency-") {
        return false;
    }
    let Ok(m) = dir.file().metadata() else {
        return false;
    };
    FAIL_COPY.with(|s| {
        let matches = s.borrow().as_ref() == Some(&(m.dev(), m.ino()));
        if matches {
            s.borrow_mut().take();
            true
        } else {
            false
        }
    })
}
/// All writes use an original File through rename; no reopened postrename baseline.
fn write(
    dir: &Directory,
    name: &str,
    bytes: &[u8],
    expected: Option<&Metadata>,
) -> Result<Metadata> {
    if bytes.len() > 32 * 1024 * 1024 || name.contains('/') || name.contains('\0') {
        return Err(failure(1));
    }
    dir.verify().map_err(|_| failure(1))?;
    let owner = dir.file().metadata().map_err(|_| failure(1))?;
    match expected {
        Some(m) => dir.check(name, m).map_err(|_| failure(1))?,
        None => absent(dir, name)?,
    }
    let temp = format!("{name}.part");
    let mut f = dir.create(&temp).map_err(|_| failure(1))?;
    let result = (|| {
        let initial = f.metadata().map_err(|_| failure(1))?;
        if !initial.is_file()
            || initial.mode() & 0o7777 != 0o600
            || initial.uid() != owner.uid()
            || initial.gid() != owner.gid()
            || initial.nlink() != 1
            || initial.len() != 0
        {
            return Err(failure(1));
        }
        f.write_all(bytes)
            .and_then(|()| f.sync_all())
            .map_err(|_| failure(1))?;
        #[cfg(test)]
        if copy_fault(dir, name) {
            return Err(failure(1));
        }
        #[cfg(test)]
        write_fault(dir, &f)?;
        let before = f.metadata().map_err(|_| failure(1))?;
        if before.dev() != initial.dev()
            || before.ino() != initial.ino()
            || before.mode() != initial.mode()
            || before.uid() != initial.uid()
            || before.gid() != initial.gid()
            || before.nlink() != initial.nlink()
            || before.len() != bytes.len() as u64
        {
            return Err(failure(1));
        }
        dir.check(&temp, &before).map_err(|_| failure(1))?;
        match expected {
            Some(m) => dir.check(name, m).map_err(|_| failure(1))?,
            None => absent(dir, name)?,
        }
        let flags = if expected.is_some() {
            rustix::fs::RenameFlags::empty()
        } else {
            rustix::fs::RenameFlags::NOREPLACE
        };
        rustix::fs::renameat_with(dir.file(), temp.as_str(), dir.file(), name, flags)
            .map_err(|_| failure(1))?;
        let after = f.metadata().map_err(|_| failure(1))?;
        if after.dev() != before.dev()
            || after.ino() != before.ino()
            || after.mode() != before.mode()
            || after.uid() != before.uid()
            || after.gid() != before.gid()
            || after.nlink() != 1
            || after.len() != before.len()
            || after.mtime() != before.mtime()
            || after.mtime_nsec() != before.mtime_nsec()
        {
            return Err(failure(1));
        }
        dir.check(name, &after).map_err(|_| failure(1))?;
        dir.file().sync_all().map_err(|_| failure(1))?;
        let readback = bounded(dir, name, bytes.len() as u64, after.uid(), &[0o600])?
            .ok_or_else(|| failure(1))?;
        if readback != bytes {
            return Err(failure(1));
        }
        dir.check(name, &after).map_err(|_| failure(1))?;
        Ok(after)
    })();
    let closed = close(f);
    let metadata = result?;
    closed?;
    Ok(metadata)
}
struct Public {
    parent: Directory,
    record: Option<(Directory, Option<Metadata>)>,
    uid: u32,
    gid: u32,
    _permit: Permit,
}
impl Public {
    fn open(path: &Path, uid: u32, gid: u32, budget: &Budget) -> Result<Self> {
        let permit = reserve(budget, path, path.components().count() + 3)?;
        let parent =
            Directory::open(path.parent().ok_or_else(|| failure(1))?).map_err(|_| failure(1))?;
        let mut value = Self {
            parent,
            record: None,
            uid,
            gid,
            _permit: permit,
        };
        let result = (|| {
            let name = path.file_name().ok_or_else(|| failure(1))?;
            match rustix::fs::openat(
                value.parent.file(),
                name,
                FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            ) {
                Ok(fd) => {
                    close(File::from(fd))?;
                }
                Err(rustix::io::Errno::NOENT) => return Ok(()),
                Err(_) => return Err(failure(1)),
            }
            let d = Directory::open(path).map_err(|_| failure(1))?;
            value.record = Some((d, None));
            let d = &value.record.as_ref().unwrap().0;
            dir_private(d, uid, gid)?;
            let bytes = bounded_stamp(d, "release-state.json", 16384, uid, &[0o600])?;
            if let Some((bytes, stamp)) = bytes {
                if bytes.is_empty() {
                    return Err(failure(1));
                }
                let v: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| failure(1))?;
                if v.get("schemaVersion").and_then(|v| v.as_u64()) != Some(1)
                    || v.get("finalized").and_then(|v| v.as_bool()) != Some(true)
                {
                    return Err(failure(1));
                }
                value.record.as_mut().unwrap().1 = Some(stamp);
            }
            Ok(())
        })();
        if let Err(e) = result {
            let _ = value.finish(path);
            return Err(e);
        }
        Ok(value)
    }
    fn check(&self, path: &Path) -> Result<()> {
        self.parent.verify().map_err(|_| failure(1))?;
        match &self.record {
            Some((d, m)) => {
                dir_private(d, self.uid, self.gid)?;
                match m {
                    Some(m) => d.check("release-state.json", m).map_err(|_| failure(1)),
                    None => absent(d, "release-state.json"),
                }
            }
            None => {
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .ok_or_else(|| failure(1))?;
                absent(&self.parent, name)
            }
        }
    }
    fn finish(self, path: &Path) -> Result<()> {
        let check = self.check(path);
        let record = self.record.map(|(d, _)| d.finish()).transpose();
        let parent = self.parent.finish();
        check?;
        record.map_err(|_| failure(1))?;
        parent.map_err(|_| failure(1))
    }
}
pub(super) fn absent(dir: &Directory, name: &str) -> Result<()> {
    dir.verify().map_err(|_| failure(1))?;
    match rustix::fs::statat(dir.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => Ok(()),
        _ => Err(failure(1)),
    }
}
#[derive(Clone)]
pub(super) struct Paths {
    pub state: PathBuf,
    pub public: PathBuf,
    pub uid: u32,
    pub gid: u32,
    pub guest_uid: u32,
    pub guest_gid: u32,
}
impl Paths {
    pub fn fixed() -> Self {
        Self {
            state: "/run/simplestchat-bench".into(),
            public: "/srv/simplestchat-public".into(),
            uid: 0,
            gid: 0,
            guest_uid: 10001,
            guest_gid: 10001,
        }
    }
}
pub(super) struct Guard {
    pub state: Directory,
    lock: File,
    lock_stamp: Metadata,
    public: Option<Public>,
    pub config: PathBuf,
    config_name: String,
    config_dir: Directory,
    paths: Paths,
    current_stamp: RefCell<Current>,
    _permit: Permit,
}
enum Current {
    Unread,
    Absent,
    Present(Metadata),
}
impl Guard {
    pub fn exists(paths: &Paths) -> Result<bool> {
        match std::fs::symlink_metadata(&paths.state) {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(failure(1)),
        }
    }
    pub async fn acquire(
        paths: Paths,
        time: &Time,
        budget: &Budget,
        recovery: bool,
    ) -> Result<Self> {
        time.fence(true)?;
        if recovery && time.elapsed() + Duration::from_secs(1190) > time.entry() {
            return Err(failure(124));
        }
        let config_name = format!("docker-config-{}", hex(&random::<16>(budget)?));
        let permit = reserve(budget, &paths.state, paths.state.components().count() + 8)?;
        let parent = Directory::open(paths.state.parent().ok_or_else(|| failure(1))?)
            .map_err(|_| failure(1))?;
        let name = paths.state.file_name().ok_or_else(|| failure(1))?;
        let created = rustix::fs::mkdirat(parent.file(), name, Mode::from_bits_truncate(0o700));
        let closed = parent.finish();
        if created.is_err_and(|e| e != rustix::io::Errno::EXIST) {
            return Err(failure(1));
        }
        closed.map_err(|_| failure(1))?;
        let state = Directory::open(&paths.state).map_err(|_| failure(1))?;
        let result = (|| {
            dir_private(&state, paths.uid, paths.gid)?;
            let admitted = regular_entry(
                &state,
                "workload.lock",
                u64::MAX,
                paths.uid,
                paths.gid,
                &[0o600],
            )?;
            let flags = if admitted.is_some() {
                OFlags::RDWR
            } else {
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL
            };
            #[cfg(test)]
            record_open();
            let lock = rustix::fs::openat(
                state.file(),
                "workload.lock",
                flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            if let Some(admitted) = admitted {
                let checked = lock.metadata().map_err(|_| failure(1)).and_then(|m| {
                    if matches_entry(&admitted, &m) {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                });
                if let Err(e) = checked {
                    let _ = close(lock);
                    return Err(e);
                }
            }
            Ok(lock)
        })();
        let lock = match result {
            Ok(f) => f,
            Err(e) => {
                let _ = state.finish();
                return Err(e);
            }
        };
        let stamp = lock.metadata().map_err(|_| failure(1));
        let valid = stamp.as_ref().is_ok_and(|m| {
            m.is_file()
                && m.uid() == paths.uid
                && m.gid() == paths.gid
                && m.mode() & 0o7777 == 0o600
                && m.nlink() == 1
        });
        if !valid {
            let _ = close(lock);
            let _ = state.finish();
            return Err(failure(1));
        }
        let lock_start = time.elapsed();
        loop {
            match rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
                Ok(()) => break,
                Err(rustix::io::Errno::WOULDBLOCK)
                    if recovery
                        && time.elapsed().saturating_sub(lock_start) < Duration::from_secs(30) =>
                {
                    let mut clock = time.clone();
                    crate::ci::container_engine::Clock::sleep(
                        &mut clock,
                        Duration::from_millis(100),
                    )
                    .await;
                }
                Err(_) => {
                    let _ = close(lock);
                    let _ = state.finish();
                    return Err(failure(1));
                }
            }
            if let Err(e) = time.fence(true) {
                let _ = close(lock);
                let _ = state.finish();
                return Err(e);
            }
        }
        if let Err(e) = time.fence(true) {
            let _ = close(lock);
            let _ = state.finish();
            return Err(e);
        }
        let public = if recovery {
            None
        } else {
            match Public::open(&paths.public, paths.uid, paths.gid, budget) {
                Ok(p) => Some(p),
                Err(e) => {
                    let _ = close(lock);
                    let _ = state.finish();
                    return Err(e);
                }
            }
        };
        let config = paths.state.join(&config_name);
        let opened = (|| {
            rustix::fs::mkdirat(
                state.file(),
                config_name.as_str(),
                Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| failure(1))?;
            let d = Directory::open(&config).map_err(|_| failure(1))?;
            if let Err(e) = dir_private(&d, paths.uid, paths.gid) {
                let _ = d.finish();
                return Err(e);
            }
            Ok(d)
        })();
        let config_dir = match opened {
            Ok(d) => d,
            Err(e) => {
                let _ = public.map(|p| p.finish(&paths.public)).transpose();
                let _ = close(lock);
                let _ = state.finish();
                return Err(e);
            }
        };
        Ok(Self {
            state,
            lock,
            lock_stamp: stamp.unwrap(),
            public,
            config,
            config_name,
            config_dir,
            paths,
            current_stamp: RefCell::new(Current::Unread),
            _permit: permit,
        })
    }
    pub fn check(&self) -> Result<()> {
        dir_private(&self.state, self.paths.uid, self.paths.gid)?;
        dir_private(&self.config_dir, self.paths.uid, self.paths.gid)?;
        self.state
            .check("workload.lock", &self.lock_stamp)
            .map_err(|_| failure(1))?;
        if !same(
            &self.lock_stamp,
            &self.lock.metadata().map_err(|_| failure(1))?,
        ) {
            return Err(failure(1));
        }
        match &*self.current_stamp.borrow() {
            Current::Present(stamp) => self
                .state
                .check("current.json", stamp)
                .map_err(|_| failure(1))?,
            Current::Absent => absent(&self.state, "current.json")?,
            Current::Unread => {}
        }
        self.public
            .as_ref()
            .map(|p| p.check(&self.paths.public))
            .transpose()
            .map(|_| ())
    }
    pub fn current(&self) -> Result<Option<Vec<u8>>> {
        self.check()?;
        let read = bounded_stamp(&self.state, "current.json", 16384, self.paths.uid, &[0o600])?;
        match read {
            Some((bytes, stamp)) => {
                match &*self.current_stamp.borrow() {
                    Current::Absent => return Err(failure(1)),
                    Current::Present(old) if !same(old, &stamp) => return Err(failure(1)),
                    _ => {}
                }
                *self.current_stamp.borrow_mut() = Current::Present(stamp);
                Ok(Some(bytes))
            }
            None => {
                if matches!(*self.current_stamp.borrow(), Current::Present(_)) {
                    return Err(failure(1));
                }
                *self.current_stamp.borrow_mut() = Current::Absent;
                Ok(None)
            }
        }
    }
    pub fn save(&self, bytes: &[u8]) -> Result<()> {
        if matches!(*self.current_stamp.borrow(), Current::Unread) {
            self.current()?;
        }
        self.check()?;
        let observed = self.current_stamp.borrow();
        let expected = match &*observed {
            Current::Present(stamp) => Some(stamp),
            _ => None,
        };
        let after = write(&self.state, "current.json", bytes, expected)?;
        drop(observed);
        *self.current_stamp.borrow_mut() = Current::Present(after);
        Ok(())
    }
    pub fn finish(self) -> Result<()> {
        let check = self.check();
        let config_stamp = self.config_dir.file().metadata().map_err(|_| failure(1));
        let config_checked = config_stamp.as_ref().map_err(|_| failure(1)).and_then(|m| {
            self.state
                .check(&self.config_name, m)
                .map_err(|_| failure(1))
        });
        let config = self.config_dir.finish();
        let removed = if config_checked.is_ok() && config.is_ok() {
            rustix::fs::unlinkat(
                self.state.file(),
                self.config_name.as_str(),
                AtFlags::REMOVEDIR,
            )
            .map_err(|_| failure(1))
        } else {
            Err(failure(1))
        };
        let public = self
            .public
            .map(|p| p.finish(&self.paths.public))
            .transpose();
        let lock = close(self.lock);
        let state = self.state.finish();
        check?;
        config_checked?;
        removed?;
        config.map_err(|_| failure(1))?;
        public?;
        lock?;
        state.map_err(|_| failure(1))
    }
}
pub(super) struct Output {
    pub directory: Directory,
    pub workload: Directory,
    pub path: PathBuf,
    uid: u32,
    gid: u32,
    guest_uid: u32,
    guest_gid: u32,
    _permit: Permit,
    stamps: RefCell<BTreeMap<String, Metadata>>,
    absent_names: RefCell<BTreeSet<String>>,
}
impl Output {
    pub fn new(path: &Path, paths: &Paths, budget: &Budget, control: &Directory) -> Result<Self> {
        Self::open(path, paths, budget, control, true)
    }
    pub fn reopen(
        path: &Path,
        paths: &Paths,
        budget: &Budget,
        control: &Directory,
    ) -> Result<Self> {
        let output = Self::open(path, paths, budget, control, false)?;
        let admitted = (|| {
            output.read("result.json", 16384)?;
            output.read("cleanup.json", 16384)?;
            for role in super::io::Role::ALL {
                for (suffix, max) in [
                    ("cid", 128),
                    ("removed", 128),
                    ("create-attempted", 32),
                    ("log", super::plan::LOG),
                    ("final-state.json", 4096),
                ] {
                    output.read(&format!("{}.{suffix}", role.name()), max)?;
                }
            }
            Ok(())
        })();
        if let Err(e) = admitted {
            let _ = output.finish();
            return Err(e);
        }
        Ok(output)
    }
    fn open(
        path: &Path,
        paths: &Paths,
        budget: &Budget,
        control: &Directory,
        create: bool,
    ) -> Result<Self> {
        if !super::config::path(path) {
            return Err(failure(1));
        }
        let parent = path
            .parent()
            .ok_or_else(|| failure(1))?
            .canonicalize()
            .map_err(|_| failure(1))?;
        let path = parent.join(path.file_name().ok_or_else(|| failure(1))?);
        if !super::config::path(&path) {
            return Err(failure(1));
        }
        if path.starts_with(&paths.state) || paths.state.starts_with(&path) {
            return Err(failure(1));
        }
        let permit = reserve(budget, &path, 10 + path.components().count())?;
        control.verify().map_err(|_| failure(1))?;
        let control_stamp = control.file().metadata().map_err(|_| failure(1))?;
        let d = Directory::open(&parent).map_err(|_| failure(1))?;
        if d.contains_identity(&control_stamp) {
            let _ = d.finish();
            return Err(failure(1));
        }
        let name = path
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| failure(1))?;
        if create {
            let made = rustix::fs::mkdirat(d.file(), name, Mode::from_bits_truncate(0o700));
            if made.is_err() {
                let _ = d.finish();
                return Err(failure(1));
            }
        }
        let directory = d.descend(name).map_err(|_| failure(1))?;
        let result = (|| {
            let stamp = directory.file().metadata().map_err(|_| failure(1))?;
            if control.contains_identity(&stamp) {
                return Err(failure(1));
            }
            dir_private(&directory, paths.uid, paths.gid)?;
            if create {
                rustix::fs::mkdirat(
                    directory.file(),
                    "workload",
                    Mode::from_bits_truncate(0o700),
                )
                .map_err(|_| failure(1))?;
            }
            let raw = rustix::fs::openat(
                directory.file(),
                "workload",
                FLAGS | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            let admitted = (|| {
                let before = raw.metadata().map_err(|_| failure(1))?;
                if !before.is_dir()
                    || before.mode() & 0o7777 != 0o700
                    || before.uid() != if create { paths.uid } else { paths.guest_uid }
                    || before.gid() != if create { paths.gid } else { paths.guest_gid }
                {
                    return Err(failure(1));
                }
                if create {
                    rustix::fs::fchown(
                        &raw,
                        Some(rustix::fs::Uid::from_raw(paths.guest_uid)),
                        Some(rustix::fs::Gid::from_raw(paths.guest_gid)),
                    )
                    .map_err(|_| failure(1))?;
                }
                let after = raw.metadata().map_err(|_| failure(1))?;
                if after.dev() != before.dev()
                    || after.ino() != before.ino()
                    || after.mode() != before.mode()
                    || after.uid() != paths.guest_uid
                    || after.gid() != paths.guest_gid
                    || after.nlink() != before.nlink()
                {
                    return Err(failure(1));
                }
                directory
                    .check("workload", &after)
                    .map_err(|_| failure(1))?;
                let workload = Directory::open(&path.join("workload")).map_err(|_| failure(1))?;
                let checked = workload
                    .file()
                    .metadata()
                    .map_err(|_| failure(1))
                    .and_then(|m| {
                        if same(&after, &m) {
                            dir_private(&workload, paths.guest_uid, paths.guest_gid)
                        } else {
                            Err(failure(1))
                        }
                    });
                if let Err(e) = checked {
                    let _ = workload.finish();
                    return Err(e);
                }
                Ok(workload)
            })();
            let closed = close(raw);
            match admitted {
                Ok(workload) => {
                    if let Err(e) = closed {
                        let _ = workload.finish();
                        return Err(e);
                    }
                    Ok(workload)
                }
                Err(e) => {
                    let _ = closed;
                    Err(e)
                }
            }
        })();
        let workload = match result {
            Ok(d) => d,
            Err(e) => {
                let _ = directory.finish();
                return Err(e);
            }
        };
        Ok(Self {
            directory,
            workload,
            path,
            uid: paths.uid,
            gid: paths.gid,
            guest_uid: paths.guest_uid,
            guest_gid: paths.guest_gid,
            _permit: permit,
            stamps: RefCell::new(BTreeMap::new()),
            absent_names: RefCell::new(BTreeSet::new()),
        })
    }
    pub fn check(&self) -> Result<()> {
        dir_private(&self.directory, self.uid, self.gid)?;
        dir_private(&self.workload, self.guest_uid, self.guest_gid)?;
        for (name, stamp) in self.stamps.borrow().iter() {
            self.directory.check(name, stamp).map_err(|_| failure(1))?;
        }
        for name in self.absent_names.borrow().iter() {
            absent(&self.directory, name)?;
        }
        Ok(())
    }
    pub fn bytes(&self, name: &str, bytes: &[u8]) -> Result<()> {
        self.check()?;
        let after = write(&self.directory, name, bytes, self.stamps.borrow().get(name))?;
        self.stamps.borrow_mut().insert(name.into(), after);
        self.absent_names.borrow_mut().remove(name);
        Ok(())
    }
    pub fn json(&self, name: &str, value: &impl Serialize) -> Result<()> {
        self.bytes(name, &serde_json::to_vec(value).map_err(|_| failure(1))?)
    }
    pub fn read(&self, name: &str, max: u64) -> Result<Option<Vec<u8>>> {
        self.check()?;
        match bounded_stamp(&self.directory, name, max, self.uid, &[0o600])? {
            Some((bytes, stamp)) => {
                if self.absent_names.borrow().contains(name) {
                    return Err(failure(1));
                }
                if self
                    .stamps
                    .borrow()
                    .get(name)
                    .is_some_and(|old| !same(old, &stamp))
                {
                    return Err(failure(1));
                }
                self.stamps.borrow_mut().insert(name.into(), stamp);
                Ok(Some(bytes))
            }
            None => {
                if self.stamps.borrow().contains_key(name) {
                    Err(failure(1))
                } else {
                    self.absent_names.borrow_mut().insert(name.into());
                    Ok(None)
                }
            }
        }
    }
    pub fn guest_summary(&self) -> Result<Vec<u8>> {
        self.check()?;
        absent(&self.workload, "load_test_timeout.json")?;
        let bytes = bounded(
            &self.workload,
            "load_test_summary.json",
            8 * 1024 * 1024,
            self.guest_uid,
            &[0o600, 0o644],
        )?
        .ok_or_else(|| failure(1))?;
        absent(&self.workload, "load_test_timeout.json")?;
        Ok(bytes)
    }
    pub fn attempted(&self, role: super::io::Role) -> Result<bool> {
        Ok(self
            .read(&format!("{}.create-attempted", role.name()), 32)?
            .is_some())
    }
    pub fn mark(&self, role: super::io::Role) -> Result<()> {
        self.bytes(&format!("{}.create-attempted", role.name()), b"attempted\n")
    }
    pub fn finish(self) -> Result<()> {
        let check = self.check();
        let workload = self.workload.finish();
        let d = self.directory.finish();
        check?;
        workload.map_err(|_| failure(1))?;
        d.map_err(|_| failure(1))
    }
}

struct Reader {
    file: File,
    initial: Metadata,
    _permit: Permit,
}
impl Reader {
    fn bytes(
        mut self,
        maximum: u64,
        seal: Option<&crate::ci::ops_artifact::verify::tools::Seal>,
    ) -> Result<Vec<u8>> {
        let read = (|| {
            let seal = seal.ok_or_else(|| failure(1))?;
            let before = self.file.metadata().map_err(|_| failure(1))?;
            if before.dev() != self.initial.dev()
                || before.ino() != self.initial.ino()
                || before.uid() != self.initial.uid()
                || before.gid() != self.initial.gid()
                || !same(&before, &seal.metadata)
                || before.len() != seal.bytes
                || before.mode() != self.initial.mode()
                || before.nlink() != 0
                || before.len() > maximum
            {
                return Err(failure(1));
            }
            self.file.seek(SeekFrom::Start(0)).map_err(|_| failure(1))?;
            let mut bytes = vec![0; before.len() as usize];
            self.file.read_exact(&mut bytes).map_err(|_| failure(1))?;
            if !same(&before, &self.file.metadata().map_err(|_| failure(1))?) {
                return Err(failure(1));
            }
            use sha2::{Digest, Sha256};
            let hash = hex(&Sha256::digest(&bytes));
            if hash != seal.sha256 {
                return Err(failure(1));
            }
            Ok(bytes)
        })();
        let closed = close(self.file);
        let bytes = read?;
        closed?;
        Ok(bytes)
    }
    fn finish(self) -> Result<()> {
        close(self.file)
    }
}
#[cfg(test)]
pub(super) fn read_sealed_test(
    file: File,
    initial: Metadata,
    permit: Permit,
    maximum: u64,
    seal: &crate::ci::ops_artifact::verify::tools::Seal,
) -> Result<Vec<u8>> {
    Reader {
        file,
        initial,
        _permit: permit,
    }
    .bytes(maximum, Some(seal))
}
fn anonymous(dir: &Directory, budget: &Budget, name: &str) -> Result<(File, Reader)> {
    let permit = budget.reserve(8)?; // log input/output, pipe pair, readers and Native duplicates
    let writer = dir.create(name).map_err(|_| failure(1))?;
    let opened = (|| {
        let m = writer.metadata().map_err(|_| failure(1))?;
        let owner = dir.file().metadata().map_err(|_| failure(1))?;
        if !m.is_file()
            || m.nlink() != 1
            || m.mode() & 0o7777 != 0o600
            || m.len() != 0
            || m.uid() != owner.uid()
            || m.gid() != owner.gid()
        {
            return Err(failure(1));
        }
        dir.check(name, &m).map_err(|_| failure(1))?;
        let reader = rustix::fs::openat(dir.file(), name, FLAGS, Mode::empty())
            .map(File::from)
            .map_err(|_| failure(1))?;
        let observed = reader.metadata().map_err(|_| failure(1));
        if !observed.as_ref().is_ok_and(|v| same(&m, v)) {
            let _ = close(reader);
            return Err(failure(1));
        }
        #[cfg(test)]
        if let Err(e) = anonymous_fault(dir, name) {
            let _ = close(reader);
            return Err(e);
        }
        if dir.check(name, &m).is_err() {
            let _ = close(reader);
            return Err(failure(1));
        }
        let unlinked =
            rustix::fs::unlinkat(dir.file(), name, AtFlags::empty()).map_err(|_| failure(1));
        if let Err(e) = unlinked {
            let _ = close(reader);
            return Err(e);
        }
        let initial = (|| {
            #[cfg(test)]
            write_fault(dir, &writer)?;
            let after = reader.metadata().map_err(|_| failure(1))?;
            if after.dev() != m.dev()
                || after.ino() != m.ino()
                || after.mode() != m.mode()
                || after.uid() != m.uid()
                || after.gid() != m.gid()
                || after.nlink() != 0
                || after.len() != 0
                || after.mtime() != m.mtime()
                || after.mtime_nsec() != m.mtime_nsec()
            {
                return Err(failure(1));
            }
            Ok(after)
        })();
        match initial {
            Ok(initial) => Ok(Reader {
                file: reader,
                initial,
                _permit: permit,
            }),
            Err(e) => {
                let _ = close(reader);
                Err(e)
            }
        }
    })();
    match opened {
        Ok(reader) => Ok((writer, reader)),
        Err(e) => {
            let _ = close(writer);
            Err(e)
        }
    }
}
/// Capture has one bounded anonymous sink per stream. Every admitted peer is
/// explicitly finished if later construction/admission fails.
pub(super) struct Capture<'a, E> {
    pub inner: &'a mut E,
    pub spool: &'a Directory,
    pub budget: &'a Budget,
    pub time: &'a Time,
    pub counter: u64,
    pub settled: bool,
}
impl<E: crate::ci::process::Executor> crate::ci::process::Executor for Capture<'_, E> {
    async fn run(&mut self, mut c: Command) -> Result<Vec<u8>> {
        use crate::ci::process::settle_unforwarded;
        let Some(max) = c.capture.take() else {
            let cleanup = c.cleanup;
            let timeout = c.timeout;
            let grace = c.grace;
            if let Err(e) = self.time.latest(timeout, grace, cleanup) {
                self.settled &= settle_unforwarded(c).await.is_ok();
                return Err(e);
            }
            let result = self.inner.run(c).await;
            if cleanup {
                self.time.settled_slot(timeout, grace);
            }
            return result;
        };
        if c.stdout.is_some() || c.combined_log.is_some() || c.separate_logs.is_some() {
            self.settled &= settle_unforwarded(c).await.is_ok();
            return Err(failure(1));
        }
        self.counter += 1;
        let out = anonymous(
            self.spool,
            self.budget,
            &format!("capture-{}-out", self.counter),
        );
        let (out, read_out) = match out {
            Ok(v) => v,
            Err(e) => {
                self.settled = false;
                let _ = settle_unforwarded(c).await;
                return Err(e);
            }
        };
        let out = match Log::start(out, max) {
            Ok(l) => l,
            Err(e) => {
                self.settled = false;
                let _ = read_out.finish();
                let _ = settle_unforwarded(c).await;
                return Err(e);
            }
        };
        let second = anonymous(
            self.spool,
            self.budget,
            &format!("capture-{}-err", self.counter),
        );
        let (err, read_err) = match second {
            Ok(v) => v,
            Err(e) => {
                self.settled = false;
                let _ = out.finish().await;
                let _ = read_out.finish();
                let _ = settle_unforwarded(c).await;
                return Err(e);
            }
        };
        let err = match Log::start(err, 64 * 1024) {
            Ok(l) => l,
            Err(e) => {
                self.settled = false;
                let _ = out.finish().await;
                let _ = read_out.finish();
                let _ = read_err.finish();
                let _ = settle_unforwarded(c).await;
                return Err(e);
            }
        };
        c.separate_logs = Some(Arc::new(SeparateLogs {
            stdout: out,
            stderr: err,
        }));
        let receipt = Arc::new(super::tools::CaptureReceipt::new());
        c.benchmark_output = Some(receipt.clone());
        let admission = async {
            if !c.cleanup {
                self.inner.ready().await?;
            }
            self.spool.verify().map_err(|_| failure(1))?;
            self.time.latest(c.timeout, c.grace, c.cleanup)?;
            Ok(())
        }
        .await;
        let cleanup = c.cleanup;
        let timeout = c.timeout;
        let grace = c.grace;
        let result = match admission {
            Ok(()) => self.inner.run(c).await,
            Err(e) => {
                self.settled &= settle_unforwarded(c).await.is_ok();
                Err(e)
            }
        };
        if cleanup {
            self.time.settled_slot(timeout, grace);
        }
        let sealed = receipt.get();
        let paired = sealed.as_ref().ok().and_then(|s| s.paired.as_ref());
        let read = read_out.bytes(max, paired.map(|s| &s.stdout));
        let err = read_err.bytes(64 * 1024, paired.map(|s| &s.stderr));
        self.settled &= sealed.is_ok() && read.is_ok() && err.is_ok();
        result?;
        sealed?;
        let bytes = read?;
        err?;
        Ok(bytes)
    }
    async fn ready(&mut self) -> Result<()> {
        self.inner.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.settled && self.inner.cleanup_passed()
    }
    async fn replay(&mut self, b: &[u8], e: bool, d: std::time::Instant) -> Result<()> {
        self.inner.replay(b, e, d).await
    }
}
pub(super) async fn logs<E: crate::ci::process::Executor, C: crate::ci::container_engine::Clock>(
    core: &mut crate::ci::container_engine::Core<'_, E, C>,
    spool: &Directory,
    budget: &Budget,
    id: &str,
    token: &str,
) -> Result<Vec<u8>> {
    let (writer, reader) = anonymous(spool, budget, &format!("role-log-{id}"))?;
    let log = match Log::start(writer, super::plan::LOG) {
        Ok(l) => l,
        Err(e) => {
            let _ = reader.finish();
            return Err(e);
        }
    };
    let mut c = super::plan::command(core.endpoint, &["logs", "--timestamps", id], true);
    c.capture = None;
    c.combined_log = Some(Arc::new(log));
    let receipt = Arc::new(super::tools::CaptureReceipt::new());
    c.benchmark_output = Some(receipt.clone());
    let result = core.run(c).await;
    let sealed = receipt.get();
    let read = reader.bytes(
        super::plan::LOG,
        sealed.as_ref().ok().and_then(|s| s.combined.as_ref()),
    );
    result?;
    sealed?;
    let bytes = read?;
    Ok(redact(&bytes, token.as_bytes()))
}
pub(super) fn redact(bytes: &[u8], token: &[u8]) -> Vec<u8> {
    let mut exact = Vec::with_capacity(bytes.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !token.is_empty() && bytes[cursor..].starts_with(token) {
            exact.extend_from_slice(b"[metrics-token-redacted]");
            cursor += token.len();
        } else {
            exact.push(bytes[cursor]);
            cursor += 1;
        }
    }
    // Recovery intentionally does not retain the secret. Drop every sufficiently
    // long lower-hex run, including tokens embedded in a larger hex string.
    let mut out = Vec::with_capacity(exact.len());
    let mut cursor = 0;
    while cursor < exact.len() {
        if exact[cursor].is_ascii_digit() || (b'a'..=b'f').contains(&exact[cursor]) {
            let start = cursor;
            while cursor < exact.len()
                && (exact[cursor].is_ascii_digit() || (b'a'..=b'f').contains(&exact[cursor]))
            {
                cursor += 1;
            }
            if cursor - start >= 64 {
                out.extend_from_slice(b"[hex-secret-redacted]");
            } else {
                out.extend_from_slice(&exact[start..cursor]);
            }
        } else {
            out.push(exact[cursor]);
            cursor += 1;
        }
    }
    out
}
/// One unlinked, read-only, bounded curl configuration; no token in argv/logs.
pub(super) struct Token {
    file: File,
    stamp: Metadata,
    secret: String,
    _permit: Permit,
}
impl Token {
    pub fn new(spool: &Directory, budget: &Budget, token: &str) -> Result<Self> {
        let name = format!("token-{}", hex(&random::<8>(budget)?));
        let (mut writer, mut reader) = anonymous(spool, budget, &name)?;
        let bytes = format!("header = \"Authorization: Bearer {token}\"\n").into_bytes();
        let written = writer
            .write_all(&bytes)
            .and_then(|()| writer.sync_all())
            .map_err(|_| failure(1));
        let closed = close(writer);
        if let Err(e) = written.and(closed) {
            let _ = reader.finish();
            return Err(e);
        }
        let admitted = (|| {
            let stamp = reader.file.metadata().map_err(|_| failure(1))?;
            if stamp.dev() != reader.initial.dev()
                || stamp.ino() != reader.initial.ino()
                || stamp.mode() != reader.initial.mode()
                || stamp.uid() != reader.initial.uid()
                || stamp.gid() != reader.initial.gid()
                || stamp.nlink() != 0
                || stamp.len() != bytes.len() as u64
            {
                return Err(failure(1));
            }
            reader
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|_| failure(1))?;
            let mut actual = vec![0; bytes.len()];
            reader
                .file
                .read_exact(&mut actual)
                .map_err(|_| failure(1))?;
            if actual != bytes || !same(&stamp, &reader.file.metadata().map_err(|_| failure(1))?) {
                return Err(failure(1));
            }
            reader
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|_| failure(1))?;
            Ok(stamp)
        })();
        let stamp = match admitted {
            Ok(m) => m,
            Err(e) => {
                let _ = reader.finish();
                return Err(e);
            }
        };
        Ok(Self {
            file: reader.file,
            stamp,
            secret: token.into(),
            _permit: reader._permit,
        })
    }
    pub fn safe(&self, bytes: &[u8]) -> Vec<u8> {
        redact(bytes, self.secret.as_bytes())
    }
    pub fn check(&self) -> Result<()> {
        if same(&self.stamp, &self.file.metadata().map_err(|_| failure(1))?) {
            Ok(())
        } else {
            Err(failure(1))
        }
    }
    pub fn input(&mut self) -> Result<File> {
        self.check()?;
        self.file.seek(SeekFrom::Start(0)).map_err(|_| failure(1))?;
        self.file.try_clone().map_err(|_| failure(1))
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let closed = close(self.file);
        checked?;
        closed
    }
}
