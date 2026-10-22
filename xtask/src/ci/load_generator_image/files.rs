//! Anchored private files and a two-file artifact directory publication.

use super::plan::{self, Context, failure};
use crate::{ci::release_controller::files::Directory, tooling::Result};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    cell::Cell,
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}
fn mapped<T>(value: crate::tooling::Result<T>) -> Result<T> {
    value.map_err(|_| failure())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn private(directory: &Directory) -> Result<()> {
    mapped(directory.verify())?;
    let value = directory.file().metadata().map_err(|_| failure())?;
    if !value.is_dir()
        || value.uid() != rustix::process::getuid().as_raw()
        || value.mode() & 0o7777 != 0o700
    {
        return Err(failure());
    }
    Ok(())
}
#[derive(Clone, Copy)]
pub(super) enum Area {
    Work,
    Output,
}
pub(super) struct Record {
    pub name: String,
    pub stamp: Metadata,
    pub maximum: u64,
    pub area: Area,
}
pub(super) struct Owned {
    parent: Option<Directory>,
    work: Option<Directory>,
    output: Option<Directory>,
    configs: Vec<Directory>,
    pub path: PathBuf,
    pub temporary: PathBuf,
    pub token: String,
    next: Cell<usize>,
}
impl Owned {
    pub fn new(context: &Context) -> Result<Self> {
        Self::new_with(context, |_| {})
    }
    fn new_with(context: &Context, mut before_open: impl FnMut(&std::path::Path)) -> Result<Self> {
        let temporary = context.temporary.canonicalize().map_err(|_| failure())?;
        if temporary.to_str().is_none() || temporary.as_os_str().as_encoded_bytes().contains(&b',')
        {
            return Err(failure());
        }
        let parent = mapped(Directory::open(&temporary))?;
        let mut value = Self {
            parent: Some(parent),
            work: None,
            output: None,
            configs: vec![],
            path: PathBuf::new(),
            temporary,
            token: String::new(),
            next: Cell::new(0),
        };
        let result = (|| {
            let mut random = File::open("/dev/urandom").map_err(|_| failure())?;
            let mut bytes = [0; 16];
            let read = random.read_exact(&mut bytes).map_err(|_| failure());
            let closed = close(random);
            read?;
            closed?;
            value.token = bytes.iter().map(|b| format!("{b:02x}")).collect();
            let parent = value.parent.as_ref().unwrap();
            match rustix::fs::statat(parent.file(), "loadtest-image", AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => {}
                _ => return Err(failure()),
            }
            let name = format!("simplestchat-load-image-{}", value.token);
            rustix::fs::mkdirat(
                parent.file(),
                name.as_str(),
                Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| failure())?;
            value.path = value.temporary.join(&name);
            before_open(&value.path);
            value.work = Some(mapped(Directory::open(&value.path))?);
            private(value.work.as_ref().unwrap())?;
            for name in ["docker", "buildx", "publish"] {
                rustix::fs::mkdirat(
                    value.work.as_ref().unwrap().file(),
                    name,
                    Mode::from_bits_truncate(0o700),
                )
                .map_err(|_| failure())?;
                let directory = mapped(Directory::open(&value.path.join(name)))?;
                if let Err(error) = private(&directory) {
                    let _ = directory.finish();
                    return Err(error);
                }
                if name == "publish" {
                    value.output = Some(directory);
                } else {
                    value.configs.push(directory);
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    fn directory(&self, area: Area) -> &Directory {
        match area {
            Area::Work => self.work.as_ref().unwrap(),
            Area::Output => self.output.as_ref().unwrap(),
        }
    }
    pub fn check(&self) -> Result<()> {
        mapped(self.parent.as_ref().unwrap().verify())?;
        private(self.work.as_ref().unwrap())?;
        if let Some(output) = &self.output {
            private(output)?;
        }
        for config in &self.configs {
            private(config)?;
        }
        Ok(())
    }
    pub fn environment(
        &self,
        environment: &crate::tooling::Environment,
    ) -> crate::tooling::Environment {
        let mut environment = super::engine::frozen_environment(environment);
        environment.retain(|key, _| !key.as_encoded_bytes().starts_with(b"GIT_"));
        for key in ["DOCKER_CONFIG", "BUILDX_CONFIG", "WSL_DISTRO_NAME"] {
            environment.remove(std::ffi::OsStr::new(key));
        }
        environment.insert("DOCKER_CONFIG".into(), self.path.join("docker").into());
        environment.insert("BUILDX_CONFIG".into(), self.path.join("buildx").into());
        environment
    }
    pub fn cache(&self) -> Result<bool> {
        self.check()?;
        let path = self.temporary.join("buildx-cache");
        match std::fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Ok(value) if value.is_dir() => {}
            _ => return Err(failure()),
        }
        let directory = mapped(Directory::open(&path))?;
        let result = (|| {
            let file = match rustix::fs::openat(directory.file(), "index.json", READ, Mode::empty())
            {
                Ok(file) => File::from(file),
                Err(rustix::io::Errno::NOENT) => return Ok(false),
                Err(_) => return Err(failure()),
            };
            let stamp = file.metadata().map_err(|_| failure());
            let closed = close(file);
            let stamp = stamp?;
            closed?;
            if !stamp.is_file() || stamp.nlink() != 1 || stamp.len() > 1024 * 1024 {
                return Err(failure());
            }
            mapped(directory.check("index.json", &stamp))?;
            Ok(true)
        })();
        let closed = directory.finish().map_err(|_| failure());
        let result = result?;
        closed?;
        Ok(result)
    }
    pub fn create(&self, area: Area, name: &str, maximum: u64) -> Result<(File, Record)> {
        self.create_with(area, name, maximum, || {}, |_| {})
    }
    fn create_with(
        &self,
        area: Area,
        name: &str,
        maximum: u64,
        before_open: impl FnOnce(),
        before_admit: impl FnOnce(&File),
    ) -> Result<(File, Record)> {
        self.check()?;
        if name.is_empty()
            || name.len() > 128
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
            || maximum == 0
            || maximum > plan::COMPRESSED_LIMIT
        {
            return Err(failure());
        }
        let directory = self.directory(area);
        before_open();
        let file = File::from(
            rustix::fs::openat(
                directory.file(),
                name,
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| failure())?,
        );
        before_admit(&file);
        let admitted = file.metadata().map_err(|_| failure()).and_then(|stamp| {
            if !stamp.is_file()
                || stamp.len() != 0
                || stamp.nlink() != 1
                || stamp.mode() & 0o7777 != 0o600
                || stamp.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure());
            }
            mapped(directory.check(name, &stamp))?;
            Ok(stamp)
        });
        match admitted {
            Ok(stamp) => Ok((
                file,
                Record {
                    name: name.into(),
                    stamp,
                    maximum,
                    area,
                },
            )),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    pub fn log(&self, maximum: u64) -> Result<(File, Record)> {
        let next = self.next.get();
        if next >= 192 {
            return Err(failure());
        }
        self.next.set(next + 1);
        self.create(Area::Work, &format!("command-{next:03}.log"), maximum)
    }
    pub fn write(&self, area: Area, name: &str, bytes: &[u8]) -> Result<Record> {
        if bytes.len() > 16 * 1024 {
            return Err(failure());
        }
        let (mut file, mut record) = self.create(area, name, 16 * 1024)?;
        let result = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| failure());
        let stamp = file.metadata().map_err(|_| failure());
        let closed = close(file);
        result?;
        closed?;
        let stamp = stamp?;
        if self.read(&record)? != bytes {
            return Err(failure());
        }
        self.check_input(&record, &stamp)?;
        record.stamp = stamp;
        Ok(record)
    }
    fn open(&self, record: &Record) -> Result<(File, Metadata)> {
        self.check()?;
        let file = File::from(
            rustix::fs::openat(
                self.directory(record.area).file(),
                record.name.as_str(),
                READ,
                Mode::empty(),
            )
            .map_err(|_| failure())?,
        );
        let result = file.metadata().map_err(|_| failure()).and_then(|stamp| {
            if !stamp.is_file()
                || !binding(&record.stamp, &stamp)
                || stamp.nlink() != 1
                || stamp.len() > record.maximum
            {
                return Err(failure());
            }
            Ok(stamp)
        });
        match result {
            Ok(stamp) => Ok((file, stamp)),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    pub fn input(&self, record: &Record) -> Result<(File, Metadata)> {
        self.open(record)
    }
    pub fn check_input(&self, record: &Record, stamp: &Metadata) -> Result<()> {
        let (file, observed) = self.open(record)?;
        let closed = close(file);
        if !same(stamp, &observed) {
            return Err(failure());
        }
        closed
    }
    pub fn read(&self, record: &Record) -> Result<Vec<u8>> {
        if record.maximum > plan::BUILD_LOG_LIMIT {
            return Err(failure());
        }
        let (mut file, stamp) = self.open(record)?;
        let result = (|| {
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(record.maximum + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure())?;
            if bytes.len() as u64 != stamp.len()
                || !same(&stamp, &file.metadata().map_err(|_| failure())?)
            {
                return Err(failure());
            }
            mapped(self.directory(record.area).check(&record.name, &stamp))?;
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = result?;
        closed?;
        Ok(bytes)
    }
    pub fn remove(&self, record: &Record) -> Result<()> {
        let (file, stamp) = self.open(record)?;
        let closed = close(file);
        closed?;
        mapped(self.directory(record.area).check(&record.name, &stamp))?;
        rustix::fs::unlinkat(
            self.directory(record.area).file(),
            record.name.as_str(),
            AtFlags::empty(),
        )
        .map_err(|_| failure())
    }
    pub fn archive_path(&self, record: &Record) -> PathBuf {
        match record.area {
            Area::Work => self.path.join(&record.name),
            Area::Output => self.path.join("publish").join(&record.name),
        }
    }
    pub fn publish(
        &mut self,
        archive: &Record,
        sealed: &Metadata,
        id: &Record,
        expected_id: &str,
    ) -> Result<()> {
        self.check()?;
        self.check_input(archive, sealed)?;
        if self.read(id)? != format!("{expected_id}\n").as_bytes() {
            return Err(failure());
        }
        self.check_input(id, &id.stamp)?;
        let output = self.output.as_ref().ok_or_else(failure)?;
        let prepared = (|| {
            let mut names = Vec::new();
            for entry in rustix::fs::Dir::read_from(output.file()).map_err(|_| failure())? {
                let entry = entry.map_err(|_| failure())?;
                let name = entry.file_name().to_bytes();
                if matches!(name, b"." | b"..") {
                    continue;
                }
                if names.len() >= 2 {
                    return Err(failure());
                }
                names.push(name.to_vec());
            }
            names.sort();
            let mut expected = vec![
                archive.name.as_bytes().to_vec(),
                id.name.as_bytes().to_vec(),
            ];
            expected.sort();
            if names != expected {
                return Err(failure());
            }
            let (a, astamp) = self.open(archive)?;
            let aclose = close(a);
            let (b, bstamp) = self.open(id)?;
            let bclose = close(b);
            aclose?;
            bclose?;
            mapped(output.check(&archive.name, &astamp))?;
            mapped(output.check(&id.name, &bstamp))?;
            output.file().metadata().map_err(|_| failure())
        })();
        let closed = self.output.take().unwrap().finish().map_err(|_| failure());
        let stamp = prepared?;
        closed?;
        let work = self.work.as_ref().unwrap();
        let parent = self.parent.as_ref().unwrap();
        rustix::fs::renameat_with(
            work.file(),
            "publish",
            parent.file(),
            "loadtest-image",
            rustix::fs::RenameFlags::NOREPLACE,
        )
        .map_err(|_| failure())?;
        let directory = mapped(Directory::open(&self.temporary.join("loadtest-image")))?;
        let checked = private(&directory).and_then(|()| {
            let observed = directory.file().metadata().map_err(|_| failure())?;
            if !binding(&stamp, &observed) {
                return Err(failure());
            }
            Ok(())
        });
        let closed = directory.finish().map_err(|_| failure());
        checked.and(closed)
    }
    pub fn finish(mut self) -> Result<()> {
        let mut failed = false;
        for config in self.configs.drain(..) {
            failed |= config.finish().is_err();
        }
        if let Some(output) = self.output.take() {
            failed |= output.finish().is_err();
        }
        if let Some(work) = self.work.take() {
            failed |= work.finish().is_err();
        }
        if let Some(parent) = self.parent.take() {
            failed |= parent.finish().is_err();
        }
        if failed { Err(failure()) } else { Ok(()) }
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
