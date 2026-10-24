//! Original nofollow witnesses and private bounded transfer/capture files.

use super::super::namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun, Permit};
use super::{failure, io, worker::Checkpoint};
use crate::{
    ci::{bounded_log::Log, command::SeparateLogs},
    tooling::Result,
};
use rustix::fs::{AtFlags, FileType};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct Record {
    name: String,
    stamp: Metadata,
    sha256: String,
    bytes: u64,
}
#[derive(Clone)]
pub(super) struct Original {
    parent: ChainWitness,
    record: Record,
}

fn ordinary(parent: &File, name: &str) -> Result<()> {
    let stamp =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| failure(1))?;
    if FileType::from_raw_mode(stamp.st_mode) == FileType::RegularFile {
        Ok(())
    } else {
        Err(failure(1))
    }
}
fn visible(parent: &File, record: &Record, budget: &Budget) -> Result<()> {
    ordinary(parent, &record.name)?;
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let result = io(file.file().metadata()).and_then(|stamp| {
        if namespace::same(&record.stamp, &stamp) {
            Ok(())
        } else {
            Err(failure(1))
        }
    });
    result.and(file.finish())
}
fn consume(
    file: &File,
    count: u64,
    checkpoint: &Checkpoint,
    mut accept: impl FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    let mut file = file;
    io(file.seek(SeekFrom::Start(0)))?;
    let mut remaining = count;
    let mut block = [0; 65536];
    let mut hash = Sha256::new();
    while remaining != 0 {
        checkpoint.check()?;
        let size = remaining.min(block.len() as u64) as usize;
        let read = io(file.read(&mut block[..size]))?;
        if read == 0 {
            return Err(failure(1));
        }
        hash.update(&block[..read]);
        accept(&block[..read])?;
        remaining -= read as u64;
    }
    checkpoint.check()?;
    if io(file.read(&mut block[..1]))? != 0 {
        return Err(failure(1));
    }
    Ok(super::super::hex(&hash.finalize()))
}
fn read_record(
    parent: &File,
    record: &Record,
    budget: &Budget,
    checkpoint: &Checkpoint,
    collect: bool,
) -> Result<Vec<u8>> {
    ordinary(parent, &record.name)?;
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let result = (|| {
        if !namespace::same(&record.stamp, &io(file.file().metadata())?) {
            return Err(failure(1));
        }
        let mut bytes = Vec::new();
        if collect {
            bytes
                .try_reserve_exact(record.bytes as usize)
                .map_err(|_| failure(1))?;
        }
        let hash = consume(file.file(), record.bytes, checkpoint, |chunk| {
            if collect {
                bytes.extend_from_slice(chunk);
            }
            Ok(())
        })?;
        if hash != record.sha256 || !namespace::same(&record.stamp, &io(file.file().metadata())?) {
            return Err(failure(1));
        }
        visible(parent, record, budget)?;
        Ok(bytes)
    })();
    let closed = file.finish();
    result.and_then(|value| closed.map(|()| value))
}
impl Original {
    pub fn sha256(&self) -> &str {
        &self.record.sha256
    }
    pub fn bytes(&self) -> u64 {
        self.record.bytes
    }
    /// Seal a fresh private copy without reopening or rebasing its writer inode.
    pub fn create(
        parent: &ChainWitness,
        name: &str,
        bytes: &[u8],
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        Self::create_with(parent, name, bytes, budget, checkpoint, Held::finish)
    }
    fn create_with(
        parent: &ChainWitness,
        name: &str,
        bytes: &[u8],
        budget: &Budget,
        checkpoint: &Checkpoint,
        finish: impl FnOnce(Held) -> Result<()>,
    ) -> Result<Self> {
        if bytes.is_empty()
            || bytes.len() > 256 * 1024
            || name.is_empty()
            || name.contains('/')
            || matches!(name, "." | "..")
        {
            return Err(failure(1));
        }
        checkpoint.check()?;
        let directory = parent.reopen(budget)?;
        let result = (|| {
            let file = Held::create(directory.file(), name, false, budget)?;
            let result = (|| {
                let before = namespace::private_file(&file, false)?;
                for chunk in bytes.chunks(65536) {
                    checkpoint.check()?;
                    io(file.file().write_all(chunk))?;
                }
                io(file.file().sync_all())?;
                let after = namespace::private_file(&file, false)?;
                if !namespace::binding(&before, &after) || after.len() != bytes.len() as u64 {
                    return Err(failure(1));
                }
                let record = Record {
                    name: name.into(),
                    stamp: after,
                    sha256: super::super::sha(bytes),
                    bytes: bytes.len() as u64,
                };
                if consume(file.file(), record.bytes, checkpoint, |_| Ok(()))? != record.sha256
                    || !namespace::same(&record.stamp, &io(file.file().metadata())?)
                {
                    return Err(failure(1));
                }
                visible(directory.file(), &record, budget)?;
                Ok(Self {
                    parent: parent.clone(),
                    record,
                })
            })();
            let closed = finish(file);
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = directory.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn require_private(&self) -> Result<()> {
        let stamp = &self.record.stamp;
        if stamp.mode() & 0o7777 != 0o600
            || stamp.uid() != rustix::process::getuid().as_raw()
            || stamp.gid() != rustix::process::getgid().as_raw()
        {
            return Err(failure(1));
        }
        Ok(())
    }
    pub fn open(
        path: &Path,
        maximum: u64,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<(Self, Vec<u8>)> {
        if !super::request::path(path) {
            return Err(failure(1));
        }
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = (|| {
            let name = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or_else(|| failure(1))?;
            ordinary(parent.file(), name)?;
            let file = Held::open(parent.file(), OsStr::new(name), FLAGS, budget)?;
            let result = (|| {
                let stamp = io(file.file().metadata())?;
                if !stamp.is_file()
                    || stamp.nlink() != 1
                    || stamp.len() == 0
                    || stamp.len() > maximum
                {
                    return Err(failure(1));
                }
                let mut bytes = Vec::new();
                let hash = consume(file.file(), stamp.len(), checkpoint, |chunk| {
                    bytes.extend_from_slice(chunk);
                    Ok(())
                })?;
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
                Ok((
                    Self {
                        parent: parent.witness()?,
                        record,
                    },
                    bytes,
                ))
            })();
            let closed = file.finish();
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let result = read_record(parent.file(), &self.record, budget, checkpoint, false);
        result.map(|_| ()).and(parent.finish())
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;

pub(super) struct Pending {
    pub logs: Option<Arc<SeparateLogs>>,
    names: [String; 2],
    initial: [Metadata; 2],
    _permits: Vec<Permit>,
}
pub(super) struct Output {
    root: ChainWitness,
    pub path: PathBuf,
    budget: Budget,
    records: BTreeMap<String, Record>,
}
impl Output {
    pub fn witness(&self) -> &ChainWitness {
        &self.root
    }
    pub fn new(
        path: &Path,
        caller: &Path,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        let source = Chain::open(caller, budget)?;
        let parent = match Chain::open(path.parent().ok_or_else(|| failure(1))?, budget) {
            Ok(value) => value,
            Err(error) => {
                let _ = source.finish();
                return Err(error);
            }
        };
        let admitted = (|| {
            let witness = parent.witness()?;
            if source.witness()?.leaf_in(&witness) {
                return Err(failure(1));
            }
            Ok(witness)
        })();
        let closed = source.finish().and(parent.finish());
        let witness = admitted?;
        closed?;
        checkpoint.check()?;
        let run = OwnedRun::local_witness(path, budget, &witness)?;
        let result = (|| {
            for name in ["python-cache", "tmp", "home"] {
                run.directory(name)?.finish()?;
            }
            let chain = Chain::open(path, budget)?;
            let result = (|| {
                if !namespace::binding(&io(run.file().metadata())?, &io(chain.file().metadata())?) {
                    return Err(failure(1));
                }
                chain.witness()
            })();
            let closed = chain.finish();
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = run.finish();
        let witness = result.and_then(|value| closed.map(|()| value))?;
        Ok(Self {
            root: witness,
            path: path.into(),
            budget: budget.clone(),
            records: BTreeMap::new(),
        })
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        if bytes.is_empty() || bytes.len() as u64 > super::SMALL || self.records.contains_key(name)
        {
            return Err(failure(1));
        }
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let file = Held::create(root.file(), name, false, &self.budget)?;
            let result = (|| {
                let before = namespace::private_file(&file, false)?;
                io(file.file().write_all(bytes))?;
                io(file.file().sync_all())?;
                let after = namespace::private_file(&file, false)?;
                if !namespace::binding(&before, &after)
                    || after.nlink() != 1
                    || after.len() != bytes.len() as u64
                {
                    return Err(failure(1));
                }
                let record = Record {
                    name: name.into(),
                    stamp: after,
                    sha256: super::super::sha(bytes),
                    bytes: bytes.len() as u64,
                };
                if consume(file.file(), record.bytes, checkpoint, |_| Ok(()))? != record.sha256
                    || !namespace::same(&record.stamp, &io(file.file().metadata())?)
                {
                    return Err(failure(1));
                }
                visible(root.file(), &record, &self.budget)?;
                Ok(record)
            })();
            let closed = file.finish();
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = root.finish();
        let record = result.and_then(|value| closed.map(|()| value))?;
        self.records.insert(name.into(), record);
        Ok(())
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            for record in self.records.values() {
                read_record(root.file(), record, &self.budget, checkpoint, false)?;
            }
            root.verify()?;
            checkpoint.check()
        })();
        result.and(root.finish())
    }
    pub async fn capture(&self, index: usize) -> Result<Pending> {
        let root = self.root.reopen(&self.budget)?;
        let names = [
            format!("process-{index}.stdout"),
            format!("process-{index}.stderr"),
        ];
        let permits = match self.budget.reserve(8) {
            Ok(value) => value,
            Err(error) => {
                let _ = root.finish();
                return Err(error);
            }
        };
        let mut logs = Vec::new();
        let mut initial = Vec::new();
        let result = (|| {
            for name in &names {
                let file = Held::create(root.file(), name, false, &self.budget)?;
                let before = namespace::private_file(&file, false);
                // Log owns its File through joined finalization; release this
                // temporary owner only after admitting a separately opened FD.
                let stamp = match before {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = file.finish();
                        return Err(error);
                    }
                };
                if let Err(error) = ordinary(root.file(), name) {
                    let _ = file.finish();
                    return Err(error);
                }
                let copied = Held::open(
                    root.file(),
                    OsStr::new(name),
                    rustix::fs::OFlags::RDWR | FLAGS,
                    &self.budget,
                );
                let copied = match copied {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = file.finish();
                        return Err(error);
                    }
                };
                if !io(copied.file().metadata()).is_ok_and(|m| namespace::same(&stamp, &m)) {
                    let _ = file.finish();
                    let _ = copied.finish();
                    return Err(failure(1));
                }
                let cloned = io(copied.file().try_clone());
                let closed = file.finish().and(copied.finish());
                let cloned = match cloned {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = closed;
                        return Err(error);
                    }
                };
                if let Err(error) = closed {
                    let _ = nix::unistd::close(cloned);
                    return Err(error);
                }
                logs.push(Log::start(cloned, super::CAPTURE)?);
                initial.push(stamp);
            }
            Ok(())
        })();
        let closed = root.finish();
        if let Err(error) = result.and(closed) {
            for log in logs {
                let _ = log.finish().await;
            }
            return Err(error);
        }
        let stderr = logs.pop().unwrap();
        let stdout = logs.pop().unwrap();
        Ok(Pending {
            logs: Some(Arc::new(SeparateLogs { stdout, stderr })),
            names,
            initial: initial.try_into().map_err(|_| failure(1))?,
            _permits: permits,
        })
    }
    pub fn capture_read(
        &mut self,
        pending: Pending,
        seals: &super::tools::Captures,
        checkpoint: &Checkpoint,
    ) -> Result<Vec<u8>> {
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let mut bytes = Vec::new();
            for (index, seal) in [&seals.stdout, &seals.stderr].into_iter().enumerate() {
                if !namespace::binding(&pending.initial[index], &seal.metadata)
                    || seal.metadata.mode() & 0o7777 != 0o600
                    || seal.metadata.nlink() != 1
                    || seal.bytes != seal.metadata.len()
                    || seal.bytes > super::CAPTURE
                {
                    return Err(failure(1));
                }
                let record = Record {
                    name: pending.names[index].clone(),
                    stamp: seal.metadata.clone(),
                    sha256: seal.sha256.clone(),
                    bytes: seal.bytes,
                };
                let read = read_record(root.file(), &record, &self.budget, checkpoint, index == 0)?;
                if index == 0 {
                    bytes = read;
                }
                self.records.insert(record.name.clone(), record);
            }
            Ok(bytes)
        })();
        let closed = root.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn transfer(
        &mut self,
        authenticated: &super::super::acquire::AuthenticatedAcquisition,
        checkpoint: &Checkpoint,
    ) -> Result<(Input, super::super::acquire::PairPins)> {
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let writer = Held::create(root.file(), "transfer.tar", false, &self.budget)?;
            let copied = (|| {
                let before = namespace::private_file(&writer, false)?;
                let pins = authenticated.export_tar(
                    &mut writer.file(),
                    &self.budget,
                    &checkpoint.acquired(),
                )?;
                io(writer.file().sync_all())?;
                let stamp = namespace::private_file(&writer, false)?;
                if !namespace::binding(&before, &stamp)
                    || stamp.nlink() != 1
                    || stamp.len() == 0
                    || stamp.len() > super::TRANSFER
                {
                    return Err(failure(1));
                }
                let sha = consume(writer.file(), stamp.len(), checkpoint, |_| Ok(()))?;
                if !namespace::same(&stamp, &io(writer.file().metadata())?) {
                    return Err(failure(1));
                }
                let record = Record {
                    name: "transfer.tar".into(),
                    bytes: stamp.len(),
                    stamp,
                    sha256: sha,
                };
                visible(root.file(), &record, &self.budget)?;
                let reader =
                    Held::open(root.file(), OsStr::new("transfer.tar"), FLAGS, &self.budget)?;
                if !namespace::same(&record.stamp, &io(reader.file().metadata())?) {
                    let _ = reader.finish();
                    return Err(failure(1));
                }
                Ok((reader, record, pins))
            })();
            let closed = writer.finish();
            match copied {
                Ok((reader, record, pins)) => {
                    if let Err(error) = closed {
                        let _ = reader.finish();
                        return Err(error);
                    }
                    Ok((
                        Input {
                            reader: Some(reader),
                            root: self.root.clone(),
                            record: record.clone(),
                            budget: self.budget.clone(),
                        },
                        record,
                        pins,
                    ))
                }
                Err(error) => {
                    let _ = closed;
                    Err(error)
                }
            }
        })();
        let closed = root.finish();
        let (input, record, pins) = match result {
            Ok(value) => value,
            Err(error) => {
                let _ = closed;
                return Err(error);
            }
        };
        if let Err(error) = closed {
            let _ = input.finish(checkpoint);
            return Err(error);
        }
        self.records.insert(record.name.clone(), record);
        Ok((input, pins))
    }
}

pub(super) struct Input {
    reader: Option<Held>,
    root: ChainWitness,
    record: Record,
    budget: Budget,
}
impl Input {
    pub fn sha256(&self) -> &str {
        &self.record.sha256
    }
    pub fn bytes(&self) -> u64 {
        self.record.bytes
    }
    pub fn stamp(&self) -> &Metadata {
        &self.record.stamp
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        let file = self.reader.as_ref().ok_or_else(|| failure(1))?;
        if !namespace::same(&self.record.stamp, &io(file.file().metadata())?)
            || consume(file.file(), self.record.bytes, checkpoint, |_| Ok(()))?
                != self.record.sha256
            || !namespace::same(&self.record.stamp, &io(file.file().metadata())?)
        {
            return Err(failure(1));
        }
        let root = self.root.reopen(&self.budget)?;
        let result = visible(root.file(), &self.record, &self.budget);
        result.and(root.finish())
    }
    pub fn clone_input(&self, checkpoint: &Checkpoint) -> Result<File> {
        self.check(checkpoint)?;
        let mut reader = self.reader.as_ref().unwrap().file();
        io(reader.seek(SeekFrom::Start(0)))?;
        io(reader.try_clone())
    }
    pub fn finish(mut self, checkpoint: &Checkpoint) -> Result<()> {
        let checked = self.check(checkpoint);
        let closed = self
            .reader
            .take()
            .ok_or_else(|| failure(1))
            .and_then(Held::finish);
        checked.and(closed)
    }
}
