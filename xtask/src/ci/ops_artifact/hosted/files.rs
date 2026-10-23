//! Private nofollow trees. Expected writer bytes, full stamps and original
//! namespace witnesses survive close; same-user changes are detected, not locked.

use super::super::namespace::{self, Budget, Held, OwnedRun, Permit};
use super::failure;
use crate::{
    ci::{
        bounded_log::Log,
        command::SeparateLogs,
        ops_artifact::verify::tools::{Captures, Seal},
    },
    tooling::Result,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
};

const MAX_ENTRIES: usize = 131_072;
pub(super) const MAX_FILE: u64 = 512 * 1024 * 1024;
#[derive(Clone)]
pub(super) struct Record {
    pub name: String,
    pub stamp: Metadata,
    pub sha256: String,
    pub bytes: u64,
}
pub(super) struct Tree {
    pub run: OwnedRun,
    directories: BTreeMap<String, Metadata>,
    pub records: BTreeMap<String, Record>,
    registry: Option<Metadata>,
}

pub(super) fn safe(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 4096
        && name.split('/').count() <= 32
        && name
            .split('/')
            .all(|p| !p.is_empty() && p != "." && p != ".." && p.len() <= 255)
        && !name.bytes().any(|b| matches!(b, 0..=31 | 127 | b'\\'))
}
fn close(mut owners: Vec<Held>) -> Result<()> {
    let mut result = Ok(());
    for owner in owners.drain(..).rev() {
        result = result.and(owner.finish());
    }
    result
}
fn valid_file(stamp: &Metadata) -> bool {
    stamp.is_file()
        && stamp.nlink() == 1
        && stamp.uid() == rustix::process::getuid().as_raw()
        && matches!(stamp.mode() & 0o7777, 0o600 | 0o700)
}

impl Tree {
    pub fn create(path: &Path, budget: &Budget) -> Result<Self> {
        let run = OwnedRun::local(path, budget)?;
        Ok(Self {
            run,
            directories: BTreeMap::new(),
            records: BTreeMap::new(),
            registry: None,
        })
    }
    fn count(&self) -> Result<()> {
        if self.directories.len() + self.records.len() >= MAX_ENTRIES {
            Err(failure(1))
        } else {
            Ok(())
        }
    }
    fn walk(&self, name: &str) -> Result<Vec<Held>> {
        let mut owners: Vec<Held> = Vec::new();
        let mut prefix = String::new();
        let result = (|| {
            for part in name.split('/').filter(|p| !p.is_empty()) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(part);
                let parent = owners.last().map_or(self.run.file(), Held::file);
                let next = Held::open(
                    parent,
                    OsStr::new(part),
                    namespace::FLAGS | OFlags::DIRECTORY,
                    self.run.budget(),
                )?;
                let observed = namespace::private_directory(&next);
                owners.push(next);
                if !namespace::binding(
                    self.directories.get(&prefix).ok_or_else(|| failure(1))?,
                    &observed?,
                ) {
                    return Err(failure(1));
                }
            }
            Ok(())
        })();
        if let Err(error) = result {
            let _ = close(owners);
            return Err(error);
        }
        Ok(owners)
    }
    fn parent(&self, name: &str) -> Result<Vec<Held>> {
        self.walk(name.rsplit_once('/').map_or("", |(p, _)| p))
    }
    pub fn directory(&mut self, name: &str) -> Result<()> {
        if !safe(name) || self.records.contains_key(name) {
            return Err(failure(1));
        }
        if self.directories.contains_key(name) {
            return Ok(());
        }
        self.count()?;
        if let Some((parent, _)) = name.rsplit_once('/') {
            self.directory(parent)?;
        }
        let owners = self.parent(name)?;
        let parent = owners.last().map_or(self.run.file(), Held::file);
        let result = (|| {
            rustix::fs::mkdirat(
                parent,
                name.rsplit('/').next().unwrap(),
                Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| failure(1))?;
            let owner = Held::open(
                parent,
                OsStr::new(name.rsplit('/').next().unwrap()),
                namespace::FLAGS | OFlags::DIRECTORY,
                self.run.budget(),
            )?;
            let stamp = namespace::private_directory(&owner);
            let settled = owner.finish();
            let stamp = stamp?;
            settled?;
            self.directories.insert(name.to_owned(), stamp);
            Ok(())
        })();
        result.and(close(owners))
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], executable: bool) -> Result<()> {
        let mut reader = bytes;
        self.stream(
            name,
            bytes.len() as u64,
            executable,
            &mut reader,
            &mut || Ok(()),
        )
    }
    pub fn stream(
        &mut self,
        name: &str,
        size: u64,
        executable: bool,
        reader: &mut impl Read,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if !safe(name)
            || size > MAX_FILE
            || self.records.contains_key(name)
            || self.directories.contains_key(name)
        {
            return Err(failure(1));
        }
        self.count()?;
        if let Some((parent, _)) = name.rsplit_once('/') {
            self.directory(parent)?;
        }
        let owners = self.parent(name)?;
        let parent = owners.last().map_or(self.run.file(), Held::file);
        let result = (|| {
            let file = Held::create(
                parent,
                name.rsplit('/').next().unwrap(),
                executable,
                self.run.budget(),
            )?;
            let result = (|| {
                let initial = namespace::private_file(&file, executable)?;
                let mut writer = file.file();
                let mut hash = Sha256::new();
                let mut count = 0_u64;
                let mut block = [0_u8; 65536];
                while count < size {
                    checkpoint()?;
                    let limit = (size - count).min(block.len() as u64) as usize;
                    let n = reader.read(&mut block[..limit]).map_err(|_| failure(1))?;
                    if n == 0 {
                        return Err(failure(1));
                    }
                    writer.write_all(&block[..n]).map_err(|_| failure(1))?;
                    hash.update(&block[..n]);
                    count += n as u64;
                }
                writer.sync_all().map_err(|_| failure(1))?;
                let stamp = namespace::private_file(&file, executable)?;
                if !namespace::binding(&initial, &stamp) || stamp.len() != size {
                    return Err(failure(1));
                }
                let expected = super::super::hex(&hash.finalize());
                writer.seek(SeekFrom::Start(0)).map_err(|_| failure(1))?;
                let observed = consume(&mut writer, size, checkpoint)?;
                if observed != expected
                    || !namespace::same(&stamp, &file.file().metadata().map_err(|_| failure(1))?)
                {
                    return Err(failure(1));
                }
                let record = Record {
                    name: name.to_owned(),
                    stamp,
                    sha256: expected,
                    bytes: size,
                };
                self.visible(parent, &record)?;
                Ok(record)
            })();
            let settled = file.finish();
            let record = result?;
            settled?;
            Ok(record)
        })();
        let settled = close(owners);
        let record = result?;
        settled?;
        self.records.insert(name.to_owned(), record);
        Ok(())
    }
    fn visible(&self, parent: &File, record: &Record) -> Result<()> {
        ordinary(parent, record.name.rsplit('/').next().unwrap())?;
        let file = Held::open(
            parent,
            OsStr::new(record.name.rsplit('/').next().unwrap()),
            namespace::FLAGS,
            self.run.budget(),
        )?;
        let observed = file.file().metadata().map_err(|_| failure(1));
        let settled = file.finish();
        if !namespace::same(&record.stamp, &observed?) {
            return Err(failure(1));
        }
        settled
    }
    pub fn open(&self, name: &str) -> Result<Held> {
        let record = self.records.get(name).ok_or_else(|| failure(1))?;
        let owners = self.parent(name)?;
        let parent = owners.last().map_or(self.run.file(), Held::file);
        let admitted = rustix::fs::statat(
            parent,
            name.rsplit('/').next().unwrap(),
            AtFlags::SYMLINK_NOFOLLOW,
        )
        .map_err(|_| failure(1));
        let result = (|| {
            let admitted = admitted?;
            if rustix::fs::FileType::from_raw_mode(admitted.st_mode)
                != rustix::fs::FileType::RegularFile
                || convert::<_, u64>(admitted.st_dev)? != record.stamp.dev()
                || convert::<_, u64>(admitted.st_ino)? != record.stamp.ino()
            {
                return Err(failure(1));
            }
            let file = Held::open(
                parent,
                OsStr::new(name.rsplit('/').next().unwrap()),
                namespace::FLAGS,
                self.run.budget(),
            )?;
            let checked = file
                .file()
                .metadata()
                .map_err(|_| failure(1))
                .and_then(|stamp| {
                    if namespace::same(&record.stamp, &stamp) {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                });
            if let Err(error) = checked {
                let _ = file.finish();
                return Err(error);
            }
            Ok(file)
        })();
        let settled = close(owners);
        match result {
            Ok(file) => {
                if let Err(error) = settled {
                    let _ = file.finish();
                    Err(error)
                } else {
                    Ok(file)
                }
            }
            Err(error) => Err(error),
        }
    }
    pub fn input(&self, name: &str) -> Result<(Arc<File>, Vec<Permit>)> {
        let record = self.records.get(name).ok_or_else(|| failure(1))?;
        let owners = self.parent(name)?;
        let parent = owners.last().map_or(self.run.file(), Held::file);
        let result = (|| {
            ordinary(parent, name.rsplit('/').next().unwrap())?;
            // Original retained input plus the Native-owned Stdio clone.
            let permit = self.run.budget().reserve(2)?;
            let file = rustix::fs::openat(
                parent,
                name.rsplit('/').next().unwrap(),
                namespace::FLAGS,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| failure(1))?;
            let checked = file.metadata().map_err(|_| failure(1)).and_then(|m| {
                if namespace::same(&m, &record.stamp) {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            if let Err(error) = checked {
                let _ = nix::unistd::close(file);
                return Err(error);
            }
            Ok((file, permit))
        })();
        let closed = close(owners);
        match result {
            Ok((file, permit)) => match closed {
                Ok(()) => Ok((Arc::new(file), permit)),
                Err(error) => {
                    let _ = nix::unistd::close(file);
                    Err(error)
                }
            },
            Err(error) => Err(error),
        }
    }
    pub fn read(
        &self,
        name: &str,
        maximum: u64,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<Vec<u8>> {
        let record = self.records.get(name).ok_or_else(|| failure(1))?;
        if record.bytes > maximum {
            return Err(failure(1));
        }
        let file = self.open(name)?;
        let result = (|| {
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(record.bytes as usize)
                .map_err(|_| failure(1))?;
            let mut reader = file.file();
            let mut block = [0u8; 65536];
            while (bytes.len() as u64) < record.bytes {
                checkpoint()?;
                let limit = (record.bytes - bytes.len() as u64).min(block.len() as u64) as usize;
                let n = reader.read(&mut block[..limit]).map_err(|_| failure(1))?;
                if n == 0 {
                    return Err(failure(1));
                }
                bytes.extend_from_slice(&block[..n]);
            }
            if super::super::sha(&bytes) != record.sha256
                || !namespace::same(
                    &record.stamp,
                    &file.file().metadata().map_err(|_| failure(1))?,
                )
            {
                return Err(failure(1));
            }
            Ok(bytes)
        })();
        let closed = file.finish();
        result.and_then(|v| closed.map(|()| v))
    }
    pub async fn capture(
        &self,
        index: usize,
        maximum: u64,
    ) -> Result<(Arc<SeparateLogs>, [String; 2], Vec<Permit>)> {
        self.run.check()?;
        let permits = self.run.budget().reserve(8)?;
        let names = [
            format!("capture-{index}.stdout"),
            format!("capture-{index}.stderr"),
        ];
        let mut logs = Vec::new();
        let setup = (|| {
            for (name, cap) in names.iter().zip([maximum, 65536]) {
                let file = rustix::fs::openat(
                    self.run.file(),
                    name.as_str(),
                    OFlags::RDWR
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )
                .map(File::from)
                .map_err(|_| failure(1))?;
                let admitted = file.metadata().map_err(|_| failure(1)).and_then(|s| {
                    if valid_file(&s) && s.len() == 0 && s.mode() & 0o7777 == 0o600 {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                });
                if let Err(error) = admitted {
                    let _ = nix::unistd::close(file);
                    return Err(error);
                }
                logs.push(Log::start(file, cap)?);
            }
            Ok(())
        })();
        if let Err(error) = setup {
            for log in logs {
                let _ = log.finish().await;
            }
            return Err(error);
        }
        let stderr = logs.pop().unwrap();
        let stdout = logs.pop().unwrap();
        Ok((Arc::new(SeparateLogs { stdout, stderr }), names, permits))
    }
    pub fn captured(
        &mut self,
        names: &[String; 2],
        seals: &Captures,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        for (name, seal) in names.iter().zip([&seals.stdout, &seals.stderr]) {
            self.admit_seal(name, seal, checkpoint)?;
        }
        Ok(())
    }
    fn admit_seal(
        &mut self,
        name: &str,
        seal: &Seal,
        checkpoint: &mut impl FnMut() -> Result<()>,
    ) -> Result<()> {
        if !safe(name)
            || name.contains('/')
            || !valid_file(&seal.metadata)
            || seal.bytes != seal.metadata.len()
            || seal.bytes > MAX_FILE
            || self.records.contains_key(name)
        {
            return Err(failure(1));
        }
        ordinary(self.run.file(), name)?;
        let file = Held::open(
            self.run.file(),
            OsStr::new(name),
            namespace::FLAGS,
            self.run.budget(),
        )?;
        let result = (|| {
            if !namespace::same(
                &seal.metadata,
                &file.file().metadata().map_err(|_| failure(1))?,
            ) {
                return Err(failure(1));
            }
            let mut reader = file.file();
            if consume(&mut reader, seal.bytes, checkpoint)? != seal.sha256
                || !namespace::same(
                    &seal.metadata,
                    &file.file().metadata().map_err(|_| failure(1))?,
                )
            {
                return Err(failure(1));
            }
            let record = Record {
                name: name.to_owned(),
                stamp: seal.metadata.clone(),
                sha256: seal.sha256.clone(),
                bytes: seal.bytes,
            };
            self.visible(self.run.file(), &record)?;
            Ok(record)
        })();
        let settled = file.finish();
        let record = result?;
        settled?;
        self.records.insert(name.to_owned(), record);
        Ok(())
    }
    pub fn check(&self) -> Result<()> {
        self.run.check()?;
        if let Some(original) = &self.registry {
            let directory = Held::open(
                self.run.file(),
                OsStr::new("registry"),
                namespace::FLAGS | OFlags::DIRECTORY,
                self.run.budget(),
            )?;
            let checked = namespace::private_directory(&directory).and_then(|observed| {
                if namespace::binding(original, &observed) {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            checked.and(directory.finish())?;
        }
        for (directory, stamp) in &self.directories {
            let owners = self.walk(directory)?;
            let result = owners
                .last()
                .ok_or_else(|| failure(1))
                .and_then(namespace::private_directory)
                .and_then(|s| {
                    if namespace::binding(stamp, &s) {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                });
            result.and(close(owners))?;
        }
        for record in self.records.values() {
            self.open(&record.name)?.finish()?;
        }
        let mut folders = vec![String::new()];
        folders.extend(self.directories.keys().cloned());
        for folder in folders {
            let owners = self.walk(&folder)?;
            let parent = owners.last().map_or(self.run.file(), Held::file);
            let checked = (|| {
                let _permit = self.run.budget().permit()?;
                let entries = rustix::fs::Dir::read_from(parent).map_err(|_| failure(1))?;
                let mut actual = BTreeSet::new();
                for entry in entries {
                    let entry = entry.map_err(|_| failure(1))?;
                    let name = entry.file_name().to_str().map_err(|_| failure(1))?;
                    if !matches!(name, "." | "..")
                        && (name.len() > 255
                            || !actual.insert(name.to_owned())
                            || actual.len() > MAX_ENTRIES)
                    {
                        return Err(failure(1));
                    }
                }
                let prefix = if folder.is_empty() {
                    String::new()
                } else {
                    format!("{folder}/")
                };
                let mut expected = self
                    .directories
                    .keys()
                    .chain(self.records.keys())
                    .filter_map(|p| p.strip_prefix(&prefix))
                    .filter(|p| !p.contains('/'))
                    .map(str::to_owned)
                    .collect::<BTreeSet<_>>();
                if folder.is_empty() && self.registry.is_some() {
                    expected.insert("registry".to_owned());
                }
                if actual == expected {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            })();
            checked.and(close(owners))?;
        }
        Ok(())
    }
    pub fn directory_names(&self) -> impl Iterator<Item = &String> {
        self.directories.keys()
    }
    /// Only the separately copied and frame-verified registry subtree may be
    /// added after compiler materialization. Its contents remain governed by
    /// the unchanged registry/full-tree reader, not a relaxed generic walk.
    pub fn registry_created(&mut self) -> Result<()> {
        if self.registry.is_some()
            || self.directories.contains_key("registry")
            || self.records.contains_key("registry")
        {
            return Err(failure(1));
        }
        let directory = Held::open(
            self.run.file(),
            OsStr::new("registry"),
            namespace::FLAGS | OFlags::DIRECTORY,
            self.run.budget(),
        )?;
        let stamp = namespace::private_directory(&directory);
        let closed = directory.finish();
        let stamp = stamp?;
        closed?;
        self.registry = Some(stamp);
        self.check()
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let closed = self.run.finish();
        checked.and(closed)
    }
}

fn ordinary(parent: &File, name: &str) -> Result<()> {
    let stamp =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| failure(1))?;
    if rustix::fs::FileType::from_raw_mode(stamp.st_mode) != rustix::fs::FileType::RegularFile {
        Err(failure(1))
    } else {
        Ok(())
    }
}

fn convert<T: TryInto<U>, U>(value: T) -> Result<U> {
    value.try_into().map_err(|_| failure(1))
}

fn consume(
    reader: &mut impl Read,
    size: u64,
    checkpoint: &mut impl FnMut() -> Result<()>,
) -> Result<String> {
    let mut hash = Sha256::new();
    let mut count = 0;
    let mut block = [0u8; 65536];
    while count < size {
        checkpoint()?;
        let limit = (size - count).min(block.len() as u64) as usize;
        let n = reader.read(&mut block[..limit]).map_err(|_| failure(1))?;
        if n == 0 {
            return Err(failure(1));
        }
        hash.update(&block[..n]);
        count += n as u64;
    }
    checkpoint()?;
    Ok(super::super::hex(&hash.finalize()))
}
