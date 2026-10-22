//! Offline lock/archive/output relationships. No registry origin, dependency
//! resolution, complete build graph, ready toolchain or signing claim is made.

mod archive;
mod lock;
mod output;
mod request;
#[cfg(test)]
mod tests;
use super::{
    Clock, Deadline, NativeClock, digest, failure, hex, io, materials,
    namespace::{self, Budget, Chain, Held, OwnedRun},
    sha,
};
use crate::{ci::process::Executor, tooling::Result};
use request::Request;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, ffi::OsStr, io::Read, os::unix::fs::MetadataExt, path::Path};

const COMPRESSED: u64 = 128 * 1024 * 1024;
const EXPANDED: u64 = 1024 * 1024 * 1024;
const MEMBERS: usize = 65536;

struct Cache {
    root: Chain,
    budget: Budget,
}
impl Cache {
    fn open(path: &Path, budget: &Budget) -> Result<Self> {
        Ok(Self {
            root: Chain::open(path, budget)?,
            budget: budget.clone(),
        })
    }
    async fn archive<E: Executor, C: Clock>(
        &self,
        selected: &lock::Crate,
        executor: &mut E,
        deadline: &Deadline<'_, C>,
    ) -> Result<Vec<u8>> {
        self.root.verify()?;
        let name = format!("{}.crate", selected.directory);
        let file = Held::open(
            self.root.file(),
            OsStr::new(&name),
            namespace::FLAGS,
            &self.budget,
        )?;
        let result = async {
            let before = io(file.file().metadata())?;
            if !before.is_file()
                || before.nlink() != 1
                || before.len() == 0
                || before.len() > archive::COMPRESSED
            {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            bytes
                .try_reserve_exact(before.len() as usize)
                .map_err(|_| failure(1))?;
            let mut reader = file.file();
            let mut buffer = [0u8; 65536];
            loop {
                deadline.check(false)?;
                executor.ready().await?;
                let n = io(reader.read(&mut buffer))?;
                if n == 0 {
                    break;
                }
                if bytes.len() + n > before.len() as usize {
                    return Err(failure(1));
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            if bytes.len() as u64 != before.len()
                || sha(&bytes) != selected.checksum
                || !namespace::same(&before, &io(file.file().metadata())?)
            {
                return Err(failure(1));
            }
            let visible = Held::open(
                self.root.file(),
                OsStr::new(&name),
                namespace::FLAGS,
                &self.budget,
            )?;
            let matched = namespace::same(&before, &io(visible.file().metadata())?);
            visible.finish()?;
            self.root.verify()?;
            if matched { Ok(bytes) } else { Err(failure(1)) }
        }
        .await;
        let closed = file.finish();
        result.and_then(|bytes| closed.map(|()| bytes))
    }
    fn finish(self) -> Result<()> {
        self.root.finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Observation<'a> {
    schema_version: u8,
    kind: &'static str,
    request_sha256: &'a str,
    request_bytes: usize,
    lock_sha256: &'a str,
    lock_bytes: usize,
    registry_packages: usize,
    archives_sha256: String,
    compressed_bytes: u64,
    expanded_tar_bytes: u64,
    registry: materials::Identity,
    claims: &'static str,
}
#[derive(Default)]
struct Totals {
    compressed: u64,
    expanded: u64,
    members: usize,
    names: usize,
}
impl Totals {
    fn add(&mut self, bytes: u64, archive: &archive::Archive) -> Result<()> {
        self.compressed = self
            .compressed
            .checked_add(bytes)
            .ok_or_else(|| failure(1))?;
        self.expanded = self
            .expanded
            .checked_add(archive.bytes.len() as u64)
            .ok_or_else(|| failure(1))?;
        self.members = self
            .members
            .checked_add(archive.members.len() + 1)
            .ok_or_else(|| failure(1))?;
        self.names = self
            .names
            .checked_add(archive.members.keys().map(String::len).sum::<usize>())
            .ok_or_else(|| failure(1))?;
        if self.compressed > COMPRESSED
            || self.expanded > EXPANDED
            || self.members >= MEMBERS
            || self.names > 16 * 1024 * 1024
        {
            return Err(failure(1));
        }
        Ok(())
    }
}
async fn hydrate<E: Executor, C: Clock>(
    request: &Request,
    cache: &Cache,
    run: &OwnedRun,
    executor: &mut E,
    deadline: &Deadline<'_, C>,
) -> Result<Vec<u8>> {
    executor.ready().await?;
    deadline.check(false)?;
    let lock = namespace::read(&request.lock.path, lock::MAXIMUM, run.budget())?;
    if sha(&lock) != request.lock.sha256 {
        return Err(failure(1));
    }
    let packages = lock::parse(&lock)?;
    let mut output = output::Output::new(run)?;
    let result = async {
        let mut totals = Totals::default();
        let mut archives_frame = Sha256::new();
        archives_frame.update(b"ops-registry-archives-v1\0");
        for selected in &packages {
            executor.ready().await?;
            deadline.check(false)?;
            run.check()?;
            // Immutable in-memory original bytes have passed full SHA, stamp,
            // namespace and explicit-close checks before either parser sees them.
            let compressed = cache.archive(selected, executor, deadline).await?;
            if totals.compressed + compressed.len() as u64 > COMPRESSED {
                return Err(failure(1));
            }
            let bytes = archive::inflate(&compressed, executor, deadline).await?;
            let archive = archive::Archive::parse(bytes, selected)?;
            totals.add(compressed.len() as u64, &archive)?;
            archives_frame.update((selected.directory.len() as u32).to_be_bytes());
            archives_frame.update(selected.directory.as_bytes());
            archives_frame.update(Sha256::digest(&compressed));
            archives_frame.update((compressed.len() as u64).to_be_bytes());
            let mut checksums = BTreeMap::new();
            for (name, member) in &archive.members {
                executor.ready().await?;
                deadline.check(false)?;
                if member.directory {
                    output.directory(name)?;
                } else {
                    let data = &archive.bytes[member.offset..member.offset + member.size];
                    output.write(name, data, member.executable)?;
                    let relative = &name[selected.directory.len() + 1..];
                    checksums.insert(relative, sha(data));
                }
            }
            // Cargo directory-source semantics: package is the original .crate
            // SHA; files map actual regular payload bytes, excluding this file.
            let checksum = serde_json::to_vec(&serde_json::json!({
                "package":selected.checksum,"files":checksums
            }))
            .map_err(|_| failure(1))?;
            output.write(
                &format!("{}/.cargo-checksum.json", selected.directory),
                &checksum,
                false,
            )?;
        }
        cache.root.verify()?;
        let identity = output.measure(run, executor, deadline).await?;
        serde_json::to_vec(&Observation {
            schema_version: 1,
            kind: "ops-offline-registry-observation",
            request_sha256: &request.sha256,
            request_bytes: request.bytes,
            lock_sha256: &request.lock.sha256,
            lock_bytes: lock.len(),
            registry_packages: packages.len(),
            archives_sha256: hex(&archives_frame.finalize()),
            compressed_bytes: totals.compressed,
            expanded_tar_bytes: totals.expanded,
            registry: identity,
            claims: concat!(
                "observedLockArchiveOutputRelationship",
                "NotAuthenticatedOriginOrResolvedBuildGraph"
            ),
        })
        .map_err(|_| failure(1))
    }
    .await;
    let closed = output.finish(run);
    result.and_then(|bytes| closed.map(|()| bytes))
}

pub(super) async fn native(path: &Path, selected: &str) -> Result<()> {
    use crate::{ci::process::Native, owned_process::Signals};
    let mut signals = Signals::new()?;
    let clock = NativeClock;
    let deadline = Deadline::new(&clock);
    let budget = Budget::default();
    if let Some(code) = signals.pending().await {
        return Err(failure(code));
    }
    let request = Request::read(path, selected, &budget)?;
    let cache = Cache::open(&request.archives_root, &budget)?;
    let run = OwnedRun::local(&request.output, &budget)?;
    let cwd = run.path.clone();
    // No child tools are dispatched. The existing supervisor only owns final
    // bounded forwarding of the fixed public observation under the same Signals.
    let environment = crate::tooling::Environment::new();
    let mut executor = Native::new(&cwd, &environment, signals);
    let result = hydrate(&request, &cache, &run, &mut executor, &deadline).await;
    let cache_closed = cache.finish();
    let run_closed = run.finish();
    let mut bytes = result?;
    cache_closed.and(run_closed)?;
    executor.ready().await?;
    deadline.check(true)?;
    let _replay = budget.reserve(16)?;
    bytes.push(b'\n');
    executor.replay(&bytes, false, deadline.end()).await?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(failure(1));
    }
    deadline.check(true)
}
