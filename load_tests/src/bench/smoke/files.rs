//! Reuse the established private directory owner; bind executable and stopped recorder FDs.

use super::config::Config;
use crate::bench::account_room::files::Directory;
use anyhow::{Result, ensure};
use nix::{
    fcntl::{OFlag, open},
    sys::stat::Mode,
};
use serde_json::Value;
use std::{
    fs::{self, File, Metadata},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
fn directory_binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file)?;
    Ok(())
}
const READ: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);

pub(super) struct Selected {
    file: Option<File>,
    path: PathBuf,
    stamp: Metadata,
    ancestors: Vec<(PathBuf, Metadata)>,
}
impl Selected {
    pub fn open(path: &Path) -> Result<Self> {
        let mut ancestors = Vec::new();
        for path in path.ancestors().skip(1) {
            let stamp = fs::symlink_metadata(path)?;
            ensure!(stamp.is_dir(), "smoke_executable_namespace_refused");
            ancestors.push((path.to_owned(), stamp));
        }
        let file = File::from(open(path, READ, Mode::empty())?);
        let stamp = match file.metadata() {
            Ok(value) => value,
            Err(error) => {
                let _ = close(file);
                return Err(error.into());
            }
        };
        let value = Self {
            file: Some(file),
            path: path.into(),
            stamp,
            ancestors,
        };
        if let Err(error) = value.check() {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn check(&self) -> Result<()> {
        ensure!(
            self.stamp.is_file() && self.stamp.mode() & 0o111 != 0 && self.stamp.len() > 0,
            "smoke_executable_refused"
        );
        for (path, stamp) in &self.ancestors {
            ensure!(
                directory_binding(stamp, &fs::symlink_metadata(path)?),
                "smoke_executable_changed"
            );
        }
        ensure!(
            same(&self.stamp, &self.file.as_ref().unwrap().metadata()?)
                && same(&self.stamp, &fs::symlink_metadata(&self.path)?),
            "smoke_executable_changed"
        );
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        let checked = self.check();
        let closed = close(self.file.take().unwrap());
        checked.and(closed)
    }
}
impl Drop for Selected {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
    }
}

pub(super) struct Recorder {
    file: Option<File>,
    stamp: Metadata,
    path: PathBuf,
}
impl Recorder {
    pub fn report(mut self, mut checkpoint: impl FnMut() -> bool) -> Result<Value> {
        let result = (|| {
            let file = self.file.as_ref().unwrap();
            let stopped = file.metadata()?;
            ensure!(binding(&self.stamp, &stopped), "smoke_recorder_changed");
            ensure!(
                same(&stopped, &fs::symlink_metadata(&self.path)?),
                "smoke_recorder_changed"
            );
            // The duplicate shares the retained open description; no path is
            // reopened between admitting stopped bytes and parsing them.
            let report = crate::report::diagnostics::read_owned(
                file.try_clone()?,
                crate::report::diagnostics::MAX_RECORDS,
                &mut checkpoint,
            )
            .map_err(anyhow::Error::msg)?;
            ensure!(
                same(&stopped, &file.metadata()?)
                    && same(&stopped, &fs::symlink_metadata(&self.path)?),
                "smoke_recorder_changed"
            );
            Ok(serde_json::to_value(report)?)
        })();
        let closed = close(self.file.take().unwrap());
        let report = result?;
        closed?;
        Ok(report)
    }
    pub fn finish(mut self) -> Result<()> {
        close(self.file.take().unwrap())
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
    }
}

pub(super) struct Output {
    directory: Option<Directory>,
    recorder: Option<Recorder>,
}
impl Output {
    pub fn new(config: &Config) -> Result<Self> {
        Ok(Self {
            directory: Some(Directory::new(&config.output)?),
            recorder: None,
        })
    }
    pub fn check(&self) -> Result<()> {
        self.directory.as_ref().unwrap().check()
    }
    pub fn observe_recorder(&mut self) -> Result<()> {
        self.check()?;
        if self.recorder.is_none() {
            let directory = self.directory.as_ref().unwrap();
            let (file, stamp) =
                directory.open_input("server-diagnostics.jsonl", super::report::RECORDER_BYTES)?;
            self.recorder = Some(Recorder {
                file: Some(file),
                stamp,
                path: directory.path().join("server-diagnostics.jsonl"),
            });
        }
        self.check()
    }
    pub fn take_recorder(&mut self) -> Result<Recorder> {
        self.check()?;
        self.recorder
            .take()
            .ok_or_else(|| anyhow::anyhow!("smoke_recorder_unbound"))
    }
    pub fn json(&self, name: &str, value: &impl serde::Serialize) -> Result<()> {
        self.directory.as_ref().unwrap().json(name, value)
    }
    pub fn log(&self, bytes: &[u8]) -> Result<()> {
        let directory = self.directory.as_ref().unwrap();
        let mut record = directory.create("server.log", super::config::LOG_BYTES)?;
        let offset = bytes
            .len()
            .saturating_sub(super::config::LOG_BYTES as usize);
        record.append(&bytes[offset..])?;
        record.finish(directory).map(|_| ())
    }
    pub fn withdraw(&self, value: &Value) -> Result<()> {
        let directory = self.directory.as_ref().unwrap();
        directory.remove(&["diagnostics-smoke.json"])?;
        directory.json("diagnostics-smoke.json", value)
    }
    pub fn finish(mut self) -> Result<()> {
        let record = self.recorder.take().map(Recorder::finish).transpose();
        let directory = self.directory.take().unwrap().finish();
        record?;
        directory
    }
}
