//! Bounded complete-tree framing and private staged copies, not authenticated
//! acquisition or complete compiler I/O closure. No link or special file passes.

use super::{
    Clock, Deadline, failure, hex, io,
    namespace::{self, Budget, Chain, Held, OwnedRun},
};
use crate::{ci::process::Executor, tooling::Result};
use rustix::fs::{Dir, Mode, OFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
};

const ENTRIES: usize = 131072;
const NAMES: usize = 16 * 1024 * 1024;
const FILE_BYTES: u64 = 512 * 1024 * 1024;
const SOURCE_BYTES: u64 = 1024 * 1024 * 1024;
const KIT_BYTES: u64 = 8 * SOURCE_BYTES;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Identity {
    pub profile: &'static str,
    pub sha256: String,
    pub entries: usize,
    pub files: usize,
    pub bytes: u64,
}
struct Entry {
    stamp: Metadata,
    digest: [u8; 32],
    executable: bool,
    directory: bool,
}
pub(super) struct Snapshot {
    pub identity: Identity,
    entries: BTreeMap<String, Entry>,
}
#[derive(Default)]
struct Limits {
    entries: usize,
    names: usize,
}
struct Frame {
    source: Held,
    target: Option<Held>,
    initial: Metadata,
    path: String,
    names: Vec<String>,
    next: usize,
}

fn directory(parent: &File, name: &str, budget: &Budget) -> Result<Held> {
    Held::open(
        parent,
        OsStr::new(name),
        namespace::FLAGS | OFlags::DIRECTORY,
        budget,
    )
}
fn list(file: &File, prefix: &str, budget: &Budget, limits: &mut Limits) -> Result<Vec<String>> {
    let _permit = budget.permit()?;
    let cursor = Dir::read_from(file).map_err(|_| failure(1))?;
    let mut result = Vec::new();
    for entry in cursor {
        let entry = entry.map_err(|_| failure(1))?;
        let bytes = entry.file_name().to_bytes();
        if [b".".as_slice(), b".."].contains(&bytes) {
            continue;
        }
        let name = std::str::from_utf8(bytes).map_err(|_| failure(1))?;
        if name.len() > 255 || name.bytes().any(|b| matches!(b, 0..=31 | 127)) {
            return Err(failure(1));
        }
        limits.entries = limits.entries.checked_add(1).ok_or_else(|| failure(1))?;
        limits.names = limits
            .names
            .checked_add(prefix.len() + name.len() + 1)
            .ok_or_else(|| failure(1))?;
        if limits.entries > ENTRIES || limits.names > NAMES {
            return Err(failure(1));
        }
        result.push(name.to_owned());
    }
    result.sort();
    Ok(result)
}
fn frame(
    source: Held,
    target: Option<Held>,
    path: String,
    budget: &Budget,
    limits: &mut Limits,
) -> Result<Frame> {
    let initial = io(source.file().metadata())?;
    if !initial.is_dir() || initial.mode() & 0o6000 != 0 {
        return Err(failure(1));
    }
    let names = list(source.file(), &path, budget, limits)?;
    Ok(Frame {
        source,
        target,
        initial,
        path,
        names,
        next: 0,
    })
}
fn visible(parent: &File, name: &str, stamp: &Metadata, budget: &Budget) -> Result<()> {
    let file = Held::open(parent, OsStr::new(name), namespace::FLAGS, budget)?;
    let matched = namespace::same(stamp, &io(file.file().metadata())?);
    file.finish()?;
    if matched { Ok(()) } else { Err(failure(1)) }
}
async fn file<E: Executor, C: Clock>(
    source: &Held,
    target: Option<&Held>,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<([u8; 32], Metadata, Option<Metadata>)> {
    let before = io(source.file().metadata())?;
    if !before.is_file()
        || before.nlink() != 1
        || before.mode() & 0o6000 != 0
        || before.len() > FILE_BYTES
    {
        return Err(failure(1));
    }
    let mut digest = Sha256::new();
    let mut count = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let mut reader = source.file();
    let mut writer = target.map(Held::file);
    let normalized = target
        .map(|value| namespace::private_file(value, before.mode() & 0o111 != 0))
        .transpose()?;
    loop {
        deadline.check(false)?;
        executor.ready().await?;
        let n = io(reader.read(&mut buffer))?;
        if n == 0 {
            break;
        }
        count = count.checked_add(n as u64).ok_or_else(|| failure(1))?;
        if count > before.len() {
            return Err(failure(1));
        }
        digest.update(&buffer[..n]);
        if let Some(output) = &mut writer {
            io(output.write_all(&buffer[..n]))?;
        }
    }
    if count != before.len() || !namespace::same(&before, &io(source.file().metadata())?) {
        return Err(failure(1));
    }
    let digest: [u8; 32] = digest.finalize().into();
    let mut destination = None;
    if let Some(target) = target {
        io(target.file().sync_all())?;
        let written = namespace::private_file(target, before.mode() & 0o111 != 0)?;
        if !namespace::binding(normalized.as_ref().unwrap(), &written) {
            return Err(failure(1));
        }
        let mut output = target.file();
        io(output.seek(SeekFrom::Start(0)))?;
        let mut actual = Sha256::new();
        let mut count = 0u64;
        loop {
            deadline.check(false)?;
            executor.ready().await?;
            let n = io(output.read(&mut buffer))?;
            if n == 0 {
                break;
            }
            count += n as u64;
            if count > before.len() {
                return Err(failure(1));
            }
            actual.update(&buffer[..n]);
        }
        if count != before.len()
            || actual.finalize().as_slice() != digest
            || !namespace::same(&written, &io(target.file().metadata())?)
        {
            return Err(failure(1));
        }
        destination = Some(written);
    }
    Ok((digest, before, destination))
}

struct Selected<'a> {
    root: &'a File,
    target: Option<Held>,
    maximum: u64,
    #[cfg(test)]
    after_copy: Option<AfterCopy<'a>>,
}
#[cfg(test)]
type AfterCopy<'a> = &'a mut dyn FnMut(&str, &File);
async fn scan<E: Executor, C: Clock>(
    root: &File,
    target: Option<Held>,
    maximum: u64,
    budget: &Budget,
    limits: &mut Limits,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<(Snapshot, Option<Snapshot>)> {
    scan_selected(
        Selected {
            root,
            target,
            maximum,
            #[cfg(test)]
            after_copy: None,
        },
        budget,
        limits,
        executor,
        deadline,
    )
    .await
}
async fn scan_selected<E: Executor, C: Clock>(
    mut selected: Selected<'_>,
    budget: &Budget,
    limits: &mut Limits,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<(Snapshot, Option<Snapshot>)> {
    // The mutable test hook is absent from production; it models a completed
    // writer mutation at the precise pre-admission baseline boundary.
    #[cfg(not(test))]
    let _ = &mut selected;
    let root = selected.root;
    let target = selected.target.take();
    let maximum = selected.maximum;
    limits.entries = limits.entries.checked_add(1).ok_or_else(|| failure(1))?;
    if limits.entries > ENTRIES {
        return Err(failure(1));
    }
    let root = directory(root, ".", budget)?;
    let mut frames = vec![frame(root, target, String::new(), budget, limits)?];
    let mut original = BTreeMap::new();
    let mut staged = BTreeMap::new();
    let mut bytes = 0u64;
    let mut files = 0usize;
    while !frames.is_empty() {
        deadline.check(false)?;
        executor.ready().await?;
        let device = frames[0].initial.dev();
        let current = frames.last_mut().unwrap();
        if current.next == current.names.len() {
            let current = frames.pop().unwrap();
            if !namespace::same(&current.initial, &io(current.source.file().metadata())?) {
                return Err(failure(1));
            }
            original.insert(
                current.path.clone(),
                Entry {
                    stamp: current.initial,
                    digest: [0; 32],
                    executable: false,
                    directory: true,
                },
            );
            if let Some(target) = current.target {
                let stamp = namespace::private_directory(&target)?;
                staged.insert(
                    current.path,
                    Entry {
                        stamp,
                        digest: [0; 32],
                        executable: false,
                        directory: true,
                    },
                );
                target.finish()?;
            }
            current.source.finish()?;
            continue;
        }
        let name = current.names[current.next].clone();
        current.next += 1;
        let relative = if current.path.is_empty() {
            name.clone()
        } else {
            format!("{}/{name}", current.path)
        };
        if relative.len() > 4096 || relative.split('/').count() > 32 {
            return Err(failure(1));
        }
        let source = Held::open(
            current.source.file(),
            OsStr::new(&name),
            namespace::FLAGS,
            budget,
        )?;
        let before = io(source.file().metadata())?;
        if before.dev() != device {
            return Err(failure(1));
        }
        if before.is_dir() {
            let target = if let Some(target) = &current.target {
                rustix::fs::mkdirat(
                    target.file(),
                    name.as_str(),
                    Mode::from_bits_truncate(0o700),
                )
                .map_err(|_| failure(1))?;
                let child = directory(target.file(), &name, budget)?;
                namespace::private_directory(&child)?;
                Some(child)
            } else {
                None
            };
            visible(current.source.file(), &name, &before, budget)?;
            frames.push(frame(source, target, relative, budget, limits)?);
            continue;
        }
        if !before.is_file() {
            return Err(failure(1));
        }
        bytes = bytes.checked_add(before.len()).ok_or_else(|| failure(1))?;
        if bytes > maximum {
            return Err(failure(1));
        }
        files += 1;
        let executable = before.mode() & 0o111 != 0;
        let target = current
            .target
            .as_ref()
            .map(|dir| Held::create(dir.file(), &name, executable, budget))
            .transpose()?;
        let (digest, stamp, written) = file(&source, target.as_ref(), executor, deadline).await?;
        visible(current.source.file(), &name, &stamp, budget)?;
        original.insert(
            relative.clone(),
            Entry {
                stamp,
                digest,
                executable,
                directory: false,
            },
        );
        if let Some(target) = target {
            #[cfg(test)]
            if let Some(after) = selected.after_copy.as_mut() {
                after(&relative, target.file());
            }
            let stamp = written.ok_or_else(|| failure(1))?;
            if !namespace::same(&stamp, &io(target.file().metadata())?) {
                return Err(failure(1));
            }
            visible(
                current.target.as_ref().unwrap().file(),
                &name,
                &stamp,
                budget,
            )?;
            staged.insert(
                relative,
                Entry {
                    stamp,
                    digest,
                    executable,
                    directory: false,
                },
            );
            target.finish()?;
        }
        source.finish()?;
    }
    let original = snapshot(original, files, bytes);
    let staged = if staged.is_empty() {
        None
    } else {
        Some(snapshot(staged, files, bytes))
    };
    Ok((original, staged))
}
fn snapshot(entries: BTreeMap<String, Entry>, files: usize, bytes: u64) -> Snapshot {
    let mut frame = Sha256::new();
    frame.update(b"ops-producer-tree-v1\0");
    for (name, entry) in &entries {
        frame.update((name.len() as u32).to_be_bytes());
        frame.update(name.as_bytes());
        frame.update([u8::from(entry.directory), u8::from(entry.executable)]);
        frame.update(
            if entry.directory {
                0
            } else {
                entry.stamp.len()
            }
            .to_be_bytes(),
        );
        frame.update(entry.digest);
    }
    Snapshot {
        identity: Identity {
            profile: "ops-producer-tree-v1",
            sha256: hex(&frame.finalize()),
            entries: entries.len(),
            files,
            bytes,
        },
        entries,
    }
}
fn unchanged(original: &Snapshot, observed: &Snapshot) -> Result<()> {
    if original.identity.sha256 != observed.identity.sha256
        || original.entries.len() != observed.entries.len()
        || original.entries.iter().any(|(name, entry)| {
            observed
                .entries
                .get(name)
                .is_none_or(|other| !namespace::same(&entry.stamp, &other.stamp))
        })
    {
        Err(failure(1))
    } else {
        Ok(())
    }
}

#[cfg(test)]
pub(super) async fn measure<E: Executor, C: Clock>(
    path: &std::path::Path,
    budget: &Budget,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<Identity> {
    let root = Chain::open(path, budget)?;
    let measured = scan(
        root.file(),
        None,
        KIT_BYTES,
        budget,
        &mut Limits::default(),
        executor,
        deadline,
    )
    .await;
    let closed = root.finish();
    let identity = measured?.0.identity;
    closed?;
    Ok(identity)
}

#[cfg(test)]
pub(super) async fn copy_after<E: Executor, C: Clock>(
    path: &std::path::Path,
    run: &OwnedRun,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
    after: &mut dyn FnMut(&str, &File),
) -> Result<()> {
    let root = Chain::open(path, run.budget())?;
    let selected = Selected {
        root: root.file(),
        target: Some(run.directory("source")?),
        maximum: SOURCE_BYTES,
        after_copy: Some(after),
    };
    let result = scan_selected(
        selected,
        run.budget(),
        &mut Limits::default(),
        executor,
        deadline,
    )
    .await;
    let closed = root.finish();
    result?;
    closed
}

pub(super) struct Materials {
    source: Chain,
    kit: Chain,
    pub source_identity: Identity,
    pub kit_identity: Identity,
    source_snapshot: Snapshot,
    kit_snapshot: Snapshot,
    staged_source: Snapshot,
    staged_kit: Snapshot,
}
impl Materials {
    pub fn tool_digest(&self, relative: &str) -> Result<serde_json::Value> {
        let entry = self
            .staged_kit
            .entries
            .get(relative)
            .ok_or_else(|| failure(1))?;
        if entry.directory || !entry.executable {
            return Err(failure(1));
        }
        Ok(serde_json::json!({"sha256":hex(&entry.digest),"bytes":entry.stamp.len()}))
    }
    pub fn kit_stamps(&self, relative: &str) -> Result<Vec<Metadata>> {
        let mut path = String::new();
        let mut result = Vec::new();
        for part in relative.split('/') {
            if !path.is_empty() {
                path.push('/');
            }
            path.push_str(part);
            result.push(
                self.staged_kit
                    .entries
                    .get(&path)
                    .ok_or_else(|| failure(1))?
                    .stamp
                    .clone(),
            );
        }
        Ok(result)
    }
    pub fn kit_root(&self) -> &Metadata {
        &self.staged_kit.entries[""].stamp
    }
    pub fn manifest(&self, path: &std::path::Path, run: &OwnedRun) -> bool {
        for (prefix, snapshot) in [
            ("source", &self.staged_source),
            ("kit/registry", &self.staged_kit),
        ] {
            if let Ok(relative) = path.strip_prefix(run.path.join(prefix)) {
                let key = if prefix == "source" {
                    relative.to_str().map(str::to_owned)
                } else {
                    relative.to_str().map(|path| format!("registry/{path}"))
                };
                return key
                    .and_then(|key| snapshot.entries.get(&key))
                    .is_some_and(|entry| !entry.directory);
            }
        }
        false
    }
    pub async fn stage<E: Executor, C: Clock>(
        request: &super::request::Request,
        run: &OwnedRun,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Self> {
        deadline.check(false)?;
        executor.ready().await?;
        run.check()?;
        let source = Chain::open(&request.source.path, run.budget())?;
        let kit = Chain::open(&request.kit.path, run.budget())?;
        let mut limits = Limits::default();
        let (source_snapshot, staged_source) = scan(
            source.file(),
            Some(run.directory("source")?),
            SOURCE_BYTES,
            run.budget(),
            &mut limits,
            executor,
            deadline,
        )
        .await?;
        let (kit_snapshot, staged_kit) = scan(
            kit.file(),
            Some(run.directory("kit")?),
            KIT_BYTES,
            run.budget(),
            &mut limits,
            executor,
            deadline,
        )
        .await?;
        if source_snapshot.identity.sha256 != request.source.sha256
            || kit_snapshot.identity.sha256 != request.kit.sha256
        {
            return Err(failure(1));
        }
        for name in [
            "toolchain",
            "host-tools",
            "registry",
            "toolchain/lib/rustlib/x86_64-unknown-linux-musl/lib",
        ] {
            if !kit_snapshot
                .entries
                .get(name)
                .is_some_and(|entry| entry.directory)
            {
                return Err(failure(1));
            }
        }
        if kit_snapshot.entries.keys().any(|path| {
            !path.is_empty()
                && !["toolchain", "host-tools", "registry"]
                    .contains(&path.split('/').next().unwrap())
        }) {
            return Err(failure(1));
        }
        let staged_source = staged_source.unwrap();
        let staged_kit = staged_kit.unwrap();
        if source_snapshot.identity.sha256 != staged_source.identity.sha256
            || kit_snapshot.identity.sha256 != staged_kit.identity.sha256
        {
            return Err(failure(1));
        }
        source.verify()?;
        kit.verify()?;
        run.check()?;
        Ok(Self {
            source_identity: source_snapshot.identity.clone(),
            kit_identity: kit_snapshot.identity.clone(),
            source,
            kit,
            source_snapshot,
            kit_snapshot,
            staged_source,
            staged_kit,
        })
    }
    pub async fn verify<E: Executor, C: Clock>(
        &self,
        run: &OwnedRun,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<()> {
        self.source.verify()?;
        self.kit.verify()?;
        run.check()?;
        let mut limits = Limits::default();
        for (root, maximum, expected) in [
            (self.source.file(), SOURCE_BYTES, &self.source_snapshot),
            (self.kit.file(), KIT_BYTES, &self.kit_snapshot),
        ] {
            let (observed, _) = scan(
                root,
                None,
                maximum,
                run.budget(),
                &mut limits,
                executor,
                deadline,
            )
            .await?;
            unchanged(expected, &observed)?;
        }
        let mut limits = Limits::default();
        for (name, maximum, expected) in [
            ("source", SOURCE_BYTES, &self.staged_source),
            ("kit", KIT_BYTES, &self.staged_kit),
        ] {
            let root = directory(run.file(), name, run.budget())?;
            let observed = scan(
                root.file(),
                None,
                maximum,
                run.budget(),
                &mut limits,
                executor,
                deadline,
            )
            .await;
            let closed = root.finish();
            unchanged(expected, &observed?.0)?;
            closed?;
        }
        self.source.verify()?;
        self.kit.verify()?;
        run.check()
    }
    pub fn finish(self) -> Result<()> {
        let source = self.source.finish();
        let kit = self.kit.finish();
        source?;
        kit
    }
}
