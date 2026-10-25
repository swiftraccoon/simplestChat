//! Deployment generations and sealed private evidence; never daemon authority.

use super::super::{
    control::{self, Cause, Owner, Result, Window},
    files::{Dir, hex},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    os::unix::fs::{FileExt, MetadataExt},
};

#[cfg(test)]
pub(in crate::release::attestation) fn start_probe() {
    super::files::start_probe();
}
#[cfg(test)]
pub(in crate::release::attestation) fn take_probe() -> (usize, usize) {
    super::files::take_probe()
}

pub(super) fn check(owner: &mut Owner, recovery: bool) -> Result<()> {
    if recovery {
        owner.final_work()
    } else {
        owner.check(Window::Body)
    }
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.is_file()
        && b.is_file()
        && a.dev() == b.dev()
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
fn absent(parent: &Dir, name: &str, owner: &mut Owner, recovery: bool) -> Result<()> {
    check(owner, recovery)?;
    match rustix::fs::statat(parent.file(), name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
        Err(rustix::io::Errno::NOENT) => check(owner, recovery),
        Ok(_) => control::failed(owner, Cause::Changed),
        Err(_) => control::failed(owner, Cause::Io),
    }
}
#[cfg(test)]
pub(super) const FOREIGN_AFTER_TEMP: u8 = 3;
fn close(file: File, owner: &mut Owner) -> Result<()> {
    let result = nix::unistd::close(file);
    #[cfg(test)]
    let synthetic = super::files::closed();
    #[cfg(not(test))]
    let synthetic = false;
    if result.is_err() || synthetic {
        owner.record(Cause::Close);
        Err(Cause::Close)
    } else {
        Ok(())
    }
}
fn digest(file: &File, maximum: u64, owner: &mut Owner, recovery: bool) -> Result<(u64, String)> {
    let mut buffer = vec![0; 65536];
    let mut offset = 0_u64;
    let mut hash = Sha256::new();
    loop {
        check(owner, recovery)?;
        #[cfg(test)]
        super::files::digest_read();
        let count = match file.read_at(&mut buffer, offset) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => control::io(owner, value)?,
        };
        check(owner, recovery)?;
        if count == 0 {
            break;
        }
        offset = offset.checked_add(count as u64).ok_or(Cause::Bounds)?;
        if offset > maximum {
            return control::failed(owner, Cause::Bounds);
        }
        hash.update(&buffer[..count]);
    }
    Ok((offset, hex(&hash.finalize())))
}

#[derive(Clone, Copy)]
enum Protection {
    Private,
    Tool,
    Caddy,
    RestoreSql,
    RestoreInput,
}
struct Admission<'a> {
    uid: u32,
    maximum: u64,
    protection: Protection,
    expected: Option<&'a Metadata>,
}

pub(in crate::release::attestation) struct Record {
    file: Option<File>,
    parent: Dir,
    name: String,
    stamp: Metadata,
    sha256: Option<String>,
    visible: bool,
}
impl Record {
    pub fn file(&self) -> &File {
        self.file.as_ref().expect("owned record before finish")
    }
    pub fn len(&self) -> u64 {
        self.stamp.len()
    }
    pub fn path(&self) -> std::path::PathBuf {
        self.parent.path().join(&self.name)
    }
    pub fn open(
        parent: Dir,
        name: &str,
        uid: u32,
        maximum: u64,
        owner: &mut Owner,
    ) -> Result<Self> {
        Self::open_in(
            parent,
            name,
            owner,
            false,
            Admission {
                uid,
                maximum,
                protection: Protection::Private,
                expected: None,
            },
        )
    }
    pub fn tool(parent: Dir, name: &str, uid: u32, owner: &mut Owner) -> Result<Self> {
        Self::open_in(
            parent,
            name,
            owner,
            false,
            Admission {
                uid,
                maximum: 256 * 1024 * 1024,
                protection: Protection::Tool,
                expected: None,
            },
        )
    }
    /// Only the fixed readonly public proxy file has this admission profile.
    pub fn caddy(parent: Dir, uid: u32, owner: &mut Owner) -> Result<Self> {
        Self::open_in(
            parent,
            "Caddyfile",
            owner,
            false,
            Admission {
                uid,
                maximum: u64::MAX,
                protection: Protection::Caddy,
                expected: None,
            },
        )
    }
    /// Restore's readonly reference inputs allow existing positive link counts.
    /// Private writers and all other operations retain their one-link rule.
    pub fn restore_input(
        parent: Dir,
        name: &str,
        uid: u32,
        maximum: u64,
        owner: &mut Owner,
    ) -> Result<Self> {
        Self::open_in(
            parent,
            name,
            owner,
            false,
            Admission {
                uid,
                maximum,
                protection: Protection::RestoreInput,
                expected: None,
            },
        )
    }
    /// Remove only this original name while retaining the anonymous input owner.
    pub fn unname(&mut self, owner: &mut Owner) -> Result<()> {
        self.verify(owner, false)?;
        rustix::fs::unlinkat(
            self.parent.file(),
            self.name.as_str(),
            rustix::fs::AtFlags::empty(),
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        self.displaced(owner)?;
        self.verify(owner, false)
    }
    /// Only the fixed installed restore SQL has this readonly public profile.
    pub fn restore_sql(parent: Dir, uid: u32, owner: &mut Owner) -> Result<Self> {
        Self::open_in(
            parent,
            "restore-verify.sql",
            owner,
            false,
            Admission {
                uid,
                maximum: u64::MAX,
                protection: Protection::RestoreSql,
                expected: None,
            },
        )
    }
    /// Retire exactly this visible original; retain checked close even on refusal.
    pub fn retire(mut self, owner: &mut Owner, recovery: bool) -> Result<()> {
        let result = (|| {
            self.verify(owner, recovery)?;
            rustix::fs::unlinkat(
                self.parent.file(),
                self.name.as_str(),
                rustix::fs::AtFlags::empty(),
            )
            .map_err(|_| {
                owner.record(Cause::Io);
                Cause::Io
            })?;
            self.displaced(owner)?;
            check(owner, recovery)
        })();
        if let Err(cause) = result {
            owner.record(cause);
        }
        self.finish(owner);
        result
    }
    fn open_in(
        parent: Dir,
        name: &str,
        owner: &mut Owner,
        recovery: bool,
        admission: Admission<'_>,
    ) -> Result<Self> {
        let Admission {
            uid,
            maximum,
            protection,
            expected,
        } = admission;
        check(owner, recovery)?;
        parent.final_namespace(owner)?;
        let descriptor = rustix::fs::openat(
            parent.file(),
            name,
            rustix::fs::OFlags::RDONLY
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC
                | rustix::fs::OFlags::NONBLOCK,
            rustix::fs::Mode::empty(),
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        let file = File::from(descriptor);
        let admitted = control::io(owner, file.metadata());
        let stamp = match admitted {
            Ok(stamp) => stamp,
            Err(cause) => {
                let _ = close(file, owner);
                return Err(cause);
            }
        };
        let mut value = Self {
            file: Some(file),
            parent,
            name: name.into(),
            stamp,
            sha256: None,
            visible: true,
        };
        let accepted = (|| {
            if expected.is_some_and(|expected| !same(expected, &value.stamp)) {
                return control::failed(owner, Cause::Changed);
            }
            let mode = value.stamp.mode() & 0o7777;
            let permissions = match protection {
                Protection::Private | Protection::RestoreInput => mode == 0o600,
                Protection::Tool => mode & 0o111 != 0,
                Protection::Caddy => matches!(mode, 0o600 | 0o644),
                Protection::RestoreSql => mode == 0o644,
            };
            let links = match protection {
                Protection::Caddy | Protection::RestoreSql | Protection::RestoreInput => {
                    value.stamp.nlink() > 0
                }
                _ => value.stamp.nlink() == 1,
            };
            if !value.stamp.is_file()
                || value.stamp.uid() != uid
                || !permissions
                || !links
                || value.stamp.len() > maximum
            {
                return control::failed(owner, Cause::Policy);
            }
            value.sha256 = Some(digest(value.file(), maximum, owner, recovery)?.1);
            value.verify(owner, recovery)
        })();
        if let Err(cause) = accepted {
            value.finish(owner);
            return Err(cause);
        }
        Ok(value)
    }
    pub fn create(parent: Dir, name: &str, owner: &mut Owner, recovery: bool) -> Result<Self> {
        check(owner, recovery)?;
        parent.final_namespace(owner)?;
        let descriptor = rustix::fs::openat(
            parent.file(),
            name,
            rustix::fs::OFlags::RDWR
                | rustix::fs::OFlags::CREATE
                | rustix::fs::OFlags::EXCL
                | rustix::fs::OFlags::NOFOLLOW
                | rustix::fs::OFlags::CLOEXEC,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .map_err(|_| {
            owner.record(Cause::Io);
            Cause::Io
        })?;
        let file = File::from(descriptor);
        let stamp = match control::io(owner, file.metadata()) {
            Ok(value) => value,
            Err(cause) => {
                let _ = close(file, owner);
                return Err(cause);
            }
        };
        let value = Self {
            file: Some(file),
            parent,
            name: name.into(),
            stamp,
            sha256: None,
            visible: true,
        };
        if !value.stamp.is_file()
            || value.stamp.nlink() != 1
            || value.stamp.len() != 0
            || value.stamp.mode() & 0o7777 != 0o600
            || value.stamp.uid() != rustix::process::geteuid().as_raw()
        {
            owner.record(Cause::Changed);
            value.finish(owner);
            return Err(Cause::Changed);
        }
        Ok(value)
    }
    pub fn bytes(&self, maximum: usize, owner: &mut Owner, recovery: bool) -> Result<Vec<u8>> {
        self.verify(owner, recovery)?;
        if self.len() > maximum as u64 {
            return control::failed(owner, Cause::Bounds);
        }
        let mut data = vec![0; self.len() as usize + 1];
        let mut offset = 0;
        loop {
            check(owner, recovery)?;
            let count = match self.file().read_at(&mut data[offset..], offset as u64) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                value => control::io(owner, value)?,
            };
            check(owner, recovery)?;
            if count == 0 {
                break;
            }
            offset += count;
            if offset > maximum || offset == data.len() {
                return control::failed(owner, Cause::Bounds);
            }
        }
        data.truncate(offset);
        self.verify(owner, recovery)?;
        Ok(data)
    }
    pub fn write(&mut self, data: &[u8], owner: &mut Owner, recovery: bool) -> Result<()> {
        self.write_bytes(data, owner, recovery)?;
        self.seal(owner, recovery)
    }
    fn write_bytes(&mut self, data: &[u8], owner: &mut Owner, recovery: bool) -> Result<()> {
        let mut at = 0;
        while at < data.len() {
            check(owner, recovery)?;
            let end = (at + 65536).min(data.len());
            let count = match self.file().write_at(&data[at..end], at as u64) {
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                value => control::io(owner, value)?,
            };
            if count == 0 {
                return control::failed(owner, Cause::Io);
            }
            at += count;
            check(owner, recovery)?;
        }
        Ok(())
    }
    pub fn seal(&mut self, owner: &mut Owner, recovery: bool) -> Result<()> {
        check(owner, recovery)?;
        control::io(owner, self.file().sync_all())?;
        self.observe_growth(owner, recovery)
    }
    pub fn observe_growth(&mut self, owner: &mut Owner, recovery: bool) -> Result<()> {
        check(owner, recovery)?;
        let stamp = control::io(owner, self.file().metadata())?;
        if !binding(&self.stamp, &stamp) || self.stamp.nlink() != stamp.nlink() {
            return control::failed(owner, Cause::Changed);
        }
        let (count, hash) = digest(self.file(), stamp.len(), owner, recovery)?;
        if count != stamp.len() {
            return control::failed(owner, Cause::Changed);
        }
        self.stamp = stamp;
        self.sha256 = Some(hash);
        self.verify(owner, recovery)
    }
    pub fn verify(&self, owner: &mut Owner, recovery: bool) -> Result<()> {
        check(owner, recovery)?;
        self.parent.final_namespace(owner)?;
        let stamp = control::io(owner, self.file().metadata())?;
        if !same(&self.stamp, &stamp) {
            return control::failed(owner, Cause::Changed);
        }
        if self.visible {
            let descriptor = rustix::fs::openat(
                self.parent.file(),
                self.name.as_str(),
                rustix::fs::OFlags::RDONLY
                    | rustix::fs::OFlags::NOFOLLOW
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
                rustix::fs::Mode::empty(),
            )
            .map_err(|_| {
                owner.record(Cause::Io);
                Cause::Io
            })?;
            let probe = File::from(descriptor);
            let metadata = control::io(owner, probe.metadata());
            if metadata
                .as_ref()
                .is_ok_and(|value| !same(&self.stamp, value))
            {
                owner.record(Cause::Changed);
            }
            let matched = metadata
                .as_ref()
                .is_ok_and(|value| same(&self.stamp, value));
            let closed = close(probe, owner);
            metadata?;
            closed?;
            if !matched {
                return Err(Cause::Changed);
            }
        }
        if let Some(expected) = &self.sha256 {
            let (count, actual) = digest(self.file(), self.len(), owner, recovery)?;
            if count != self.len() || expected != &actual {
                return control::failed(owner, Cause::Changed);
            }
        }
        check(owner, recovery)?;
        let after = control::io(owner, self.file().metadata())?;
        if !same(&self.stamp, &after) {
            return control::failed(owner, Cause::Changed);
        }
        check(owner, recovery)
    }
    pub fn renamed(&mut self, name: &str, owner: &mut Owner, recovery: bool) -> Result<()> {
        let stamp = control::io(owner, self.file().metadata())?;
        if !binding(&self.stamp, &stamp)
            || self.stamp.nlink() != stamp.nlink()
            || self.stamp.len() != stamp.len()
            || (self.stamp.mtime(), self.stamp.mtime_nsec()) != (stamp.mtime(), stamp.mtime_nsec())
        {
            return control::failed(owner, Cause::Changed);
        }
        self.name = name.into();
        self.stamp = stamp;
        self.verify(owner, recovery)
    }
    fn displaced(&mut self, owner: &mut Owner) -> Result<()> {
        let stamp = control::io(owner, self.file().metadata())?;
        if !binding(&self.stamp, &stamp)
            || stamp.nlink() != 0
            || self.stamp.len() != stamp.len()
            || (self.stamp.mtime(), self.stamp.mtime_nsec()) != (stamp.mtime(), stamp.mtime_nsec())
        {
            return control::failed(owner, Cause::Changed);
        }
        self.visible = false;
        self.stamp = stamp;
        Ok(())
    }
    pub fn sha256(&self) -> Option<&str> {
        self.sha256.as_deref()
    }
    pub fn aliases(&self, other: &Self) -> bool {
        same(&self.stamp, &other.stamp)
    }
    pub fn finish(mut self, owner: &mut Owner) {
        if let Some(file) = self.file.take() {
            let _ = close(file, owner);
        }
    }
}
impl Drop for Record {
    fn drop(&mut self) {
        #[cfg(test)]
        if self.file.is_some() {
            super::files::fallback();
        }
    }
}

#[derive(Default)]
pub(in crate::release::attestation) struct Records {
    pub files: Vec<Record>,
    sealed: Vec<Sealed>,
}
struct Sealed {
    parent: Dir,
    name: String,
    stamp: Metadata,
    sha256: String,
}
impl Records {
    pub fn caddy(&mut self, parent: Dir, uid: u32, owner: &mut Owner) -> Result<usize> {
        self.files.push(Record::caddy(parent, uid, owner)?);
        Ok(self.files.len() - 1)
    }
    pub fn open(
        &mut self,
        parent: Dir,
        name: &str,
        uid: u32,
        maximum: u64,
        owner: &mut Owner,
    ) -> Result<usize> {
        let record = Record::open(parent, name, uid, maximum, owner)?;
        self.files.push(record);
        Ok(self.files.len() - 1)
    }
    pub fn atomic(
        &mut self,
        parent: &Dir,
        name: &str,
        data: &[u8],
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        self.atomic_fault(parent, name, data, owner, recovery, None)
    }
    pub fn atomic_fault(
        &mut self,
        parent: &Dir,
        name: &str,
        data: &[u8],
        owner: &mut Owner,
        recovery: bool,
        fault: Option<u8>,
    ) -> Result<()> {
        check(owner, recovery)?;
        let previous = self.files.iter().rposition(|file| {
            file.visible && file.parent.path() == parent.path() && file.name == name
        });
        if let Some(at) = previous {
            self.files[at].verify(owner, recovery)?;
        } else {
            absent(parent, name, owner, recovery)?;
        }
        if fault == Some(0) {
            return control::failed(owner, Cause::Io);
        }
        let mut random = [0; 16];
        getrandom::fill(&mut random).map_err(|_| Cause::Io)?;
        let temporary = format!(".release-{}", hex(&random));
        let record = Record::create(parent.clone(), &temporary, owner, recovery)?;
        self.files.push(record);
        let at = self.files.len() - 1;
        self.files[at].write_bytes(data, owner, recovery)?;
        if fault == Some(1) {
            return control::failed(owner, Cause::Io);
        }
        self.files[at].seal(owner, recovery)?;
        #[cfg(test)]
        if fault == Some(FOREIGN_AFTER_TEMP) {
            let mut foreign = Record::create(parent.clone(), name, owner, recovery)?;
            let written = foreign.write(b"foreign", owner, recovery);
            foreign.finish(owner);
            written?;
        }
        if let Some(prior) = previous {
            self.files[prior].verify(owner, recovery)?;
        } else {
            absent(parent, name, owner, recovery)?;
        }
        check(owner, recovery)?;
        rustix::fs::renameat(parent.file(), temporary.as_str(), parent.file(), name).map_err(
            |_| {
                owner.record(Cause::Io);
                Cause::Io
            },
        )?;
        // The exact preceding rename is the only permission to update either
        // original generation's namespace/link metadata.
        if let Some(prior) = previous {
            self.files[prior].displaced(owner)?;
        }
        self.files[at].renamed(name, owner, recovery)?;
        if fault == Some(2) {
            return control::failed(owner, Cause::Io);
        }
        control::io(owner, parent.file().sync_all())?;
        check(owner, recovery)
    }
    pub fn prepare_log(
        &mut self,
        parent: &Dir,
        name: &str,
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<usize> {
        let record = Record::create(parent.clone(), name, owner, recovery)?;
        self.files.push(record);
        Ok(self.files.len() - 1)
    }
    pub fn finish_log(
        &mut self,
        at: usize,
        data: &[u8],
        owner: &mut Owner,
        recovery: bool,
    ) -> Result<()> {
        let mut written = self.files[at].write(data, owner, recovery);
        if let Err(cause) = written {
            owner.record(cause);
        }
        let record = self.files.remove(at);
        if written.is_ok()
            && let Some(digest) = &record.sha256
        {
            self.sealed.push(Sealed {
                parent: record.parent.clone(),
                name: record.name.clone(),
                stamp: record.stamp.clone(),
                sha256: digest.clone(),
            });
        } else if written.is_ok() {
            owner.record(Cause::Changed);
            written = Err(Cause::Changed);
        }
        record.finish(owner);
        written
    }
    pub fn verify(&self, owner: &mut Owner, recovery: bool) -> Result<()> {
        for file in &self.files {
            file.verify(owner, recovery)?;
        }
        for sealed in &self.sealed {
            let mut probe = Record::open_in(
                sealed.parent.clone(),
                &sealed.name,
                owner,
                recovery,
                Admission {
                    uid: sealed.stamp.uid(),
                    maximum: sealed.stamp.len(),
                    protection: Protection::Private,
                    expected: Some(&sealed.stamp),
                },
            )?;
            let matched =
                same(&probe.stamp, &sealed.stamp) && probe.sha256.as_ref() == Some(&sealed.sha256);
            if !matched {
                owner.record(Cause::Changed);
            }
            probe.visible = false;
            probe.finish(owner);
            if !matched {
                return Err(Cause::Changed);
            }
        }
        Ok(())
    }
    pub fn finish(mut self, owner: &mut Owner) {
        let _ = self.verify(owner, true);
        for record in self.files.drain(..) {
            record.finish(owner);
        }
        self.sealed.clear();
    }
}
