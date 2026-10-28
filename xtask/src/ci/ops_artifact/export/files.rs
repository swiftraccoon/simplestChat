//! Retained original request and exact private export generations. Every acquired
//! File receives an explicit close attempt; no generated path adopts a replacement.

use super::super::{
    acquire,
    namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun},
};
use super::{failure, io, request, worker::Checkpoint};
use crate::tooling::{Failure, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};

#[derive(Clone)]
struct Record {
    name: String,
    stamp: Metadata,
    sha256: String,
    bytes: u64,
}
fn consume(
    file: &File,
    bytes: u64,
    checkpoint: &Checkpoint,
    collect: bool,
) -> Result<(String, Vec<u8>)> {
    let mut file = file;
    io(file.seek(SeekFrom::Start(0)))?;
    let mut remaining = bytes;
    let mut hash = Sha256::new();
    let mut result = Vec::new();
    let mut block = [0; 65536];
    while remaining != 0 {
        checkpoint.check()?;
        let size = remaining.min(block.len() as u64) as usize;
        let count = match file.read(&mut block[..size]) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            v => io(v)?,
        };
        if count == 0 {
            return Err(failure(1));
        }
        remaining -= count as u64;
        hash.update(&block[..count]);
        if collect {
            result.extend_from_slice(&block[..count])
        }
    }
    checkpoint.check()?;
    if io(file.read(&mut block[..1]))? != 0 {
        return Err(failure(1));
    }
    Ok((super::super::hex(&hash.finalize()), result))
}
fn visible(parent: &File, record: &Record, budget: &Budget) -> Result<()> {
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let matched =
        namespace::private_file(&file, false).map(|stamp| namespace::same(&record.stamp, &stamp));
    let closed = file.finish();
    let matched = matched.and_then(|same| closed.map(|()| same))?;
    if matched { Ok(()) } else { Err(failure(1)) }
}
fn read(
    parent: &File,
    record: &Record,
    budget: &Budget,
    checkpoint: &Checkpoint,
    collect: bool,
) -> Result<Vec<u8>> {
    checkpoint.check()?;
    if collect && record.bytes > request::MAXIMUM {
        return Err(failure(1));
    }
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let result = (|| {
        if !namespace::same(&record.stamp, &namespace::private_file(&file, false)?) {
            return Err(failure(1));
        }
        let (hash, bytes) = consume(file.file(), record.bytes, checkpoint, collect)?;
        if hash != record.sha256 || !namespace::same(&record.stamp, &io(file.file().metadata())?) {
            return Err(failure(1));
        }
        visible(parent, record, budget)?;
        checkpoint.check()?;
        Ok(bytes)
    })();
    let closed = file.finish();
    result.and_then(|bytes| closed.map(|()| bytes))
}
#[derive(Clone)]
pub(super) struct Original {
    parent: ChainWitness,
    record: Record,
}
impl Original {
    pub fn open(path: &Path, budget: &Budget, checkpoint: &Checkpoint) -> Result<(Self, Vec<u8>)> {
        checkpoint.check()?;
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = (|| {
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| failure(1))?;
            let file = Held::open(parent.file(), OsStr::new(name), FLAGS, budget)?;
            let result = (|| {
                let stamp = namespace::private_file(&file, false)?;
                if stamp.len() == 0 || stamp.len() > request::MAXIMUM {
                    return Err(failure(1));
                }
                let (hash, bytes) = consume(file.file(), stamp.len(), checkpoint, true)?;
                if !namespace::same(&stamp, &io(file.file().metadata())?) {
                    return Err(failure(1));
                }
                let record = Record {
                    name: name.into(),
                    bytes: stamp.len(),
                    stamp,
                    sha256: hash,
                };
                visible(parent.file(), &record, budget)?;
                Ok((record, bytes))
            })();
            let closed = file.finish();
            let (record, bytes) = result.and_then(|value| closed.map(|()| value))?;
            Ok((
                Self {
                    parent: parent.witness()?,
                    record,
                },
                bytes,
            ))
        })();
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let checked = read(parent.file(), &self.record, budget, checkpoint, true);
        let closed = parent.finish();
        checked.map(|_| ()).and(closed)
    }
}
fn names(file: &File) -> Result<BTreeSet<String>> {
    let entries = rustix::fs::Dir::read_from(file).map_err(|_| failure(1))?;
    let mut result = BTreeSet::new();
    for entry in entries {
        let entry = entry.map_err(|_| failure(1))?;
        let name = entry.file_name().to_str().map_err(|_| failure(1))?;
        if matches!(name, "." | "..") {
            continue;
        }
        if result.len() >= 8 || name.len() > 64 || !result.insert(name.into()) {
            return Err(failure(1));
        }
    }
    Ok(result)
}
struct Bounded<'a> {
    file: &'a File,
    checkpoint: &'a Checkpoint,
    count: u64,
    primary: Option<Failure>,
    hash: Sha256,
}
impl Write for Bounded<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let result = (|| {
            self.checkpoint.check()?;
            let length = bytes.len().min(65536);
            if self
                .count
                .checked_add(length as u64)
                .is_none_or(|n| n > super::TRANSFER)
            {
                return Err(failure(1));
            }
            let mut file = self.file;
            let count = io(file.write(&bytes[..length]))?;
            self.count += count as u64;
            self.hash.update(&bytes[..count]);
            self.checkpoint.check()?;
            Ok(count)
        })();
        match result {
            Ok(count) => Ok(count),
            Err(error) => {
                super::keep(&mut self.primary, error);
                Err(std::io::Error::other("Ops export write is unproven."))
            }
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
pub(super) struct Output {
    run: OwnedRun,
    records: BTreeMap<String, Record>,
}
impl Output {
    pub fn new(path: &Path, budget: &Budget) -> Result<Self> {
        Ok(Self {
            run: OwnedRun::local(path, budget)?,
            records: BTreeMap::new(),
        })
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.run.path.join(name)
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], checkpoint: &Checkpoint) -> Result<()> {
        if !matches!(
            name,
            "acquire-request.json" | "install-request.json" | "export.json"
        ) || bytes.is_empty()
            || bytes.len() as u64 > request::MAXIMUM
        {
            return Err(failure(1));
        }
        checkpoint.check()?;
        self.run.check()?;
        let stamp = namespace::write(self.run.file(), name, bytes, self.run.budget())?;
        self.records.insert(
            name.into(),
            Record {
                name: name.into(),
                stamp,
                sha256: super::super::sha(bytes),
                bytes: bytes.len() as u64,
            },
        );
        checkpoint.check()?;
        self.run.check()
    }
    fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        self.run.check()?;
        for record in self.records.values() {
            read(
                self.run.file(),
                record,
                self.run.budget(),
                checkpoint,
                false,
            )?;
        }
        checkpoint.check()
    }
    pub fn finish(self, checkpoint: &Checkpoint) -> Result<Witness> {
        self.finish_with(checkpoint, OwnedRun::finish)
    }
    fn finish_with(
        self,
        checkpoint: &Checkpoint,
        close: impl FnOnce(OwnedRun) -> Result<()>,
    ) -> Result<Witness> {
        let checked = (|| {
            self.check(checkpoint)?;
            let expected = self.records.keys().cloned().collect::<BTreeSet<_>>();
            if names(self.run.file())? != expected {
                return Err(failure(1));
            }
            io(self.run.file().sync_all())?;
            io(self.run.parent.file().sync_all())?;
            Ok((self.run.parent.witness()?, io(self.run.file().metadata())?))
        })();
        let path = self.run.path.clone();
        let closed = close(self.run);
        let (parent, stamp) = checked.and_then(|v| closed.map(|()| v))?;
        Ok(Witness {
            parent,
            path,
            stamp,
            records: self.records,
            acquired: false,
        })
    }
}
pub(super) struct Witness {
    parent: ChainWitness,
    path: PathBuf,
    stamp: Metadata,
    records: BTreeMap<String, Record>,
    acquired: bool,
}
/// Short-lived original-directory reopening. Its entire ancestor chain closes
/// before any authenticated-library call; only the bounded tar writer crosses
/// that boundary. The shared descriptor budget and inner limits are unchanged.
struct Reopened {
    parent: Chain,
    file: Held,
    stamp: Metadata,
    path: PathBuf,
    budget: Budget,
}
impl Reopened {
    fn open(witness: &Witness, budget: &Budget) -> Result<Self> {
        let parent = witness.parent.reopen(budget)?;
        let acquired = (|| {
            let file = Held::open(
                parent.file(),
                witness.path.file_name().ok_or_else(|| failure(1))?,
                FLAGS | rustix::fs::OFlags::DIRECTORY,
                budget,
            )?;
            let admitted = namespace::private_directory(&file).and_then(|stamp| {
                if namespace::binding(&witness.stamp, &stamp) {
                    Ok(stamp)
                } else {
                    Err(failure(1))
                }
            });
            match admitted {
                Ok(stamp) => Ok((file, stamp)),
                Err(error) => {
                    let _ = file.finish();
                    Err(error)
                }
            }
        })();
        match acquired {
            Ok((file, stamp)) => Ok(Self {
                parent,
                file,
                stamp,
                path: witness.path.clone(),
                budget: budget.clone(),
            }),
            Err(error) => {
                let _ = parent.finish();
                Err(error)
            }
        }
    }
    fn file(&self) -> &File {
        self.file.file()
    }
    fn check(&self) -> Result<()> {
        self.parent.verify()?;
        let probe = Held::open(
            self.parent.file(),
            self.path.file_name().ok_or_else(|| failure(1))?,
            FLAGS | rustix::fs::OFlags::DIRECTORY,
            &self.budget,
        )?;
        let checked = (|| {
            if namespace::binding(&self.stamp, &namespace::private_directory(&probe)?)
                && namespace::binding(&self.stamp, &namespace::private_directory(&self.file)?)
            {
                Ok(())
            } else {
                Err(failure(1))
            }
        })();
        let closed = probe.finish();
        checked.and(closed)
    }
    fn finish(self) -> Result<()> {
        let checked = self.check();
        let file = self.file.finish();
        let parent = self.parent.finish();
        checked.and(file).and(parent)
    }
}
impl Witness {
    pub fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
    pub fn acquired(&mut self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        self.acquired = true;
        self.check(budget, checkpoint)
    }
    pub fn write(
        &mut self,
        name: &str,
        bytes: &[u8],
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        if !matches!(name, "install-request.json" | "export.json")
            || bytes.is_empty()
            || bytes.len() as u64 > request::MAXIMUM
        {
            return Err(failure(1));
        }
        self.check(budget, checkpoint)?;
        let root = Reopened::open(self, budget)?;
        let written = (|| {
            checkpoint.check()?;
            root.check()?;
            let stamp = namespace::write(root.file(), name, bytes, budget)?;
            io(root.file().sync_all())?;
            io(root.parent.file().sync_all())?;
            root.check()?;
            checkpoint.check()?;
            Ok(Record {
                name: name.into(),
                stamp,
                sha256: super::super::sha(bytes),
                bytes: bytes.len() as u64,
            })
        })();
        let closed = root.finish();
        let record = written.and_then(|v| closed.map(|()| v))?;
        self.records.insert(name.into(), record);
        self.check(budget, checkpoint)
    }
    pub fn transfer(
        &mut self,
        authenticated: &acquire::AuthenticatedAcquisition,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<(acquire::PairPins, String, u64)> {
        self.check(budget, checkpoint)?;
        let root = Reopened::open(self, budget)?;
        let acquired = (|| {
            let file = Held::create(root.file(), "ops-pair.tar", false, budget)?;
            let admitted = namespace::private_file(&file, false)
                .and_then(|stamp| root.check().map(|()| stamp));
            match admitted {
                Ok(stamp) => Ok((file, stamp)),
                Err(error) => {
                    let _ = file.finish();
                    Err(error)
                }
            }
        })();
        let closed = root.finish();
        let (file, initial) = acquired?;
        if let Err(error) = closed {
            let _ = file.finish();
            return Err(error);
        }
        let result = (|| {
            // No export ancestor is live across the mature verifier's retained
            // descriptor allowance. The one original writer remains owned.
            #[cfg(test)]
            assert_eq!(
                budget.count(),
                1,
                "only the transfer writer may cross authentication"
            );
            let mut writer = Bounded {
                file: file.file(),
                checkpoint,
                count: 0,
                primary: None,
                hash: Sha256::new(),
            };
            let pins = authenticated.export_tar(&mut writer, budget, &checkpoint.acquired());
            let primary = writer.primary.take();
            let bytes = writer.count;
            let expected = super::super::hex(&writer.hash.finalize());
            if let Some(error) = primary {
                return Err(error);
            }
            let pins = pins?;
            io(file.file().sync_all())?;
            let stamp = namespace::private_file(&file, false)?;
            if !namespace::binding(&initial, &stamp)
                || stamp.len() != bytes
                || bytes == 0
                || bytes > super::TRANSFER
            {
                return Err(failure(1));
            }
            let (hash, _) = consume(file.file(), bytes, checkpoint, false)?;
            if hash != expected || !namespace::same(&stamp, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
            let record = Record {
                name: "ops-pair.tar".into(),
                stamp,
                sha256: hash.clone(),
                bytes,
            };
            let root = Reopened::open(self, budget)?;
            let checked = (|| {
                root.check()?;
                visible(root.file(), &record, budget)?;
                io(root.file().sync_all())?;
                root.check()?;
                checkpoint.check()
            })();
            let closed = root.finish();
            checked.and(closed)?;
            Ok((pins, hash, bytes, record))
        })();
        let closed = file.finish();
        let (pins, hash, bytes, record) = result.and_then(|v| closed.map(|()| v))?;
        self.records.insert(record.name.clone(), record);
        self.check(budget, checkpoint)?;
        Ok((pins, hash, bytes))
    }
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        let parent = self.parent.reopen(budget)?;
        let result = (|| {
            let root = Held::open(
                parent.file(),
                self.path.file_name().ok_or_else(|| failure(1))?,
                FLAGS | rustix::fs::OFlags::DIRECTORY,
                budget,
            )?;
            let result = (|| {
                if !namespace::binding(&self.stamp, &namespace::private_directory(&root)?) {
                    return Err(failure(1));
                }
                let mut expected = self.records.keys().cloned().collect::<BTreeSet<_>>();
                if self.acquired {
                    expected.insert("acquired".into());
                }
                if names(root.file())? != expected {
                    return Err(failure(1));
                }
                for record in self.records.values() {
                    read(root.file(), record, budget, checkpoint, false)?;
                }
                checkpoint.check()
            })();
            let closed = root.finish();
            result.and(closed)
        })();
        let closed = parent.finish();
        result.and(closed)
    }
}

#[cfg(test)]
pub(super) fn close_failure_probe(
    path: &Path,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<()> {
    let output = Output::new(path, budget)?;
    output
        .finish_with(checkpoint, |run| {
            run.finish()?;
            Err(failure(1))
        })
        .map(|_| ())
}
