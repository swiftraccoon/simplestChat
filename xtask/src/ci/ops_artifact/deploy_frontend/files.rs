//! Private exclusive evidence and immutable byte/stamp witnesses.
//!
//! Observations detect changes; they do not lock out a concurrent same-UID writer.
//! Command streams are read only after the shared native owner closes both pipes.

use super::super::namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun, Permit};
use super::{failure, io, worker::Checkpoint};
use crate::{
    ci::{bounded_log::Log, command::SeparateLogs},
    tooling::Result,
};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

const SMALL: u64 = 4 * 1024 * 1024;
pub(super) const STDERR: u64 = 64 * 1024;
fn digest(bytes: &[u8]) -> String {
    super::super::sha(bytes)
}
fn consume(
    file: &File,
    maximum: u64,
    collect: bool,
    cp: &Checkpoint,
) -> Result<(Metadata, String, Vec<u8>)> {
    cp.check()?;
    let before = io(file.metadata())?;
    if !before.is_file() || before.nlink() != 1 || before.len() > maximum {
        return Err(failure(1));
    }
    let mut source = file;
    io(source.seek(SeekFrom::Start(0)))?;
    let mut hash = Sha256::new();
    let mut bytes = Vec::new();
    let mut left = before.len();
    let mut block = [0; 65536];
    while left != 0 {
        cp.check()?;
        let limit = left.min(block.len() as u64) as usize;
        let count = match source.read(&mut block[..limit]) {
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            value => io(value)?,
        };
        if count == 0 {
            return Err(failure(1));
        }
        hash.update(&block[..count]);
        if collect {
            bytes.extend_from_slice(&block[..count]);
        }
        left -= count as u64;
    }
    if io(source.read(&mut block[..1]))? != 0 || !namespace::same(&before, &io(file.metadata())?) {
        return Err(failure(1));
    }
    cp.check()?;
    Ok((before, super::super::hex(&hash.finalize()), bytes))
}
#[derive(Clone)]
pub(super) struct Original {
    path: PathBuf,
    parent: ChainWitness,
    stamp: Metadata,
    hash: String,
}
impl Original {
    pub fn open(path: &Path, budget: &Budget, cp: &Checkpoint) -> Result<(Self, Vec<u8>)> {
        if !namespace::path(path) {
            return Err(failure(1));
        }
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = (|| {
            let name = path.file_name().ok_or_else(|| failure(1))?;
            let file = Held::open(parent.file(), name, FLAGS, budget)?;
            let result = consume(file.file(), SMALL, true, cp);
            let closed = file.finish();
            let (stamp, hash, bytes) = result?;
            closed?;
            if stamp.mode() & 0o022 != 0 {
                return Err(failure(1));
            }
            Ok((
                Self {
                    path: path.into(),
                    parent: parent.witness()?,
                    stamp,
                    hash,
                },
                bytes,
            ))
        })();
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn check(&self, budget: &Budget, cp: &Checkpoint) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let result = (|| {
            let file = Held::open(parent.file(), self.path.file_name().unwrap(), FLAGS, budget)?;
            let result = consume(file.file(), SMALL, false, cp);
            let closed = file.finish();
            let (stamp, hash, _) = result?;
            closed?;
            if !namespace::same(&self.stamp, &stamp) || hash != self.hash {
                return Err(failure(1));
            }
            Ok(())
        })();
        let closed = parent.finish();
        result.and(closed)
    }
}
#[derive(Clone)]
struct Record {
    stamp: Metadata,
    hash: String,
}
pub(super) struct Pending {
    pub logs: Option<Arc<SeparateLogs>>,
    names: [String; 2],
    initial: [Metadata; 2],
    _permits: Vec<Permit>,
}
pub(super) struct Evidence {
    run: OwnedRun,
    records: BTreeMap<String, Record>,
}
impl Evidence {
    pub fn new(root: &Path, budget: &Budget) -> Result<Self> {
        let root_chain = Chain::open(root, budget)?;
        let result = (|| {
            match rustix::fs::mkdirat(
                root_chain.file(),
                "results",
                rustix::fs::Mode::from_bits_truncate(0o700),
            ) {
                Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                Err(_) => return Err(failure(1)),
            }
            let results = Held::open(
                root_chain.file(),
                OsStr::new("results"),
                FLAGS | rustix::fs::OFlags::DIRECTORY,
                budget,
            )?;
            let checked = io(results.file().metadata()).and_then(|metadata| {
                if metadata.is_dir()
                    && metadata.uid() == rustix::process::getuid().as_raw()
                    && metadata.mode() & 0o022 == 0
                {
                    Ok(())
                } else {
                    Err(failure(1))
                }
            });
            let closed = results.finish();
            checked?;
            closed?;
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| failure(1))?
                .as_nanos();
            let path = root
                .join("results")
                .join(format!("deploy-rust.{}.{nonce}", std::process::id()));
            Ok(Self {
                run: OwnedRun::local(&path, budget)?,
                records: BTreeMap::new(),
            })
        })();
        let closed = root_chain.finish();
        match (result, closed) {
            (Ok(value), Ok(())) => Ok(value),
            (Ok(value), Err(error)) => {
                let _ = value.finish();
                Err(error)
            }
            (Err(error), _) => Err(error),
        }
    }
    pub fn path(&self) -> &Path {
        &self.run.path
    }
    pub fn write(&mut self, name: &str, bytes: &[u8], cp: &Checkpoint) -> Result<()> {
        cp.check()?;
        self.run.check()?;
        if bytes.len() as u64 > SMALL
            || self.records.contains_key(name)
            || name.is_empty()
            || name.contains('/')
            || matches!(name, "." | "..")
        {
            return Err(failure(1));
        }
        let stamp = namespace::write(self.run.file(), name, bytes, self.run.budget())?;
        self.records.insert(
            name.into(),
            Record {
                stamp,
                hash: digest(bytes),
            },
        );
        cp.check()
    }
    pub fn check(&self, cp: &Checkpoint) -> Result<()> {
        cp.check()?;
        self.run.check()?;
        for (name, record) in &self.records {
            self.read_record(name, record, false, cp)?;
        }
        cp.check()
    }
    fn read_record(
        &self,
        name: &str,
        record: &Record,
        collect: bool,
        cp: &Checkpoint,
    ) -> Result<Vec<u8>> {
        let file = Held::open(self.run.file(), OsStr::new(name), FLAGS, self.run.budget())?;
        let result = consume(file.file(), record.stamp.len(), collect, cp);
        let closed = file.finish();
        let (stamp, hash, bytes) = result?;
        closed?;
        if !namespace::same(&record.stamp, &stamp) || hash != record.hash {
            return Err(failure(1));
        }
        Ok(bytes)
    }
    pub async fn capture(&self, index: usize, maximum: u64) -> Result<Pending> {
        self.run.check()?;
        let names = [
            format!("command-{index:04}.stdout"),
            format!("command-{index:04}.stderr"),
        ];
        let permits = self.run.budget().reserve(8)?;
        let mut logs = Vec::new();
        let mut initial = Vec::new();
        let admitted = (|| {
            for (name, maximum) in names.iter().zip([maximum, STDERR]) {
                let file = rustix::fs::openat(
                    self.run.file(),
                    name.as_str(),
                    rustix::fs::OFlags::RDWR
                        | rustix::fs::OFlags::CREATE
                        | rustix::fs::OFlags::EXCL
                        | rustix::fs::OFlags::NOFOLLOW
                        | rustix::fs::OFlags::CLOEXEC,
                    rustix::fs::Mode::from_bits_truncate(0o600),
                )
                .map(File::from)
                .map_err(|_| failure(1))?;
                let stamp = io(file.metadata());
                let stamp = match stamp {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = nix::unistd::close(file);
                        return Err(error);
                    }
                };
                initial.push(stamp);
                logs.push(Log::start(file, maximum)?);
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
    pub fn captured(
        &mut self,
        pending: Pending,
        captures: &super::tools::Captures,
        collect: bool,
        cp: &Checkpoint,
    ) -> Result<Vec<u8>> {
        let mut stdout = Vec::new();
        for (index, seal) in [&captures.stdout, &captures.stderr].iter().enumerate() {
            if !namespace::binding(&pending.initial[index], &seal.metadata)
                || seal.bytes != seal.metadata.len()
                || seal.metadata.mode() & 0o777 != 0o600
                || seal.metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure(1));
            }
            let record = Record {
                stamp: seal.metadata.clone(),
                hash: seal.sha256.clone(),
            };
            let bytes =
                self.read_record(&pending.names[index], &record, collect && index == 0, cp)?;
            if index == 0 {
                stdout = bytes;
            }
            self.records.insert(pending.names[index].clone(), record);
        }
        self.run.check()?;
        Ok(stdout)
    }
    pub fn finish(self) -> Result<()> {
        self.run.finish()
    }
}

pub(super) fn source_paths(bytes: &[u8]) -> Result<Vec<PathBuf>> {
    let paths = bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| {
            let text = std::str::from_utf8(part).map_err(|_| failure(1))?;
            if !(text.starts_with("ops/ansible/") || text.starts_with("build/"))
                || text.contains("..")
                || text.chars().any(char::is_control)
            {
                return Err(failure(1));
            }
            Ok(PathBuf::from(text))
        })
        .collect::<Result<Vec<_>>>()?;
    if paths.is_empty() || paths.len() > 1024 {
        return Err(failure(1));
    }
    Ok(paths
        .into_iter()
        .filter(|p| {
            let text = p.to_string_lossy();
            !text.starts_with("ops/ansible/tests/")
                && !text.starts_with("build/tests/")
                && !text.ends_with(".pyc")
        })
        .collect())
}

/// An absent ref component is witnessed at its nearest existing parent. Its
/// appearance invalidates the snapshot, including a new loose ref over packed refs.
pub(super) struct Missing {
    parent: ChainWitness,
    name: std::ffi::OsString,
}
impl Missing {
    fn open(path: &Path, budget: &Budget) -> Result<Self> {
        let mut parent_path = path.parent().ok_or_else(|| failure(1))?;
        let mut name = path.file_name().ok_or_else(|| failure(1))?.to_os_string();
        loop {
            match std::fs::symlink_metadata(parent_path) {
                Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    name = parent_path
                        .file_name()
                        .ok_or_else(|| failure(1))?
                        .to_os_string();
                    parent_path = parent_path.parent().ok_or_else(|| failure(1))?;
                }
                _ => return Err(failure(1)),
            }
        }
        let parent = Chain::open(parent_path, budget)?;
        let result = (|| {
            match rustix::fs::statat(parent.file(), &name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                Err(rustix::io::Errno::NOENT) => {}
                _ => return Err(failure(1)),
            }
            Ok(Self {
                parent: parent.witness()?,
                name,
            })
        })();
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    fn check(&self, budget: &Budget) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let result = match rustix::fs::statat(
            parent.file(),
            &self.name,
            rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => Ok(()),
            _ => Err(failure(1)),
        };
        let closed = parent.finish();
        result.and(closed)
    }
}
pub(super) enum GitInput {
    File(Original),
    Absent(Missing),
}
impl GitInput {
    pub fn check(&self, budget: &Budget, cp: &Checkpoint) -> Result<()> {
        cp.check()?;
        match self {
            Self::File(file) => file.check(budget, cp),
            Self::Absent(missing) => missing.check(budget),
        }
    }
}
pub(super) fn git_identity(
    root: &Path,
    revision: &str,
    budget: &Budget,
    cp: &Checkpoint,
) -> Result<Vec<GitInput>> {
    let mut result = Vec::new();
    for name in ["HEAD", "index", "config"] {
        let (file, bytes) = Original::open(&root.join(".git").join(name), budget, cp)?;
        if name == "HEAD"
            && std::str::from_utf8(&bytes)
                .map_err(|_| failure(1))?
                .trim_end_matches(['\r', '\n'])
                != "ref: refs/heads/main"
        {
            return Err(failure(1));
        }
        result.push(GitInput::File(file));
    }
    let mut loose = false;
    let mut packed = false;
    for name in ["refs/heads/main", "packed-refs"] {
        let path = root.join(".git").join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(_) => {
                let (file, bytes) = Original::open(&path, budget, cp)?;
                if name == "refs/heads/main" {
                    if std::str::from_utf8(&bytes)
                        .map_err(|_| failure(1))?
                        .trim_end_matches(['\r', '\n'])
                        != revision
                    {
                        return Err(failure(1));
                    }
                    loose = true;
                } else {
                    let mut entries = bytes.split(|b| *b == b'\n').filter_map(|line| {
                        line.strip_suffix(b"\r")
                            .unwrap_or(line)
                            .strip_suffix(b" refs/heads/main")
                    });
                    if let Some(value) = entries.next() {
                        packed = value == revision.as_bytes() && entries.next().is_none();
                    }
                }
                result.push(GitInput::File(file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                result.push(GitInput::Absent(Missing::open(&path, budget)?));
            }
            Err(_) => return Err(failure(1)),
        }
    }
    if !loose && !packed {
        return Err(failure(1));
    }
    Ok(result)
}

/// Candidate identity is separate from payload authentication in the callee.
/// Retain an ancestry witness and observed inode/stamp, without parsing a ZIP.
pub(super) struct Candidate {
    parent: ChainWitness,
    path: PathBuf,
    stamp: Metadata,
    directory: bool,
}
impl Candidate {
    pub fn open(path: &Path, directory: bool, budget: &Budget) -> Result<Self> {
        let parent = Chain::open(path.parent().ok_or_else(|| failure(1))?, budget)?;
        let result = (|| {
            let flags = if directory {
                FLAGS | rustix::fs::OFlags::DIRECTORY
            } else {
                FLAGS
            };
            let file = Held::open(
                parent.file(),
                path.file_name().ok_or_else(|| failure(1))?,
                flags,
                budget,
            )?;
            let stamp = if directory {
                namespace::private_directory(&file)
            } else {
                namespace::private_file(&file, false)
            };
            let closed = file.finish();
            let stamp = stamp?;
            closed?;
            Ok(Self {
                parent: parent.witness()?,
                path: path.into(),
                stamp,
                directory,
            })
        })();
        let closed = parent.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn check(&self, budget: &Budget) -> Result<()> {
        let parent = self.parent.reopen(budget)?;
        let result = (|| {
            let flags = if self.directory {
                FLAGS | rustix::fs::OFlags::DIRECTORY
            } else {
                FLAGS
            };
            let file = Held::open(parent.file(), self.path.file_name().unwrap(), flags, budget)?;
            let stamp = io(file.file().metadata());
            let closed = file.finish();
            let stamp = stamp?;
            closed?;
            if !namespace::same(&self.stamp, &stamp) {
                return Err(failure(1));
            }
            Ok(())
        })();
        let closed = parent.finish();
        result.and(closed)
    }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
