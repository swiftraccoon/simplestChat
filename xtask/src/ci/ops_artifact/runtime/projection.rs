//! A fresh closed first-party import tree, sealed before any keeper dispatch.
//!
//! Original checkout witnesses remain separate authority. This private copy and
//! partial copies remain evidence under the output; no recursive deletion or
//! traversal of an unexpected name is part of this owner.

use super::super::namespace::{self, Budget, Chain, ChainWitness, FLAGS, Held, OwnedRun};
use super::{failure, files::Original, io, sources::NAMES, worker::Checkpoint};
use crate::tooling::Result;
use rustix::fs::{Mode, OFlags};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{CStr, OsString},
    fs::{File, Metadata},
    path::{Component, Path, PathBuf},
};

struct Directory {
    witness: ChainWitness,
    stamp: Metadata,
    children: BTreeSet<OsString>,
}

/// Linux's safe borrowed RawDir opens no extra descriptor: the enclosing Chain
/// explicitly closes its file after this cursor and all error paths settle.
pub(super) fn entries(
    file: &File,
    budget: &Budget,
    checkpoint: &Checkpoint,
    mut accept: impl FnMut(&CStr) -> Result<()>,
) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        let _ = budget;
        let mut buffer = [std::mem::MaybeUninit::uninit(); 8192];
        let mut cursor = rustix::fs::RawDir::new(file, &mut buffer);
        while let Some(entry) = cursor.next() {
            checkpoint.check()?;
            let entry = entry.map_err(|_| failure(1))?;
            accept(entry.file_name())?;
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // Actual runtime rejects these targets. Their inert fixtures use the
        // safe portable API, whose internal cursor FD has only a Drop close:
        // rustix 1.1.5 exposes no consuming descriptor or checked close for Dir.
        let _permit = budget.permit()?;
        let cursor = rustix::fs::Dir::read_from(file).map_err(|_| failure(1))?;
        for entry in cursor {
            checkpoint.check()?;
            let entry = entry.map_err(|_| failure(1))?;
            accept(entry.file_name())?;
        }
    }
    checkpoint.check()
}

impl Directory {
    fn inventory(&self, chain: &Chain, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        checkpoint.check()?;
        let mut actual = BTreeSet::new();
        let mut count = 0usize;
        entries(chain.file(), budget, checkpoint, |name| {
            count = count.checked_add(1).ok_or_else(|| failure(1))?;
            if count > self.children.len() + 2 {
                return Err(failure(1));
            }
            if matches!(name.to_bytes(), b"." | b"..") {
                return Ok(());
            }
            use std::os::unix::ffi::OsStrExt;
            let name = std::ffi::OsStr::from_bytes(name.to_bytes());
            if !self.children.contains(name) || !actual.insert(name.to_owned()) {
                return Err(failure(1));
            }
            Ok(())
        })?;
        if actual != self.children {
            return Err(failure(1));
        }
        checkpoint.check()
    }
    fn seal(&mut self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        let directory = self.witness.reopen(budget)?;
        let result = (|| {
            let stamp = io(directory.file().metadata())?;
            if !namespace::binding(&self.stamp, &stamp) {
                return Err(failure(1));
            }
            self.inventory(&directory, budget, checkpoint)?;
            if !namespace::same(&stamp, &io(directory.file().metadata())?) {
                return Err(failure(1));
            }
            self.stamp = stamp;
            Ok(())
        })();
        result.and(directory.finish())
    }
    fn check(&self, budget: &Budget, checkpoint: &Checkpoint) -> Result<()> {
        let directory = self.witness.reopen(budget)?;
        let result = (|| {
            if !namespace::same(&self.stamp, &io(directory.file().metadata())?) {
                return Err(failure(1));
            }
            self.inventory(&directory, budget, checkpoint)?;
            if !namespace::same(&self.stamp, &io(directory.file().metadata())?) {
                return Err(failure(1));
            }
            checkpoint.check()
        })();
        result.and(directory.finish())
    }
}

pub(super) struct Projection {
    path: PathBuf,
    directories: BTreeMap<PathBuf, Directory>,
    files: Vec<Original>,
    budget: Budget,
}

fn layout() -> Result<BTreeMap<PathBuf, BTreeSet<OsString>>> {
    let mut directories = BTreeMap::<PathBuf, BTreeSet<OsString>>::new();
    directories.insert(PathBuf::new(), BTreeSet::new());
    for name in NAMES {
        let path = Path::new(name);
        if !path
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
            || path.components().collect::<PathBuf>().as_os_str() != path.as_os_str()
        {
            return Err(failure(1));
        }
        let mut child = path;
        while let Some(parent) = child.parent() {
            directories
                .entry(parent.into())
                .or_default()
                .insert(child.file_name().ok_or_else(|| failure(1))?.into());
            child = parent;
        }
    }
    Ok(directories)
}

fn observe(path: &Path, stamp: &Metadata, budget: &Budget) -> Result<ChainWitness> {
    let directory = Chain::open(path, budget)?;
    let result = (|| {
        if !namespace::binding(stamp, &io(directory.file().metadata())?) {
            return Err(failure(1));
        }
        directory.witness()
    })();
    let closed = directory.finish();
    result.and_then(|value| closed.map(|()| value))
}

impl Projection {
    pub fn create(
        path: &Path,
        parent: &ChainWitness,
        bodies: &[Vec<u8>],
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        if bodies.len() != NAMES.len()
            || bodies
                .iter()
                .any(|body| body.is_empty() || body.len() > 256 * 1024)
            || bodies.iter().map(Vec::len).sum::<usize>() > 2 * 1024 * 1024
        {
            return Err(failure(1));
        }
        checkpoint.check()?;
        let run = OwnedRun::local_witness(path, budget, parent)?;
        let result = (|| {
            let layout = layout()?;
            let stamp = io(run.file().metadata())?;
            let root = Directory {
                witness: observe(path, &stamp, budget)?,
                stamp,
                children: layout.get(Path::new("")).ok_or_else(|| failure(1))?.clone(),
            };
            let mut directories = BTreeMap::from([(PathBuf::new(), root)]);
            for (name, children) in layout
                .iter()
                .filter(|(name, _)| !name.as_os_str().is_empty())
            {
                checkpoint.check()?;
                let parent = directories
                    .get(name.parent().ok_or_else(|| failure(1))?)
                    .ok_or_else(|| failure(1))?
                    .witness
                    .reopen(budget)?;
                let created = (|| {
                    let leaf = name.file_name().ok_or_else(|| failure(1))?;
                    rustix::fs::mkdirat(parent.file(), leaf, Mode::from_bits_truncate(0o700))
                        .map_err(|_| failure(1))?;
                    let file = Held::open(parent.file(), leaf, FLAGS | OFlags::DIRECTORY, budget)?;
                    let observed = (|| {
                        let stamp = namespace::private_directory(&file)?;
                        Ok(Directory {
                            witness: observe(&path.join(name), &stamp, budget)?,
                            stamp,
                            children: children.clone(),
                        })
                    })();
                    let closed = file.finish();
                    observed.and_then(|value| closed.map(|()| value))
                })();
                let closed = parent.finish();
                let directory = created.and_then(|value| closed.map(|()| value))?;
                directories.insert(name.clone(), directory);
            }
            let mut files = Vec::new();
            for (name, body) in NAMES.iter().zip(bodies) {
                let name = Path::new(name);
                let parent = directories
                    .get(name.parent().ok_or_else(|| failure(1))?)
                    .ok_or_else(|| failure(1))?;
                files.push(Original::create(
                    &parent.witness,
                    name.file_name()
                        .and_then(|s| s.to_str())
                        .ok_or_else(|| failure(1))?,
                    body,
                    budget,
                    checkpoint,
                )?);
            }
            for directory in directories.values_mut() {
                directory.seal(budget, checkpoint)?;
            }
            let value = Self {
                path: path.into(),
                directories,
                files,
                budget: budget.clone(),
            };
            value.check(checkpoint)?;
            Ok(value)
        })();
        let closed = run.finish();
        result.and_then(|value| closed.map(|()| value))
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        for directory in self.directories.values() {
            directory.check(&self.budget, checkpoint)?;
        }
        for file in &self.files {
            file.check(&self.budget, checkpoint)?;
        }
        for directory in self.directories.values() {
            directory.check(&self.budget, checkpoint)?;
        }
        checkpoint.check()
    }
}
