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
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Clone)]
pub(super) struct Record {
    name: OsString,
    stamp: Metadata,
    sha256: String,
    bytes: u64,
}
#[derive(Clone)]
pub(super) struct Original {
    parent: ChainWitness,
    record: Record,
}

fn ordinary(parent: &File, name: &OsStr) -> Result<()> {
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
    pub(super) fn written(
        parent: ChainWitness,
        name: OsString,
        stamp: Metadata,
        sha256: String,
    ) -> Self {
        Self {
            parent,
            record: Record {
                name,
                bytes: stamp.len(),
                stamp,
                sha256,
            },
        }
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
        if stamp.mode() & 0o7777 != 0o600 || stamp.uid() != rustix::process::getuid().as_raw() {
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
        if !super::options::path(path) {
            return Err(failure(1));
        }
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = (|| {
            let name = path.file_name().ok_or_else(|| failure(1))?;
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
    pub fn read(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<Vec<u8>> {
        let parent = self.parent.reopen(budget)?;
        let result = read_record(parent.file(), &self.record, budget, checkpoint, true);
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let result = read_record(parent.file(), &self.record, budget, checkpoint, false);
        result.map(|_| ()).and(parent.finish())
    }
}

pub(super) struct Pending {
    maxima: [u64; 2],
    pub logs: Option<Arc<SeparateLogs>>,
    pub names: [String; 2],
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
    pub fn read(&self, name: &str, checkpoint: &Checkpoint) -> Result<Vec<u8>> {
        let record = self.records.get(name).ok_or_else(|| failure(1))?;
        let root = self.root.reopen(&self.budget)?;
        let result = read_record(root.file(), record, &self.budget, checkpoint, true);
        let closed = root.finish();
        result.and_then(|v| closed.map(|()| v))
    }
    pub fn ready_reader(&self, pending: &Pending) -> Result<ReadyReader> {
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let file = Held::open(
                root.file(),
                OsStr::new(&pending.names[0]),
                FLAGS,
                &self.budget,
            )?;
            let checked = io(file.file().metadata()).and_then(|stamp| {
                if namespace::same(&stamp, &pending.initial[0]) {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            if let Err(error) = checked {
                let _ = file.finish();
                return Err(error);
            }
            Ok(ReadyReader {
                file: Some(file),
                initial: pending.initial[0].clone(),
            })
        })();
        let closed = root.finish();
        match result {
            Ok(reader) => {
                if let Err(error) = closed {
                    let _ = reader.finish();
                    Err(error)
                } else {
                    Ok(reader)
                }
            }
            Err(error) => {
                let _ = closed;
                Err(error)
            }
        }
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
    pub fn input(&self, name: &str, checkpoint: &Checkpoint) -> Result<Input> {
        let record = self.records.get(name).ok_or_else(|| failure(1))?.clone();
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            read_record(root.file(), &record, &self.budget, checkpoint, false)?;
            let reader = Held::open(root.file(), OsStr::new(name), FLAGS, &self.budget)?;
            let admission = io(reader.file().metadata()).and_then(|stamp| {
                if namespace::same(&record.stamp, &stamp) {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            if let Err(error) = admission {
                let _ = reader.finish();
                return Err(error);
            }
            Ok(Input {
                reader: Some(reader),
                root: self.root.clone(),
                record,
                budget: self.budget.clone(),
            })
        })();
        let closed = root.finish();
        match result {
            Ok(input) => {
                if let Err(error) = closed {
                    let _ = input.finish(checkpoint);
                    Err(error)
                } else {
                    Ok(input)
                }
            }
            Err(error) => {
                let _ = closed;
                Err(error)
            }
        }
    }
    /// A framed request is sealed after all writers close. The archive is copied
    /// from its retained original read-only owner, never loaded into memory.
    pub fn frame(
        &mut self,
        name: &str,
        header: &[u8],
        transfer: Option<&Input>,
        request: &[u8],
        checkpoint: &Checkpoint,
    ) -> Result<Input> {
        if name.contains('/')
            || self.records.contains_key(name)
            || header.len() > 65536
            || request.len() > 524288
        {
            return Err(failure(1));
        }
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let writer = Held::create(root.file(), name, false, &self.budget)?;
            let written = (|| {
                let before = namespace::private_file(&writer, false)?;
                io(writer.file().write_all(header))?;
                if let Some(transfer) = transfer {
                    transfer.copy_to(&mut writer.file(), checkpoint)?;
                }
                io(writer.file().write_all(request))?;
                io(writer.file().sync_all())?;
                let after = namespace::private_file(&writer, false)?;
                let expected = (header.len() as u64)
                    .checked_add(request.len() as u64)
                    .and_then(|n| n.checked_add(transfer.map_or(0, Input::bytes)))
                    .ok_or_else(|| failure(1))?;
                if !namespace::binding(&before, &after)
                    || after.len() != expected
                    || expected > super::MAX_TRANSFER + 589824
                {
                    return Err(failure(1));
                }
                let record = Record {
                    name: name.into(),
                    bytes: expected,
                    stamp: after,
                    sha256: consume(writer.file(), expected, checkpoint, |_| Ok(()))?,
                };
                visible(root.file(), &record, &self.budget)?;
                Ok(record)
            })();
            let closed = writer.finish();
            written.and_then(|record| closed.map(|()| record))
        })();
        let closed = root.finish();
        let record = result.and_then(|record| closed.map(|()| record))?;
        self.records.insert(name.into(), record);
        self.input(name, checkpoint)
    }
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
        if bytes.is_empty()
            || bytes.len() as u64 > super::MAX_JSON
            || self.records.contains_key(name)
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
    pub async fn capture(&self, index: usize, maximum: u64) -> Result<Pending> {
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
                if let Err(error) = ordinary(root.file(), OsStr::new(name)) {
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
                logs.push(Log::start(
                    cloned,
                    if logs.is_empty() {
                        maximum
                    } else {
                        super::MAX_STDERR
                    },
                )?);
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
            maxima: [maximum, super::MAX_STDERR],
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
        collect: bool,
    ) -> Result<Vec<u8>> {
        let root = self.root.reopen(&self.budget)?;
        let result = (|| {
            let mut bytes = Vec::new();
            for (index, seal) in [&seals.stdout, &seals.stderr].into_iter().enumerate() {
                if !namespace::binding(&pending.initial[index], &seal.metadata)
                    || seal.metadata.mode() & 0o7777 != 0o600
                    || seal.metadata.nlink() != 1
                    || seal.bytes != seal.metadata.len()
                    || seal.bytes > pending.maxima[index]
                {
                    return Err(failure(1));
                }
                let record = Record {
                    name: pending.names[index].clone().into(),
                    stamp: seal.metadata.clone(),
                    sha256: seal.sha256.clone(),
                    bytes: seal.bytes,
                };
                let read = read_record(
                    root.file(),
                    &record,
                    &self.budget,
                    checkpoint,
                    index == 0 && collect,
                )?;
                if index == 0 {
                    bytes = read;
                }
                self.records.insert(pending.names[index].clone(), record);
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
                    || stamp.len() > super::MAX_TRANSFER
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
        self.records.insert("transfer.tar".into(), record);
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
    pub fn original(&self) -> Original {
        Original {
            parent: self.root.clone(),
            record: self.record.clone(),
        }
    }
    pub fn consumed(&self) -> Result<()> {
        let mut file = self.reader.as_ref().ok_or_else(|| failure(1))?.file();
        if io(file.stream_position())? == self.record.bytes {
            Ok(())
        } else {
            Err(failure(1))
        }
    }
    pub fn copy_to(&self, writer: &mut impl Write, checkpoint: &Checkpoint) -> Result<()> {
        self.check(checkpoint)?;
        let file = self.reader.as_ref().ok_or_else(|| failure(1))?.file();
        let sha = consume(file, self.record.bytes, checkpoint, |part| {
            io(writer.write_all(part))
        })?;
        if sha != self.record.sha256 {
            return Err(failure(1));
        }
        self.check(checkpoint)
    }
    pub fn file(&self) -> Result<&File> {
        self.reader
            .as_ref()
            .map(Held::file)
            .ok_or_else(|| failure(1))
    }
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

/// The original stdout inode is read only against the drainer's independently
/// observed accepted-byte progress. File growth or a supplied JSON record alone
/// cannot supply preparation readiness.
pub(super) struct ReadyReader {
    file: Option<Held>,
    initial: Metadata,
}
impl ReadyReader {
    pub fn read(&self, progress: (u64, &str)) -> Result<Option<Vec<u8>>> {
        use std::os::unix::fs::FileExt;
        let (count, expected) = progress;
        if count > super::MAX_STATUS {
            return Err(failure(1));
        }
        let file = self.file.as_ref().ok_or_else(|| failure(1))?.file();
        let before = io(file.metadata())?;
        if !namespace::binding(&self.initial, &before)
            || before.nlink() != 1
            || before.len() > super::MAX_STATUS
        {
            return Err(failure(1));
        }
        if before.len() != count {
            return Ok(None);
        }
        let mut bytes = vec![0; count as usize];
        let mut offset = 0usize;
        while offset < bytes.len() {
            let read = io(file.read_at(&mut bytes[offset..], offset as u64))?;
            if read == 0 {
                return Err(failure(1));
            }
            offset += read;
        }
        let after = io(file.metadata())?;
        if !namespace::binding(&self.initial, &after)
            || after.nlink() != 1
            || after.len() > super::MAX_STATUS
        {
            return Err(failure(1));
        }
        if after.len() != count {
            return Ok(None);
        }
        if super::super::sha(&bytes) != expected {
            return Err(failure(1));
        }
        let lines = bytes.iter().filter(|b| **b == b'\n').count();
        if lines > 1 || (lines == 1 && !bytes.ends_with(b"\n")) {
            return Err(failure(1));
        }
        if lines == 1 {
            Ok(Some(bytes))
        } else {
            Ok(None)
        }
    }
    pub fn finish(mut self) -> Result<()> {
        self.file.take().ok_or_else(|| failure(1))?.finish()
    }
}
