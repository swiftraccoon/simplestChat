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
#[cfg(test)]
thread_local! {
    static CADDY_LATE_FAULT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}
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
    /// Only a newly published private Caddyfile may receive this exact mode
    /// transition. Every other stamp field and the original content pin remain
    /// authoritative; a later failure retains the already-published generation.
    fn publish_caddy_mode(&mut self, owner: &mut Owner) -> Result<()> {
        self.verify(owner, false)?;
        if self.name != "Caddyfile"
            || !self.visible
            || self.stamp.mode() & 0o7777 != 0o600
            || self.stamp.nlink() != 1
            || self.stamp.uid() != rustix::process::geteuid().as_raw()
        {
            return control::failed(owner, Cause::Policy);
        }
        rustix::fs::fchmod(self.file(), rustix::fs::Mode::from_bits_truncate(0o644)).map_err(
            |_| {
                owner.record(Cause::Io);
                Cause::Io
            },
        )?;
        let stamp = control::io(owner, self.file().metadata())?;
        if !stamp.is_file()
            || self.stamp.dev() != stamp.dev()
            || self.stamp.ino() != stamp.ino()
            || self.stamp.uid() != stamp.uid()
            || self.stamp.gid() != stamp.gid()
            || self.stamp.nlink() != stamp.nlink()
            || self.stamp.len() != stamp.len()
            || (self.stamp.mode() & !0o7777) != (stamp.mode() & !0o7777)
            || stamp.mode() & 0o7777 != 0o644
            || (self.stamp.mtime(), self.stamp.mtime_nsec()) != (stamp.mtime(), stamp.mtime_nsec())
        {
            return control::failed(owner, Cause::Changed);
        }
        // The preceding fchmod is the sole authority for mode/ctime movement.
        // Register it before a later sync, namespace probe or deadline can fail.
        self.stamp = stamp;
        #[cfg(test)]
        if CADDY_LATE_FAULT.replace(false) {
            return control::failed(owner, Cause::Io);
        }
        check(owner, false)?;
        control::io(owner, self.file().sync_all())?;
        self.parent.sync(owner)?;
        self.verify(owner, false)
    }
    fn displaced(&mut self, owner: &mut Owner) -> Result<()> {
        let stamp = control::io(owner, self.file().metadata())?;
        // A readonly legacy Caddyfile may have other links. Our exact
        // rename/unlink removes one original name; it grants no authority over
        // those peers. All other private generations retain their zero-link rule.
        let links =
            if self.name == "Caddyfile" && matches!(self.stamp.mode() & 0o7777, 0o600 | 0o644) {
                self.stamp.nlink().checked_sub(1) == Some(stamp.nlink())
            } else {
                stamp.nlink() == 0
            };
        if !binding(&self.stamp, &stamp)
            || !links
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
    /// Replace the registered readonly Caddyfile generation and publish its
    /// new fixed readonly mode. This grants no public mode to another record.
    pub fn publish_caddy(&mut self, parent: &Dir, data: &[u8], owner: &mut Owner) -> Result<()> {
        check(owner, false)?;
        let old = self
            .files
            .iter()
            .rposition(|record| {
                record.visible
                    && record.parent.path() == parent.path()
                    && record.name == "Caddyfile"
            })
            .ok_or_else(|| {
                owner.record(Cause::Changed);
                Cause::Changed
            })?;
        self.files[old].verify(owner, false)?;
        let next = self.files.len();
        self.atomic(parent, "Caddyfile", data, owner, false)?;
        self.files
            .get_mut(next)
            .ok_or_else(|| {
                owner.record(Cause::Changed);
                Cause::Changed
            })?
            .publish_caddy_mode(owner)
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

#[cfg(test)]
mod caddy_publication_tests {
    use super::*;
    use crate::test_support::Fixture;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        time::{Duration, Instant},
    };

    fn seed(path: &std::path::Path, bytes: &[u8], mode: u32) {
        fs::write(path, bytes).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
    }
    fn setup() -> (Fixture, Owner, Dir, Records) {
        let fixture = Fixture::new();
        let mut owner = Owner::new(
            Instant::now(),
            Duration::from_secs(1800),
            Duration::from_secs(60),
        )
        .unwrap();
        super::super::files::start_probe();
        let directory = Dir::open(&fixture.0, &mut owner).unwrap();
        (fixture, owner, directory, Records::default())
    }
    #[test]
    fn replacement_publishes_only_new_caddy_readonly_and_retains_hardlinked_old_generation() {
        for mode in [0o600, 0o644] {
            for linked in [false, true] {
                let (fixture, mut owner, directory, mut records) = setup();
                let path = fixture.0.join("Caddyfile");
                seed(&path, b"old source", mode);
                let alias = fixture.0.join("old-caddy-alias");
                if linked {
                    fs::hard_link(&path, &alias).unwrap();
                }
                seed(&fixture.0.join("app.env"), b"private", 0o600);
                let uid = rustix::process::geteuid().as_raw();
                let old = records.caddy(directory.clone(), uid, &mut owner).unwrap();
                records
                    .open(directory.clone(), "app.env", uid, 64, &mut owner)
                    .unwrap();
                let written = records.publish_caddy(&directory, b"new source", &mut owner);
                let verified = records.verify(&mut owner, false);
                let old_data = records.files[old].bytes(64, &mut owner, false);
                let old_links = records.files[old].stamp.nlink();
                let visible = fs::read(&path);
                let stamp = fs::metadata(&path).unwrap();
                let private_mode = fs::metadata(fixture.0.join("app.env")).unwrap().mode() & 0o7777;
                let alias_data = if linked {
                    Some(fs::read(&alias).unwrap())
                } else {
                    None
                };
                records.finish(&mut owner);
                directory.finish(&mut owner);
                let closes = super::super::files::take_probe();
                let ended = owner.finish();
                assert_eq!(written, Ok(()));
                assert_eq!(verified, Ok(()));
                assert_eq!(ended, Ok(()));
                assert_eq!(visible.unwrap(), b"new source");
                assert_eq!(stamp.mode() & 0o7777, 0o644);
                assert_eq!(stamp.nlink(), 1);
                assert_eq!(private_mode, 0o600);
                assert_eq!(old_data.unwrap(), b"old source");
                assert_eq!(old_links, u64::from(linked));
                if linked {
                    assert_eq!(alias_data.unwrap(), b"old source");
                }
                assert!(closes.0 > 2);
                assert_eq!(closes.1, 0);
            }
        }
    }
    #[test]
    fn fixed_mode_transition_refuses_other_names_links_and_foreign_mode_changes() {
        for kind in ["other-name", "hardlink", "mode"] {
            let (fixture, mut owner, directory, _records) = setup();
            let name = if kind == "other-name" {
                "app.env"
            } else {
                "Caddyfile"
            };
            let mut record = Record::create(directory.clone(), name, &mut owner, false).unwrap();
            record.write(b"private", &mut owner, false).unwrap();
            let path = fixture.0.join(name);
            if kind == "hardlink" {
                fs::hard_link(&path, fixture.0.join("foreign-alias")).unwrap();
            }
            if kind == "mode" {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o640)).unwrap();
            }
            let refused = record.publish_caddy_mode(&mut owner);
            let mode = fs::metadata(&path).unwrap().mode() & 0o7777;
            record.finish(&mut owner);
            directory.finish(&mut owner);
            let closes = super::super::files::take_probe();
            let ended = owner.finish();
            let cause = if kind == "other-name" {
                Cause::Policy
            } else {
                Cause::Changed
            };
            assert_eq!(refused, Err(cause));
            assert_eq!(ended, Err(cause));
            assert_eq!(mode, if kind == "mode" { 0o640 } else { 0o600 });
            assert_eq!(closes.1, 0);
        }
    }
    #[test]
    fn late_failure_retains_publication_and_authorized_stamp_through_all_checked_closes() {
        let (fixture, mut owner, directory, mut records) = setup();
        let path = fixture.0.join("Caddyfile");
        seed(&path, b"old", 0o644);
        records
            .caddy(
                directory.clone(),
                rustix::process::geteuid().as_raw(),
                &mut owner,
            )
            .unwrap();
        CADDY_LATE_FAULT.set(true);
        let written = records.publish_caddy(&directory, b"retained", &mut owner);
        let checked = records.verify(&mut owner, true);
        let stamp = fs::metadata(&path).unwrap();
        let visible = fs::read(&path);
        super::super::files::fail_close(super::super::files::checked_closes() + 1);
        records.finish(&mut owner);
        directory.finish(&mut owner);
        let closes = super::super::files::take_probe();
        let ended = owner.finish();
        assert_eq!(written, Err(Cause::Io));
        assert_eq!(checked, Ok(()));
        assert_eq!(visible.unwrap(), b"retained");
        assert_eq!(stamp.mode() & 0o7777, 0o644);
        assert_eq!(ended, Err(Cause::Io));
        assert!(closes.0 > 2);
        assert_eq!(closes.1, 0);
    }
    #[test]
    fn later_public_mode_tampering_remains_changed_instead_of_becoming_admitted() {
        let (fixture, mut owner, directory, mut records) = setup();
        let path = fixture.0.join("Caddyfile");
        seed(&path, b"old", 0o644);
        records
            .caddy(
                directory.clone(),
                rustix::process::geteuid().as_raw(),
                &mut owner,
            )
            .unwrap();
        records
            .publish_caddy(&directory, b"new", &mut owner)
            .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let checked = records.verify(&mut owner, false);
        records.finish(&mut owner);
        directory.finish(&mut owner);
        let closes = super::super::files::take_probe();
        let ended = owner.finish();
        assert_eq!(checked, Err(Cause::Changed));
        assert_eq!(ended, Err(Cause::Changed));
        assert_eq!(fs::metadata(path).unwrap().mode() & 0o7777, 0o666);
        assert_eq!(closes.1, 0);
    }
    #[test]
    fn unregistered_old_caddy_is_refused_before_any_new_generation_or_mode_effect() {
        let (fixture, mut owner, directory, mut records) = setup();
        let path = fixture.0.join("Caddyfile");
        seed(&path, b"foreign", 0o644);
        let written = records.publish_caddy(&directory, b"new", &mut owner);
        let count = fs::read_dir(&fixture.0).unwrap().count();
        records.finish(&mut owner);
        directory.finish(&mut owner);
        let closes = super::super::files::take_probe();
        let ended = owner.finish();
        assert_eq!(written, Err(Cause::Changed));
        assert_eq!(ended, Err(Cause::Changed));
        assert_eq!(fs::read(path).unwrap(), b"foreign");
        assert_eq!(count, 1);
        assert_eq!(closes.1, 0);
    }
}
