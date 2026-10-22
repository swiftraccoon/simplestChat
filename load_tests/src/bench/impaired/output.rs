//! Retain the newly created private leaf until every fixed output has closed.

use crate::bench::artifacts;
use anyhow::{Result, ensure};
use nix::{
    fcntl::{OFlag, open, openat},
    sys::stat::Mode,
};
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

const READ: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
const SUMMARY_LIMIT: u64 = 8 * 1024 * 1024;

fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.mode() == b.mode()
}

fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}

pub(super) struct Output {
    path: PathBuf,
    file: File,
    identity: Metadata,
}

impl Output {
    pub fn new(path: &Path) -> Result<Self> {
        Self::new_with(path, |metadata| metadata.uid())
    }
    pub(super) fn new_with(path: &Path, creator: impl FnOnce(&Metadata) -> u32) -> Result<Self> {
        artifacts::directory(path)?;
        let file = File::from(open(
            path,
            OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC,
            Mode::empty(),
        )?);
        let identity = match file.metadata() {
            Ok(value) => value,
            Err(error) => {
                let _ = nix::unistd::close(file);
                return Err(error.into());
            }
        };
        let value = Self {
            path: path.to_owned(),
            file,
            identity,
        };
        let admission: Result<()> = (|| {
            value.check()?;
            let witness = File::from(openat(
                &value.file,
                ".creator",
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )?);
            let stamp = witness.metadata();
            let closed = nix::unistd::close(witness);
            let stamp = stamp?;
            closed?;
            // The exclusive empty file is created by this process even when
            // the selected directory was replaced by a foreign0700leaf.
            ensure!(
                stamp.uid() == creator(&value.identity),
                "Impaired creator owner mismatch"
            );
            value.check()
        })();
        if let Err(error) = admission {
            let _ = nix::unistd::close(value.file);
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        let retained = self.file.metadata()?;
        let visible = fs::symlink_metadata(&self.path)?;
        for metadata in [&retained, &visible] {
            ensure!(
                metadata.is_dir()
                    && metadata.dev() == self.identity.dev()
                    && metadata.ino() == self.identity.ino()
                    && metadata.uid() == self.identity.uid()
                    && metadata.gid() == self.identity.gid()
                    && metadata.mode() & 0o7777 == 0o700,
                "Impaired output namespace changed"
            );
        }
        Ok(())
    }
    pub fn file(&self, name: &str) -> Result<File> {
        ensure!(
            matches!(name, "generator.log" | "server.log"),
            "Invalid impaired log"
        );
        self.fresh(name, || {}).map(|(file, _)| file)
    }
    pub fn json(&self, name: &str, value: &impl serde::Serialize) -> Result<()> {
        ensure!(
            matches!(name, "impaired-exit.json" | "impaired-summary.json"),
            "Invalid impaired output"
        );
        let bytes = serde_json::to_vec_pretty(value)?;
        ensure!(bytes.len() <= 16 * 1024, "Impaired output limit");
        self.write(name, &bytes, || {})
    }
    pub fn statistics(&self, bytes: &[u8]) -> Result<()> {
        ensure!(
            bytes.len() <= super::plan::CLI_LIMIT as usize,
            "Impaired statistics limit"
        );
        self.write("netem-stats.txt", bytes, || {})
    }

    /// Only the stopped generator's fixed summary basename is admitted. The
    /// retained output directory anchors the read even if its pathname moves;
    /// metadata and the visible leaf binding must remain stable through close.
    pub fn read_summary(&self) -> Result<serde_json::Value> {
        let bytes = self.read_summary_after(|| {})?;
        serde_json::from_slice(&bytes).map_err(|_| anyhow::anyhow!("Invalid impaired summary JSON"))
    }

    fn fresh(&self, name: &str, before_open: impl FnOnce()) -> Result<(File, Metadata)> {
        self.check()?;
        before_open();
        let file = File::from(openat(
            &self.file,
            name,
            OFlag::O_RDWR
                | OFlag::O_CREAT
                | OFlag::O_EXCL
                | OFlag::O_NOFOLLOW
                | OFlag::O_NONBLOCK
                | OFlag::O_CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let admitted = (|| {
            let stamp = file.metadata()?;
            ensure!(
                stamp.is_file()
                    && stamp.len() == 0
                    && stamp.nlink() == 1
                    && stamp.uid() == self.identity.uid()
                    && stamp.mode() & 0o7777 == 0o600,
                "Impaired output file refused"
            );
            self.check()?;
            Ok(stamp)
        })();
        match admitted {
            Ok(stamp) => Ok((file, stamp)),
            Err(error) => {
                let _ = nix::unistd::close(file);
                Err(error)
            }
        }
    }

    fn visible(&self, name: &str, stamp: &Metadata) -> Result<()> {
        let file = File::from(openat(&self.file, name, READ, Mode::empty())?);
        let observed = file.metadata();
        let closed = nix::unistd::close(file);
        ensure!(same(stamp, &observed?), "Impaired output file changed");
        closed?;
        self.check()
    }

    fn write(&self, name: &str, bytes: &[u8], before_open: impl FnOnce()) -> Result<()> {
        let (mut file, admitted) = self.fresh(name, before_open)?;
        let written: Result<()> = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            let stamp = file.metadata()?;
            ensure!(
                binding(&admitted, &stamp)
                    && stamp.nlink() == 1
                    && stamp.len() == bytes.len() as u64,
                "Impaired output file changed"
            );
            file.seek(SeekFrom::Start(0))?;
            let mut actual = Vec::new();
            Read::by_ref(&mut file)
                .take(bytes.len() as u64 + 1)
                .read_to_end(&mut actual)?;
            ensure!(
                actual == bytes && same(&stamp, &file.metadata()?),
                "Impaired output file changed"
            );
            self.visible(name, &stamp)
        })();
        let closed = nix::unistd::close(file);
        written?;
        closed?;
        Ok(())
    }

    fn read_summary_after(&self, before_open: impl FnOnce()) -> Result<Vec<u8>> {
        self.check()?;
        before_open();
        let name = "load_test_summary.json";
        let mut file = File::from(openat(&self.file, name, READ, Mode::empty())?);
        let read: Result<Vec<u8>> = (|| {
            let before = file.metadata()?;
            ensure!(
                before.is_file() && before.nlink() == 1 && before.len() <= SUMMARY_LIMIT,
                "Impaired summary file refused"
            );
            let mut bytes = Vec::new();
            Read::by_ref(&mut file)
                .take(SUMMARY_LIMIT + 1)
                .read_to_end(&mut bytes)?;
            ensure!(
                bytes.len() as u64 == before.len() && same(&before, &file.metadata()?),
                "Impaired summary changed"
            );
            self.visible(name, &before)?;
            Ok(bytes)
        })();
        let closed = nix::unistd::close(file);
        let bytes = read?;
        closed?;
        Ok(bytes)
    }
    pub fn finish(self) -> Result<()> {
        let checked = self.check();
        let closed = nix::unistd::close(self.file);
        checked?;
        closed?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
