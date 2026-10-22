//! Retained entry/dependency bytes and the existing private output owner.
use anyhow::{Result, ensure};
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

pub(super) use crate::bench::account_room::files::Directory;
pub(super) const NAMES: &[&str] = &[
    "web/e2e/restart-browser-phase.cjs",
    "web/e2e/restart-browser-channel.cjs",
    "web/e2e/runner-browser-owner.cjs",
    "web/e2e/lifecycle-cleanup.cjs",
    "web/e2e/runner-webkit-owner.cjs",
];
struct Source {
    file: Option<File>,
    path: PathBuf,
    stamp: Metadata,
    sha: [u8; 32],
}
#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
pub(super) struct Sources(Vec<Source>);
fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
impl Sources {
    pub fn open(root: &Path) -> Result<Self> {
        let mut value = Self(vec![]);
        let admitted: Result<()> = (|| {
            for name in NAMES {
                let path = root.join(name);
                let file = File::from(open(
                    &path,
                    OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK | OFlag::O_CLOEXEC,
                    Mode::empty(),
                )?);
                let stamp = match file.metadata() {
                    Ok(stamp) => stamp,
                    Err(error) => {
                        let _ = nix::unistd::close(file);
                        return Err(error.into());
                    }
                };
                let mut source = Source {
                    file: Some(file),
                    path,
                    stamp,
                    sha: [0; 32],
                };
                let observed = source.hash();
                value.0.push(source);
                value.0.last_mut().unwrap().sha = observed?;
            }
            value.check()
        })();
        if let Err(error) = admitted {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        for source in &self.0 {
            ensure!(
                same(&source.stamp, &source.file.as_ref().unwrap().metadata()?)
                    && same(&source.stamp, &std::fs::symlink_metadata(&source.path)?),
                "restart_source_changed"
            );
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let mut result = self.check();
        for source in &mut self.0 {
            let observed = source.hash().and_then(|hash| {
                ensure!(hash == source.sha, "restart_source_changed");
                Ok(())
            });
            result = result.and(observed);
            if let Some(file) = source.file.take() {
                result = result.and(nix::unistd::close(file).map_err(Into::into));
            }
        }
        result
    }
    pub fn hashes(&self) -> Vec<String> {
        self.0
            .iter()
            .map(|source| source.sha.iter().map(|v| format!("{v:02x}")).collect())
            .collect()
    }
}
impl Source {
    fn hash(&mut self) -> Result<[u8; 32]> {
        ensure!(
            self.stamp.is_file() && self.stamp.len() > 0 && self.stamp.len() <= 256 * 1024,
            "restart_source_refused"
        );
        self.file.as_mut().unwrap().seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut buffer = [0; 65536];
        let mut remaining = self.stamp.len();
        while remaining != 0 {
            let length = remaining.min(buffer.len() as u64) as usize;
            let count = self.file.as_mut().unwrap().read(&mut buffer[..length])?;
            ensure!(count > 0, "restart_source_changed");
            remaining -= count as u64;
            hash.update(&buffer[..count]);
        }
        ensure!(
            same(&self.stamp, &self.file.as_ref().unwrap().metadata()?)
                && same(&self.stamp, &std::fs::symlink_metadata(&self.path)?),
            "restart_source_changed"
        );
        Ok(hash.finalize().into())
    }
}
impl Drop for Source {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = nix::unistd::close(file);
        }
    }
}
