//! Original file/namespace witnesses and fresh readback-bound private output.

use super::super::namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun};
use super::{BUNDLE, SMALL, failure, hash::Checkpoint, io, request::Request, tools};
use crate::{
    ci::{bounded_log::Log, command::SeparateLogs},
    tooling::Result,
};
use rustix::fs::{Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) const METADATA: [&str; 4] = [
    "ops-release.json",
    "ops-evidence.json",
    "ops-predicate.json",
    "ops-preparation.json",
];

#[derive(Clone)]
pub(super) struct Record {
    pub name: String,
    pub stamp: Metadata,
    pub sha256: String,
    pub bytes: u64,
}
#[derive(Clone)]
pub(super) struct Original {
    pub parent: ChainWitness,
    pub record: Record,
}
fn regular(stamp: &Metadata, private: bool) -> bool {
    stamp.is_file()
        && stamp.nlink() == 1
        && (!private
            || (stamp.mode() & 0o7777 == 0o600
                && stamp.uid() == rustix::process::getuid().as_raw()))
}
fn private_directory(stamp: &Metadata) -> bool {
    stamp.is_dir()
        && stamp.mode() & 0o7777 == 0o700
        && stamp.uid() == rustix::process::getuid().as_raw()
}
fn visible(parent: &File, record: &Record, budget: &Budget) -> Result<()> {
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let checked = io(file.file().metadata()).and_then(|stamp| {
        if namespace::same(&record.stamp, &stamp) {
            Ok(())
        } else {
            Err(failure(1))
        }
    });
    checked.and(file.finish())
}
fn consume(
    file: &File,
    length: u64,
    checkpoint: &Checkpoint,
    mut accept: impl FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    let mut reader = file;
    io(reader.seek(SeekFrom::Start(0)))?;
    let mut left = length;
    let mut buffer = [0u8; 65536];
    let mut hash = Sha256::new();
    while left != 0 {
        checkpoint.check()?;
        let maximum = left.min(buffer.len() as u64) as usize;
        let count = io(reader.read(&mut buffer[..maximum]))?;
        if count == 0 {
            return Err(failure(1));
        }
        hash.update(&buffer[..count]);
        accept(&buffer[..count])?;
        left -= count as u64;
    }
    checkpoint.check()?;
    Ok(super::super::hex(&hash.finalize()))
}
fn read_at(
    parent: &File,
    name: &str,
    maximum: u64,
    private: bool,
    expected: Option<&Record>,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<(Record, Vec<u8>)> {
    let file = Held::open(parent, OsStr::new(name), FLAGS, budget)?;
    let result = (|| {
        let stamp = io(file.file().metadata())?;
        if !regular(&stamp, private)
            || stamp.len() == 0
            || stamp.len() > maximum
            || expected.is_some_and(|record| !namespace::same(&record.stamp, &stamp))
        {
            return Err(failure(1));
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(stamp.len() as usize)
            .map_err(|_| failure(1))?;
        let sha256 = consume(file.file(), stamp.len(), checkpoint, |part| {
            bytes.extend_from_slice(part);
            Ok(())
        })?;
        if !namespace::same(&stamp, &io(file.file().metadata())?)
            || expected.is_some_and(|r| r.sha256 != sha256 || r.bytes != stamp.len())
        {
            return Err(failure(1));
        }
        let record = Record {
            name: name.into(),
            bytes: stamp.len(),
            stamp,
            sha256,
        };
        visible(parent, &record, budget)?;
        Ok((record, bytes))
    })();
    let closed = file.finish();
    result.and_then(|value| closed.map(|()| value))
}
pub(super) fn original(
    path: &Path,
    maximum: u64,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<(Original, Vec<u8>)> {
    let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
    let result = (|| {
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| failure(1))?;
        let (record, bytes) = read_at(
            parent.file(),
            name,
            maximum,
            false,
            None,
            budget,
            checkpoint,
        )?;
        Ok((
            Original {
                parent: parent.witness()?,
                record,
            },
            bytes,
        ))
    })();
    let closed = parent.finish();
    result.and_then(|value| closed.map(|()| value))
}
impl Original {
    pub fn read(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<Vec<u8>> {
        let parent = self.parent.reopen(budget)?;
        let result = read_at(
            parent.file(),
            &self.record.name,
            self.record.bytes,
            false,
            Some(&self.record),
            budget,
            checkpoint,
        )
        .map(|(_, bytes)| bytes);
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
}
pub(super) fn inventory(
    parent: &File,
    budget: &Budget,
    maximum: usize,
) -> Result<BTreeSet<String>> {
    let _cursor = budget.permit()?;
    // Dir owns a duplicated descriptor; its cursor close is RAII/unobservable.
    let cursor = rustix::fs::Dir::read_from(parent).map_err(|_| failure(1))?;
    let mut names = BTreeSet::new();
    for entry in cursor {
        let entry = entry.map_err(|_| failure(1))?;
        let name = entry.file_name().to_str().map_err(|_| failure(1))?;
        if matches!(name, "." | "..") {
            continue;
        }
        if name.len() > 128 || names.len() >= maximum || !names.insert(name.into()) {
            return Err(failure(1));
        }
    }
    Ok(names)
}

#[derive(Clone)]
pub(super) struct Source {
    pub witness: ChainWitness,
    pub records: BTreeMap<String, Record>,
}
impl Source {
    pub fn output_parent(&self, path: &Path, budget: &Budget) -> Result<ChainWitness> {
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = parent.witness().and_then(|witness| {
            if self.witness.leaf_in(&witness) {
                Err(failure(1))
            } else {
                Ok(witness)
            }
        });
        let closed = parent.finish();
        result.and_then(|witness| closed.map(|()| witness))
    }
    pub fn small(
        request: &Request,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<(Self, BTreeMap<String, Vec<u8>>)> {
        let root = Chain::open(&request.directory, budget)?;
        let result = (|| {
            if !private_directory(&io(root.file().metadata())?) {
                return Err(failure(1));
            }
            let mut records = BTreeMap::new();
            let mut data = BTreeMap::new();
            for name in METADATA.into_iter().chain([BUNDLE]) {
                let maximum = if name == BUNDLE {
                    super::MAX_BUNDLE
                } else {
                    SMALL
                };
                let (record, bytes) =
                    read_at(root.file(), name, maximum, true, None, budget, checkpoint)?;
                records.insert(name.into(), record);
                data.insert(name.into(), bytes);
            }
            Ok((
                Self {
                    witness: root.witness()?,
                    records,
                },
                data,
            ))
        })();
        let closed = root.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn names(&self, budget: &Budget, expected: &BTreeSet<String>) -> Result<()> {
        let root = self.witness.reopen(budget)?;
        let result = inventory(root.file(), budget, 22).and_then(|names| {
            if &names == expected {
                Ok(())
            } else {
                Err(failure(1))
            }
        });
        result.and(root.finish())
    }
    pub fn copy(
        &mut self,
        output: &mut Output,
        name: &str,
        sha: &str,
        bytes: u64,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        let budget = output.run.budget();
        let root = self.witness.reopen(budget)?;
        let result = (|| {
            let source = Held::open(root.file(), OsStr::new(name), FLAGS, budget)?;
            let result = (|| {
                let stamp = io(source.file().metadata())?;
                if !regular(&stamp, true)
                    || stamp.len() != bytes
                    || bytes == 0
                    || self
                        .records
                        .get(name)
                        .is_some_and(|r| !namespace::same(&r.stamp, &stamp))
                {
                    return Err(failure(1));
                }
                let target = Held::create(output.run.file(), name, false, budget)?;
                let result = (|| {
                    let fresh = namespace::private_file(&target, false)?;
                    let mut writer = target.file();
                    let digest = consume(source.file(), bytes, checkpoint, |part| {
                        io(writer.write_all(part))
                    })?;
                    if digest != sha || !namespace::same(&stamp, &io(source.file().metadata())?) {
                        return Err(failure(1));
                    }
                    let original = Record {
                        name: name.into(),
                        stamp,
                        sha256: digest,
                        bytes,
                    };
                    visible(root.file(), &original, budget)?;
                    io(writer.sync_all())?;
                    let written = namespace::private_file(&target, false)?;
                    if !namespace::binding(&fresh, &written) || written.len() != bytes {
                        return Err(failure(1));
                    }
                    let digest = consume(target.file(), bytes, checkpoint, |_| Ok(()))?;
                    if digest != sha || !namespace::same(&written, &io(target.file().metadata())?) {
                        return Err(failure(1));
                    }
                    let record = Record {
                        name: name.into(),
                        stamp: written,
                        sha256: digest,
                        bytes,
                    };
                    visible(output.run.file(), &record, budget)?;
                    Ok((original, record))
                })();
                let closed = target.finish();
                result.and_then(|value| closed.map(|()| value))
            })();
            let closed = source.finish();
            result.and_then(|value| closed.map(|()| value))
        })();
        let closed = root.finish();
        let (original, written) = result.and_then(|value| closed.map(|()| value))?;
        self.records.insert(name.into(), original);
        output.records.insert(name.into(), written);
        output.check()
    }
    pub fn frame(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<String> {
        let root = self.witness.reopen(budget)?;
        let result = frame(root.file(), &self.records, budget, checkpoint);
        let closed = root.finish();
        result.and_then(|value| closed.map(|()| value))
    }
}

fn frame(
    parent: &File,
    records: &BTreeMap<String, Record>,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<String> {
    let mut frame = Sha256::new();
    frame.update(b"ops-retained-flat-v1\0");
    for (name, record) in records {
        checkpoint.check()?;
        let file = Held::open(parent, OsStr::new(name), FLAGS, budget)?;
        let checked = (|| {
            let stamp = io(file.file().metadata())?;
            if !regular(&stamp, true) || !namespace::same(&record.stamp, &stamp) {
                return Err(failure(1));
            }
            frame.update((name.len() as u32).to_be_bytes());
            frame.update(name.as_bytes());
            frame.update(record.bytes.to_be_bytes());
            let digest = consume(file.file(), record.bytes, checkpoint, |part| {
                frame.update(part);
                Ok(())
            })?;
            if digest != record.sha256 || !namespace::same(&stamp, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
            visible(parent, record, budget)
        })();
        checked.and(file.finish())?;
    }
    Ok(super::super::hex(&frame.finalize()))
}

pub(super) struct Output {
    pub run: OwnedRun,
    work: Vec<(String, Held, Metadata)>,
    pub records: BTreeMap<String, Record>,
}

/// Original namespace and byte observations retained after explicit descriptor
/// closure. Reopening compares against these observations, never a new baseline.
#[derive(Clone)]
pub(in crate::ci::ops_artifact) struct OutputWitness {
    root: ChainWitness,
    records: BTreeMap<String, Record>,
    work: Vec<(String, Metadata)>,
}
impl OutputWitness {
    pub(in crate::ci::ops_artifact) fn check(
        &self,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        checkpoint.check()?;
        let root = self.root.reopen(budget)?;
        let result = (|| {
            let mut expected = self.records.keys().cloned().collect::<BTreeSet<_>>();
            expected.insert("work".into());
            if inventory(root.file(), budget, 32)? != expected {
                return Err(failure(1));
            }
            let work = Held::open(
                root.file(),
                OsStr::new("work"),
                FLAGS | OFlags::DIRECTORY,
                budget,
            )?;
            let checked = (|| {
                let (_, original) = self.work.first().ok_or_else(|| failure(1))?;
                if !namespace::binding(original, &namespace::private_directory(&work)?) {
                    return Err(failure(1));
                }
                for (name, original) in self.work.iter().skip(1) {
                    checkpoint.check()?;
                    let child = Held::open(
                        work.file(),
                        OsStr::new(name),
                        FLAGS | OFlags::DIRECTORY,
                        budget,
                    )?;
                    let matched = namespace::private_directory(&child).and_then(|observed| {
                        if namespace::binding(original, &observed) {
                            Ok(())
                        } else {
                            Err(failure(1))
                        }
                    });
                    matched.and(child.finish())?;
                }
                Ok(())
            })();
            checked.and(work.finish())?;
            frame(root.file(), &self.records, budget, checkpoint)?;
            root.verify()?;
            checkpoint.check()
        })();
        result.and(root.finish())
    }
}
impl Output {
    pub fn witness(&self) -> Result<OutputWitness> {
        self.check()?;
        let chain = Chain::open(&self.run.path, self.run.budget())?;
        let result = (|| {
            if !namespace::binding(
                &io(self.run.file().metadata())?,
                &io(chain.file().metadata())?,
            ) {
                return Err(failure(1));
            }
            let witness = OutputWitness {
                root: chain.witness()?,
                records: self.records.clone(),
                work: self
                    .work
                    .iter()
                    .map(|(name, _, stamp)| (name.clone(), stamp.clone()))
                    .collect(),
            };
            self.check()?;
            Ok(witness)
        })();
        let closed = chain.finish();
        result.and_then(|witness| closed.map(|()| witness))
    }
    #[cfg(test)]
    pub fn new(path: &Path, budget: &Budget) -> Result<Self> {
        let run = OwnedRun::local(path, budget)?;
        Self::from_run(run)
    }
    pub fn bound(path: &Path, budget: &Budget, parent: &ChainWitness) -> Result<Self> {
        Self::from_run(OwnedRun::local_witness(path, budget, parent)?)
    }
    fn from_run(run: OwnedRun) -> Result<Self> {
        let budget = run.budget().clone();
        let mut value = Self {
            run,
            work: Vec::new(),
            records: BTreeMap::new(),
        };
        let result = (|| {
            let work = value.run.directory("work")?;
            let stamp = namespace::private_directory(&work)?;
            value.work.push(("work".into(), work, stamp));
            for name in ["home", "gh-config", "config", "cache", "tmp"] {
                let parent = value.work[0].1.file();
                rustix::fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700))
                    .map_err(|_| failure(1))?;
                let file =
                    Held::open(parent, OsStr::new(name), FLAGS | OFlags::DIRECTORY, &budget)?;
                let stamp = namespace::private_directory(&file);
                match stamp {
                    Ok(stamp) => value.work.push((name.into(), file, stamp)),
                    Err(error) => {
                        let _ = file.finish();
                        return Err(error);
                    }
                }
            }
            value.check()
        })();
        if let Err(error) = result {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        self.run.check()?;
        for (index, (name, file, stamp)) in self.work.iter().enumerate() {
            let parent = if index == 0 {
                self.run.file()
            } else {
                self.work[0].1.file()
            };
            let visible = Held::open(
                parent,
                OsStr::new(name),
                FLAGS | OFlags::DIRECTORY,
                self.run.budget(),
            )?;
            let checked = (|| {
                let observed = namespace::private_directory(&visible)?;
                if !namespace::binding(stamp, &observed)
                    || !namespace::binding(stamp, &namespace::private_directory(file)?)
                {
                    return Err(failure(1));
                }
                Ok(())
            })();
            checked.and(visible.finish())?;
        }
        Ok(())
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], checkpoint: &Checkpoint) -> Result<()> {
        self.check()?;
        if self.records.contains_key(name)
            || name.is_empty()
            || name.len() > 128
            || matches!(name, "." | "..")
            || bytes.is_empty()
            || bytes.len() as u64 > super::MAX_BUNDLE
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
        {
            return Err(failure(1));
        }
        let file = Held::create(self.run.file(), name, false, self.run.budget())?;
        let result = (|| {
            let initial = namespace::private_file(&file, false)?;
            let mut writer = file.file();
            for part in bytes.chunks(65536) {
                checkpoint.check()?;
                io(writer.write_all(part))?;
            }
            io(writer.sync_all())?;
            let stamp = namespace::private_file(&file, false)?;
            if !namespace::binding(&initial, &stamp) || stamp.len() != bytes.len() as u64 {
                return Err(failure(1));
            }
            let mut offset = 0;
            let sha256 = consume(file.file(), stamp.len(), checkpoint, |part| {
                if bytes.get(offset..offset + part.len()) != Some(part) {
                    return Err(failure(1));
                }
                offset += part.len();
                Ok(())
            })?;
            if !namespace::same(&stamp, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
            let record = Record {
                name: name.into(),
                stamp,
                bytes: bytes.len() as u64,
                sha256,
            };
            visible(self.run.file(), &record, self.run.budget())?;
            Ok(record)
        })();
        let closed = file.finish();
        let record = result.and_then(|record| closed.map(|()| record))?;
        self.records.insert(name.into(), record);
        self.check()
    }
    pub async fn capture(
        &self,
        index: usize,
        maximum: u64,
    ) -> Result<(Arc<SeparateLogs>, [String; 2], Vec<namespace::Permit>)> {
        self.capture_observed(index, maximum, |_| {}).await
    }
    pub(super) async fn capture_observed(
        &self,
        index: usize,
        maximum: u64,
        mut admitted_log: impl FnMut(&Log),
    ) -> Result<(Arc<SeparateLogs>, [String; 2], Vec<namespace::Permit>)> {
        self.check()?;
        let names = [format!("gh-{index}.stdout"), format!("gh-{index}.stderr")];
        let permits = self.run.budget().reserve(8)?;
        let mut logs = Vec::new();
        let admitted = (|| {
            for (name, cap) in names.iter().zip([maximum, SMALL]) {
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
                .map_err(|_| failure(1));
                let file = match file {
                    Ok(file) => file,
                    Err(error) => return Err(error),
                };
                let admitted = io(file.metadata()).and_then(|stamp| {
                    if regular(&stamp, true) && stamp.len() == 0 {
                        Ok(())
                    } else {
                        Err(failure(1))
                    }
                });
                if let Err(error) = admitted {
                    let _ = nix::unistd::close(file);
                    return Err(error);
                }
                let log = Log::start(file, cap)?;
                admitted_log(&log);
                logs.push(log);
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            for log in logs {
                let _ = log.finish().await;
            }
            return Err(error);
        }
        let stderr = logs.pop().unwrap();
        let stdout = logs.pop().unwrap();
        Ok((Arc::new(SeparateLogs { stdout, stderr }), names, permits))
    }
    pub fn capture_read(
        &mut self,
        names: &[String; 2],
        seals: &tools::Captures,
        checkpoint: &Checkpoint,
    ) -> Result<Vec<u8>> {
        let mut stdout = None;
        for (name, seal) in names.iter().zip([&seals.stdout, &seals.stderr]) {
            let record = Record {
                name: name.clone(),
                stamp: seal.metadata.clone(),
                sha256: seal.sha256.clone(),
                bytes: seal.bytes,
            };
            let data = self.read(&record, checkpoint, true)?;
            self.records.insert(name.clone(), record);
            if stdout.is_none() {
                stdout = Some(data);
            }
        }
        stdout.ok_or_else(|| failure(1))
    }
    pub fn read(&self, record: &Record, checkpoint: &Checkpoint, empty: bool) -> Result<Vec<u8>> {
        self.check()?;
        if !regular(&record.stamp, true) {
            return Err(failure(1));
        }
        if empty && record.bytes == 0 {
            visible(self.run.file(), record, self.run.budget())?;
            if record.sha256 != super::super::sha(&[]) {
                return Err(failure(1));
            }
            return Ok(Vec::new());
        }
        read_at(
            self.run.file(),
            &record.name,
            record.bytes,
            true,
            Some(record),
            self.run.budget(),
            checkpoint,
        )
        .map(|(_, bytes)| bytes)
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.run.path.join(name)
    }
    pub fn frame(&self, selected: &BTreeSet<String>, checkpoint: &Checkpoint) -> Result<String> {
        self.check()?;
        let records = selected
            .iter()
            .map(|name| {
                self.records
                    .get(name)
                    .cloned()
                    .map(|r| (name.clone(), r))
                    .ok_or_else(|| failure(1))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        frame(self.run.file(), &records, self.run.budget(), checkpoint)
    }
    pub fn verify_all(&self, checkpoint: &Checkpoint) -> Result<()> {
        self.check()?;
        let mut names = self.records.keys().cloned().collect::<BTreeSet<_>>();
        names.insert("work".into());
        if inventory(self.run.file(), self.run.budget(), 32)? != names {
            return Err(failure(1));
        }
        frame(
            self.run.file(),
            &self.records,
            self.run.budget(),
            checkpoint,
        )?;
        self.check()
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let mut closed = Ok(());
        for (_, file, _) in self.work.into_iter().rev() {
            closed = closed.and(file.finish());
        }
        let run = self.run.finish();
        checked.and(closed).and(run)
    }
}
