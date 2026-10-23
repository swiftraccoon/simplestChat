//! Original namespace/file witnesses and streamed private acquisition storage.

use super::super::namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun, Permit};
use super::{archive, failure, hash::Checkpoint, io, tools::Captures};
use crate::{
    ci::{bounded_log::Log, command::SeparateLogs},
    tooling::Result,
};
use rustix::fs::{AtFlags, FileType, Mode, OFlags};
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

#[derive(Clone)]
pub(super) struct Record {
    pub name: String,
    pub stamp: Metadata,
    pub sha256: String,
    pub bytes: u64,
}
fn regular(stamp: &Metadata) -> bool {
    stamp.is_file()
        && stamp.nlink() == 1
        && stamp.mode() & 0o7777 == 0o600
        && stamp.uid() == rustix::process::getuid().as_raw()
}
fn ordinary(parent: &File, name: &str) -> Result<()> {
    let stamp =
        rustix::fs::statat(parent, name, AtFlags::SYMLINK_NOFOLLOW).map_err(|_| failure(1))?;
    if FileType::from_raw_mode(stamp.st_mode) != FileType::RegularFile {
        Err(failure(1))
    } else {
        Ok(())
    }
}
fn visible(parent: &File, record: &Record, budget: &Budget) -> Result<()> {
    ordinary(parent, &record.name)?;
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
    bytes: u64,
    checkpoint: &Checkpoint,
    mut accept: impl FnMut(&[u8]) -> Result<()>,
) -> Result<String> {
    let mut reader = file;
    io(reader.seek(SeekFrom::Start(0)))?;
    let mut left = bytes;
    let mut buffer = [0u8; 65536];
    let mut hash = Sha256::new();
    while left != 0 {
        checkpoint.check()?;
        let count = io(reader.read(&mut buffer[..left.min(65536) as usize]))?;
        if count == 0 {
            return Err(failure(1));
        }
        hash.update(&buffer[..count]);
        accept(&buffer[..count])?;
        left -= count as u64;
    }
    checkpoint.check()?;
    if io(reader.read(&mut buffer[..1]))? != 0 {
        return Err(failure(1));
    }
    Ok(super::super::hex(&hash.finalize()))
}
fn check_record(
    parent: &File,
    record: &Record,
    budget: &Budget,
    checkpoint: &Checkpoint,
    collect: bool,
) -> Result<Vec<u8>> {
    ordinary(parent, &record.name)?;
    let file = Held::open(parent, OsStr::new(&record.name), FLAGS, budget)?;
    let result = (|| {
        let stamp = io(file.file().metadata())?;
        if !stamp.is_file()
            || stamp.nlink() != 1
            || !namespace::same(&record.stamp, &stamp)
            || stamp.len() != record.bytes
            || (collect && record.bytes > super::SMALL)
        {
            return Err(failure(1));
        }
        let mut bytes = Vec::new();
        if collect {
            bytes
                .try_reserve_exact(record.bytes as usize)
                .map_err(|_| failure(1))?;
        }
        let sha = consume(file.file(), record.bytes, checkpoint, |part| {
            if collect {
                bytes.extend_from_slice(part);
            }
            Ok(())
        })?;
        if sha != record.sha256 || !namespace::same(&stamp, &io(file.file().metadata())?) {
            return Err(failure(1));
        }
        visible(parent, record, budget)?;
        Ok(bytes)
    })();
    let closed = file.finish();
    result.and_then(|value| closed.map(|()| value))
}
fn names(parent: &File, budget: &Budget, maximum: usize) -> Result<BTreeSet<String>> {
    let _permit = budget.permit()?;
    let cursor = rustix::fs::Dir::read_from(parent).map_err(|_| failure(1))?;
    let mut names = BTreeSet::new();
    for entry in cursor {
        let entry = entry.map_err(|_| failure(1))?;
        let name = entry.file_name().to_str().map_err(|_| failure(1))?;
        if matches!(name, "." | "..") {
            continue;
        }
        if names.len() >= maximum || name.len() > 128 || !names.insert(name.into()) {
            return Err(failure(1));
        }
    }
    Ok(names)
}

#[derive(Clone)]
pub(super) struct Original {
    parent: ChainWitness,
    record: Record,
}
impl Original {
    pub fn read(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<Vec<u8>> {
        let parent = self.parent.reopen(budget)?;
        let result = check_record(parent.file(), &self.record, budget, checkpoint, true);
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
}
pub(super) fn original(
    path: &Path,
    budget: &Budget,
    checkpoint: &Checkpoint,
) -> Result<(Original, Vec<u8>)> {
    let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
    let result = (|| {
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| failure(1))?;
        ordinary(parent.file(), name)?;
        let file = Held::open(parent.file(), OsStr::new(name), FLAGS, budget)?;
        let read = (|| {
            let stamp = io(file.file().metadata())?;
            if !stamp.is_file()
                || stamp.nlink() != 1
                || stamp.len() == 0
                || stamp.len() > super::request::MAXIMUM
            {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            let sha = consume(file.file(), stamp.len(), checkpoint, |part| {
                bytes.extend_from_slice(part);
                Ok(())
            })?;
            if !namespace::same(&stamp, &io(file.file().metadata())?) {
                return Err(failure(1));
            }
            let record = Record {
                name: name.into(),
                bytes: stamp.len(),
                stamp,
                sha256: sha,
            };
            visible(parent.file(), &record, budget)?;
            Ok((record, bytes))
        })();
        let closed = file.finish();
        let (record, bytes) = read.and_then(|value| closed.map(|()| value))?;
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

pub(super) struct Pending {
    pub logs: Option<Arc<SeparateLogs>>,
    names: [String; 2],
    initial: [Metadata; 2],
    _permits: Vec<Permit>,
}
pub(super) struct Output {
    run: OwnedRun,
    work: Vec<(String, Held, Metadata)>,
    input: Option<(Held, Metadata)>,
    records: BTreeMap<String, Record>,
    payloads: BTreeMap<String, Record>,
}
impl Output {
    pub fn new(path: &Path, budget: &Budget) -> Result<Self> {
        let run = OwnedRun::local(path, budget)?;
        let mut value = Self {
            run,
            work: Vec::new(),
            input: None,
            records: BTreeMap::new(),
            payloads: BTreeMap::new(),
        };
        let prepared = (|| {
            let work = value.run.directory("work")?;
            let stamp = match namespace::private_directory(&work) {
                Ok(stamp) => stamp,
                Err(error) => {
                    let _ = work.finish();
                    return Err(error);
                }
            };
            value.work.push(("work".into(), work, stamp));
            for name in ["home", "gh-config", "config", "cache", "tmp"] {
                let parent = value.work[0].1.file();
                rustix::fs::mkdirat(parent, name, Mode::from_bits_truncate(0o700))
                    .map_err(|_| failure(1))?;
                let file = Held::open(parent, OsStr::new(name), FLAGS | OFlags::DIRECTORY, budget)?;
                let admitted = namespace::private_directory(&file);
                match admitted {
                    Ok(stamp) => value.work.push((name.into(), file, stamp)),
                    Err(error) => {
                        let _ = file.finish();
                        return Err(error);
                    }
                }
            }
            let input = value.run.directory("input")?;
            let stamp = match namespace::private_directory(&input) {
                Ok(stamp) => stamp,
                Err(error) => {
                    let _ = input.finish();
                    return Err(error);
                }
            };
            value.input = Some((input, stamp));
            value.check()
        })();
        if let Err(error) = prepared {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn path(&self, name: &str) -> PathBuf {
        self.run.path.join(name)
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
            let checked = namespace::private_directory(&visible).and_then(|other| {
                if namespace::binding(stamp, &other)
                    && namespace::binding(stamp, &namespace::private_directory(file)?)
                {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            checked.and(visible.finish())?;
        }
        if let Some((file, stamp)) = &self.input {
            let visible = Held::open(
                self.run.file(),
                OsStr::new("input"),
                FLAGS | OFlags::DIRECTORY,
                self.run.budget(),
            )?;
            let checked = namespace::private_directory(&visible).and_then(|other| {
                if namespace::binding(stamp, &other)
                    && namespace::binding(stamp, &namespace::private_directory(file)?)
                {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            checked.and(visible.finish())?;
        }
        Ok(())
    }
    pub async fn capture(&self, index: usize, maximum: u64) -> Result<Pending> {
        self.capture_observed(index, maximum, |_| {}).await
    }
    pub(super) async fn capture_observed(
        &self,
        index: usize,
        maximum: u64,
        mut observed: impl FnMut(&Log),
    ) -> Result<Pending> {
        self.check()?;
        let names = [
            if index == 4 {
                "artifact.zip".into()
            } else {
                format!("gh-{index}.stdout")
            },
            format!("gh-{index}.stderr"),
        ];
        let permits = self.run.budget().reserve(8)?;
        let mut logs = Vec::new();
        let mut initial = Vec::new();
        let admitted = (|| {
            for (name, maximum) in names.iter().zip([maximum, super::SMALL]) {
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
                let stamp = io(file.metadata()).and_then(|stamp| {
                    if regular(&stamp)
                        && stamp.len() == 0
                        && stamp.gid() == io(self.run.file().metadata())?.gid()
                    {
                        Ok(stamp)
                    } else {
                        Err(failure(1))
                    }
                });
                let stamp = match stamp {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = nix::unistd::close(file);
                        return Err(error);
                    }
                };
                let log = Log::start(file, maximum)?;
                observed(&log);
                logs.push(log);
                initial.push(stamp);
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
        captures: &Captures,
        checkpoint: &Checkpoint,
        collect: bool,
    ) -> Result<Vec<u8>> {
        let mut stdout = Vec::new();
        for (index, (name, seal)) in pending
            .names
            .iter()
            .zip([&captures.stdout, &captures.stderr])
            .enumerate()
        {
            if !namespace::binding(&pending.initial[index], &seal.metadata)
                || !regular(&seal.metadata)
                || seal.bytes != seal.metadata.len()
            {
                return Err(failure(1));
            }
            let record = Record {
                name: name.clone(),
                stamp: seal.metadata.clone(),
                sha256: seal.sha256.clone(),
                bytes: seal.bytes,
            };
            let bytes = check_record(
                self.run.file(),
                &record,
                self.run.budget(),
                checkpoint,
                collect && index == 0,
            )?;
            self.records.insert(name.clone(), record);
            if index == 0 {
                stdout = bytes;
            }
        }
        self.check()?;
        Ok(stdout)
    }
    pub fn extract(
        &mut self,
        expected_sha: &str,
        expected_bytes: u64,
        checkpoint: &Checkpoint,
    ) -> Result<archive::Inventory> {
        self.check()?;
        let record = self.records.get("artifact.zip").ok_or_else(|| failure(1))?;
        if record.sha256 != expected_sha || record.bytes != expected_bytes {
            return Err(failure(1));
        }
        check_record(
            self.run.file(),
            record,
            self.run.budget(),
            checkpoint,
            false,
        )?;
        ordinary(self.run.file(), "artifact.zip")?;
        let _permit = self.run.budget().permit()?;
        let mut zip = rustix::fs::openat(self.run.file(), "artifact.zip", FLAGS, Mode::empty())
            .map(File::from)
            .map_err(|_| failure(1))?;
        let admitted = io(zip.metadata()).and_then(|stamp| {
            if namespace::same(&stamp, &record.stamp) {
                Ok(())
            } else {
                Err(failure(1))
            }
        });
        let extracted = match admitted {
            Ok(()) => match self.input.as_ref() {
                Some((parent, _)) => {
                    let mut sink = Materializer {
                        parent: parent.file(),
                        budget: self.run.budget(),
                        checkpoint,
                        active: None,
                        records: BTreeMap::new(),
                    };
                    let result = archive::extract(&mut zip, &mut sink, &mut || checkpoint.check());
                    let closed = sink.close_active();
                    self.payloads = std::mem::take(&mut sink.records);
                    result.and_then(|inventory| closed.map(|()| inventory))
                }
                None => Err(failure(1)),
            },
            Err(error) => Err(error),
        };
        let checked = io(zip.metadata()).and_then(|stamp| {
            if namespace::same(&record.stamp, &stamp) {
                Ok(())
            } else {
                Err(failure(1))
            }
        });
        let closed = nix::unistd::close(zip).map_err(|_| failure(1));
        let inventory = extracted.and_then(|value| checked.and(closed).map(|()| value))?;
        check_record(
            self.run.file(),
            record,
            self.run.budget(),
            checkpoint,
            false,
        )?;
        self.check()?;
        Ok(inventory)
    }
    pub fn small_payload(&self, name: &str, checkpoint: &Checkpoint) -> Result<Vec<u8>> {
        let parent = self.input.as_ref().ok_or_else(|| failure(1))?.0.file();
        let record = self.payloads.get(name).ok_or_else(|| failure(1))?;
        check_record(parent, record, self.run.budget(), checkpoint, true)
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], checkpoint: &Checkpoint) -> Result<()> {
        self.check()?;
        if bytes.is_empty() || bytes.len() as u64 > super::SMALL || self.records.contains_key(name)
        {
            return Err(failure(1));
        }
        checkpoint.check()?;
        let stamp = namespace::write(self.run.file(), name, bytes, self.run.budget())?;
        let record = Record {
            name: name.into(),
            stamp,
            sha256: super::super::sha(bytes),
            bytes: bytes.len() as u64,
        };
        check_record(
            self.run.file(),
            &record,
            self.run.budget(),
            checkpoint,
            true,
        )?;
        self.records.insert(name.into(), record);
        self.check()
    }
    pub fn finish(self) -> Result<Witness> {
        let checked = self.check();
        let root = self.run.parent.witness();
        let root_stamp = io(self.run.file().metadata());
        let path = self.run.path.clone();
        let input_stamp = self.input.as_ref().map(|(_, stamp)| stamp.clone());
        let work = self
            .work
            .iter()
            .map(|(name, _, stamp)| (name.clone(), stamp.clone()))
            .collect();
        let mut closed = Ok(());
        if let Some((input, _)) = self.input {
            closed = closed.and(input.finish());
        }
        for (_, file, _) in self.work.into_iter().rev() {
            closed = closed.and(file.finish());
        }
        let run = self.run.finish();
        checked?;
        closed?;
        run?;
        Ok(Witness {
            parent: root?,
            path,
            stamp: root_stamp?,
            input_stamp,
            work,
            records: self.records,
            payloads: self.payloads,
        })
    }
}

struct Active {
    file: Held,
    initial: Metadata,
    name: String,
    expected: u64,
    bytes: u64,
}
struct Materializer<'a> {
    parent: &'a File,
    budget: &'a Budget,
    checkpoint: &'a Checkpoint,
    active: Option<Active>,
    records: BTreeMap<String, Record>,
}
impl Materializer<'_> {
    fn close_active(&mut self) -> Result<()> {
        self.active
            .take()
            .map(|value| value.file.finish())
            .transpose()
            .map(|_| ())
    }
}
impl archive::Sink for Materializer<'_> {
    fn begin(&mut self, member: &archive::Member) -> Result<()> {
        if self.active.is_some() || self.records.contains_key(member.name()) {
            return Err(failure(1));
        }
        self.checkpoint.check()?;
        let file = Held::create(self.parent, member.name(), false, self.budget)?;
        let initial = namespace::private_file(&file, false);
        let initial = match initial {
            Ok(stamp) => stamp,
            Err(error) => {
                let _ = file.finish();
                return Err(error);
            }
        };
        self.active = Some(Active {
            file,
            initial,
            name: member.name().into(),
            expected: member.bytes(),
            bytes: 0,
        });
        Ok(())
    }
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.checkpoint.check()?;
        let active = self.active.as_mut().ok_or_else(|| failure(1))?;
        active.bytes = active
            .bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| failure(1))?;
        if active.bytes > active.expected {
            return Err(failure(1));
        }
        io(active.file.file().write_all(bytes))
    }
    fn finish(&mut self, member: &archive::Member, sha256: &str) -> Result<()> {
        let active = self.active.take().ok_or_else(|| failure(1))?;
        let checked = (|| {
            if active.name != member.name()
                || active.bytes != active.expected
                || active.bytes != member.bytes()
            {
                return Err(failure(1));
            }
            io(active.file.file().sync_all())?;
            let stamp = namespace::private_file(&active.file, false)?;
            if !namespace::binding(&active.initial, &stamp) || stamp.len() != active.expected {
                return Err(failure(1));
            }
            let actual = consume(
                active.file.file(),
                active.bytes,
                self.checkpoint,
                |_| Ok(()),
            )?;
            if actual != sha256 || !namespace::same(&stamp, &io(active.file.file().metadata())?) {
                return Err(failure(1));
            }
            let record = Record {
                name: active.name.clone(),
                stamp,
                sha256: actual,
                bytes: active.bytes,
            };
            visible(self.parent, &record, self.budget)?;
            Ok(record)
        })();
        let closed = active.file.finish();
        let record = checked.and_then(|value| closed.map(|()| value))?;
        self.records.insert(record.name.clone(), record);
        Ok(())
    }
}

pub(super) struct Witness {
    parent: ChainWitness,
    pub path: PathBuf,
    stamp: Metadata,
    input_stamp: Option<Metadata>,
    work: Vec<(String, Metadata)>,
    records: BTreeMap<String, Record>,
    payloads: BTreeMap<String, Record>,
}
impl Witness {
    pub fn check(&self, budget: &Budget, checkpoint: &Checkpoint, verified: bool) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let root = Held::open(
            parent.file(),
            self.path.file_name().ok_or_else(|| failure(1))?,
            FLAGS | OFlags::DIRECTORY,
            budget,
        );
        let result = match root {
            Ok(root) => {
                let result = (|| {
                    if !namespace::binding(&self.stamp, &io(root.file().metadata())?) {
                        return Err(failure(1));
                    }
                    let mut expected = self.records.keys().cloned().collect::<BTreeSet<_>>();
                    expected.extend(["input".into(), "work".into()]);
                    if verified {
                        expected.insert("verified".into());
                    }
                    if names(root.file(), budget, 20)? != expected {
                        return Err(failure(1));
                    }
                    for record in self.records.values() {
                        check_record(root.file(), record, budget, checkpoint, false)?;
                    }
                    let work = Held::open(
                        root.file(),
                        OsStr::new("work"),
                        FLAGS | OFlags::DIRECTORY,
                        budget,
                    )?;
                    let checked = (|| {
                        let initial = self.work.first().ok_or_else(|| failure(1))?;
                        if !namespace::binding(&initial.1, &io(work.file().metadata())?) {
                            return Err(failure(1));
                        }
                        for (name, stamp) in &self.work[1..] {
                            let file = Held::open(
                                work.file(),
                                OsStr::new(name),
                                FLAGS | OFlags::DIRECTORY,
                                budget,
                            )?;
                            let checked = io(file.file().metadata()).and_then(|other| {
                                if namespace::binding(stamp, &other) {
                                    Ok(())
                                } else {
                                    Err(failure(1))
                                }
                            });
                            checked.and(file.finish())?;
                        }
                        Ok(())
                    })();
                    checked.and(work.finish())?;
                    let input = Held::open(
                        root.file(),
                        OsStr::new("input"),
                        FLAGS | OFlags::DIRECTORY,
                        budget,
                    )?;
                    let checked = (|| {
                        let observed = io(input.file().metadata())?;
                        if !self
                            .input_stamp
                            .as_ref()
                            .is_some_and(|stamp| namespace::binding(stamp, &observed))
                        {
                            return Err(failure(1));
                        }
                        if names(input.file(), budget, 22)?
                            != self.payloads.keys().cloned().collect()
                        {
                            return Err(failure(1));
                        }
                        for record in self.payloads.values() {
                            check_record(input.file(), record, budget, checkpoint, false)?;
                        }
                        Ok(())
                    })();
                    checked.and(input.finish())
                })();
                result.and(root.finish())
            }
            Err(error) => Err(error),
        };
        result.and(parent.finish())
    }
    pub fn write(
        &mut self,
        name: &str,
        bytes: &[u8],
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<()> {
        self.check(budget, checkpoint, true)?;
        if bytes.is_empty() || bytes.len() as u64 > super::SMALL || self.records.contains_key(name)
        {
            return Err(failure(1));
        }
        let parent = self.parent.reopen(budget)?;
        let root = Held::open(
            parent.file(),
            self.path.file_name().ok_or_else(|| failure(1))?,
            FLAGS | OFlags::DIRECTORY,
            budget,
        );
        let written = match root {
            Ok(root) => {
                let result = (|| {
                    if !namespace::binding(&self.stamp, &io(root.file().metadata())?) {
                        return Err(failure(1));
                    }
                    let stamp = namespace::write(root.file(), name, bytes, budget)?;
                    let record = Record {
                        name: name.into(),
                        stamp,
                        sha256: super::super::sha(bytes),
                        bytes: bytes.len() as u64,
                    };
                    check_record(root.file(), &record, budget, checkpoint, true)?;
                    Ok(record)
                })();
                let closed = root.finish();
                result.and_then(|record| closed.map(|()| record))
            }
            Err(error) => Err(error),
        };
        let closed = parent.finish();
        let record = written.and_then(|record| closed.map(|()| record))?;
        self.records.insert(name.into(), record);
        self.check(budget, checkpoint, true)
    }
}
