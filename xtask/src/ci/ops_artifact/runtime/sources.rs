//! Original selected-checkout authority and a closed fresh execution projection.
//! No Python bytes are compiled into xtask or loaded from caller siblings.

use super::super::namespace::{self, Budget, Chain, ChainWitness};
use super::{
    failure,
    files::{Original, Output},
    projection::{self, Projection},
    worker::Checkpoint,
};
use crate::tooling::Result;
use std::{
    collections::BTreeSet,
    fs::Metadata,
    io::Read,
    path::{Component, Path},
};

pub(super) const NAMES: &[&str] = &[
    "build/security_vm_ops.py",
    "build/security_vm_ops_guest.py",
    "build/security_vm_vps_guest.py",
    "build/vps_ops_bootstrap.py",
    "build/security_vm.py",
    "build/security_vm_qmp.py",
    "build/security_context.py",
    "build/security_tools.py",
    "build/python-requirements.txt",
    "security/vm/ops-stage.yml",
    "security/vm/debian-cloud.json",
    "ops/ansible/ansible.cfg",
    "ops/ansible/requirements.txt",
    "ops/ansible/files/bounded_process.py",
    "ops/ansible/files/release_artifact.py",
    "ops/ansible/files/release_json.py",
    "ops/ansible/files/release_preflight.py",
    "ops/ansible/files/release-public.py",
    "ops/ansible/files/release_public.py",
    "ops/ansible/files/release_assets.py",
    "ops/ansible/files/browser_assets.py",
    "ops/ansible/files/runtime_profile.py",
    "ops/ansible/files/reboot-public.py",
    "ops/ansible/files/reboot_public.py",
    "ops/ansible/files/release_trust.py",
    "ops/ansible/files/fetch-release.py",
    "ops/ansible/files/release_fetch_receiver.py",
    "ops/ansible/files/force_release.py",
];
pub(super) struct Sources {
    originals: Vec<Original>,
    projection: Projection,
    cache: ChainWitness,
    cache_stamp: Metadata,
    budget: Budget,
    pub cloud_sha512: String,
}
impl Sources {
    pub fn open(
        caller: &Path,
        output: &Output,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        let mut originals = Vec::new();
        let mut bodies = Vec::new();
        let mut total = 0usize;
        let mut cloud = None;
        for name in NAMES {
            let (original, bytes) =
                Original::open(&caller.join(name), 256 * 1024, budget, checkpoint)?;
            total = total.checked_add(bytes.len()).ok_or_else(|| failure(1))?;
            if total > 2 * 1024 * 1024 {
                return Err(failure(1));
            }
            if *name == "security/vm/debian-cloud.json" {
                let value: serde_json::Value =
                    serde_json::from_slice(&bytes).map_err(|_| failure(1))?;
                let digest = value["sha512"].as_str().ok_or_else(|| failure(1))?;
                if value["distribution"] != "Debian 13"
                    || value["architecture"] != "amd64"
                    || digest.len() != 128
                    || !digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(failure(1));
                }
                cloud = Some(digest.to_owned());
            }
            originals.push(original);
            bodies.push(bytes);
        }
        let projection = Projection::create(
            &output.path.join("source"),
            output.witness(),
            &bodies,
            budget,
            checkpoint,
        )?;
        let directory = Chain::open(&output.path.join("python-cache"), budget)?;
        let result = directory.witness().and_then(|witness| {
            directory
                .file()
                .metadata()
                .map(|stamp| (witness, stamp))
                .map_err(|_| failure(1))
        });
        let closed = directory.finish();
        let (cache, cache_stamp) = result.and_then(|value| closed.map(|()| value))?;
        let value = Self {
            originals,
            projection,
            cache,
            cache_stamp,
            budget: budget.clone(),
            cloud_sha512: cloud.ok_or_else(|| failure(1))?,
        };
        value.check(checkpoint)?;
        Ok(value)
    }
    pub fn directory(&self) -> &Path {
        self.projection.path()
    }
    pub fn check(&self, checkpoint: &Checkpoint) -> Result<()> {
        for source in &self.originals {
            source.check(&self.budget, checkpoint)?;
        }
        self.projection.check(checkpoint)?;
        let cache = self.cache.reopen(&self.budget)?;
        let result = (|| {
            let stamp = cache.file().metadata().map_err(|_| failure(1))?;
            use std::os::unix::fs::MetadataExt;
            if !namespace::same(&self.cache_stamp, &stamp)
                || stamp.mode() & 0o7777 != 0o700
                || stamp.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure(1));
            }
            projection::entries(cache.file(), &self.budget, checkpoint, |name| {
                if !matches!(name.to_bytes(), b"." | b"..") {
                    return Err(failure(1));
                }
                Ok(())
            })?;
            if !namespace::same(
                &self.cache_stamp,
                &cache.file().metadata().map_err(|_| failure(1))?,
            ) {
                return Err(failure(1));
            }
            cache.verify()?;
            checkpoint.check()
        })();
        result.and(cache.finish())
    }
    pub fn archive(&self, bytes: &[u8], revision: &str) -> Result<()> {
        if bytes.len() > 2 * 1024 * 1024
            || bytes.len() < 1024
            || revision.len() != 40
            || !revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(failure(1));
        }
        let mut archive = tar::Archive::new(bytes);
        let mut actual = BTreeSet::new();
        let mut names = BTreeSet::new();
        let mut position = 0u64;
        let mut comment = false;
        for entry in archive.entries().map_err(|_| failure(1))?.raw(true) {
            let mut entry = entry.map_err(|_| failure(1))?;
            if names.len() >= 128
                || entry.raw_header_position() != position
                || entry.raw_file_position() != position + 512
                || entry.size() > 256 * 1024
                || entry.link_name_bytes().is_some()
            {
                return Err(failure(1));
            }
            let kind = entry.header().entry_type();
            let path_bytes = entry.path_bytes().into_owned();
            let name = std::str::from_utf8(&path_bytes).map_err(|_| failure(1))?;
            let name = if kind.is_dir() {
                name.strip_suffix('/').unwrap_or(name)
            } else {
                name
            };
            let path = Path::new(name);
            if !path.components().all(|c| matches!(c, Component::Normal(_)))
                || path
                    .components()
                    .collect::<std::path::PathBuf>()
                    .as_os_str()
                    != path.as_os_str()
                || !names.insert(name.to_owned())
            {
                return Err(failure(1));
            }
            let size = entry.size();
            let end = position
                .checked_add(512)
                .and_then(|n| n.checked_add(size.div_ceil(512) * 512))
                .ok_or_else(|| failure(1))?;
            if end > bytes.len() as u64
                || bytes[(position + 512 + size) as usize..end as usize]
                    .iter()
                    .any(|b| *b != 0)
            {
                return Err(failure(1));
            }
            if kind.is_pax_global_extensions() {
                if position != 0 || comment || name != "pax_global_header" {
                    return Err(failure(1));
                }
                let mut body = Vec::new();
                entry.read_to_end(&mut body).map_err(|_| failure(1))?;
                if body != format!("52 comment={revision}\n").as_bytes() {
                    return Err(failure(1));
                }
                comment = true;
            } else if kind.is_dir() {
                if size != 0
                    || !NAMES
                        .iter()
                        .any(|name| Path::new(name).starts_with(path) && Path::new(name) != path)
                {
                    return Err(failure(1));
                }
            } else {
                if !kind.is_file() {
                    return Err(failure(1));
                }
                let index = NAMES
                    .iter()
                    .position(|expected| *expected == name)
                    .ok_or_else(|| failure(1))?;
                if !actual.insert(name.to_owned()) || size != self.originals[index].bytes() {
                    return Err(failure(1));
                }
                let mut body = Vec::new();
                entry.read_to_end(&mut body).map_err(|_| failure(1))?;
                if body.len() as u64 != self.originals[index].bytes()
                    || super::super::sha(&body) != self.originals[index].sha256()
                {
                    return Err(failure(1));
                }
            }
            position = end;
        }
        if !comment
            || actual != NAMES.iter().map(|s| (*s).to_owned()).collect()
            || bytes.len().saturating_sub(position as usize) < 1024
            || bytes[position as usize..].iter().any(|b| *b != 0)
        {
            return Err(failure(1));
        }
        Ok(())
    }
}
