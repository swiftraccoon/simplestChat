//! The receipt stays bound to the output directory witnessed at creation.
//! Namespace checks are optimistic observations, never locks or authentication.

use anyhow::{Result, ensure};
use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use std::{
    fs::{self, File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};

const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
const NAME: &str = "comparison-owner.json";

fn close(file: File) -> Result<()> {
    nix::unistd::close(file)?;
    Ok(())
}
fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn same(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.len() == b.len()
        && a.nlink() == b.nlink()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}

pub(super) struct Output {
    file: Option<File>,
    ancestors: Vec<(PathBuf, Metadata)>,
}
impl Output {
    pub fn open(path: &Path) -> Result<Self> {
        ensure!(
            path.is_absolute()
                && path.components().count() <= 20
                && path.as_os_str().as_encoded_bytes().len() <= 4096,
            "Invalid CI output path"
        );
        let mut value = Self {
            file: Some(File::from(rustix::fs::open(
                "/",
                READ | OFlags::DIRECTORY,
                Mode::empty(),
            )?)),
            ancestors: Vec::new(),
        };
        let mut visible = PathBuf::from("/");
        value
            .ancestors
            .push((visible.clone(), value.file().metadata()?));
        for component in path.components() {
            let Component::Normal(name) = component else {
                ensure!(
                    matches!(component, Component::RootDir),
                    "Invalid CI output component"
                );
                continue;
            };
            let next = File::from(rustix::fs::openat(
                value.file(),
                name,
                READ | OFlags::DIRECTORY,
                Mode::empty(),
            )?);
            let previous = value.file.replace(next).expect("live output directory");
            close(previous)?;
            visible.push(name);
            value
                .ancestors
                .push((visible.clone(), value.file().metadata()?));
        }
        let metadata = value.file().metadata()?;
        ensure!(
            metadata.mode() & 0o7777 == 0o700
                && metadata.uid() == rustix::process::getuid().as_raw(),
            "CI output is not private"
        );
        value.verify()?;
        Ok(value)
    }
    fn file(&self) -> &File {
        self.file.as_ref().expect("live output directory")
    }
    fn verify(&self) -> Result<()> {
        for (path, initial) in &self.ancestors {
            let current = fs::symlink_metadata(path)?;
            ensure!(
                current.is_dir() && binding(initial, &current),
                "CI output namespace changed"
            );
        }
        let original = &self.ancestors.last().expect("output witness").1;
        ensure!(
            binding(original, &self.file().metadata()?),
            "CI output directory changed"
        );
        Ok(())
    }
    fn visible(&self, expected: &Metadata) -> Result<()> {
        let file = File::from(rustix::fs::openat(self.file(), NAME, READ, Mode::empty())?);
        let observed = file.metadata();
        let closed = close(file);
        ensure!(same(expected, &observed?), "CI receipt identity changed");
        closed?;
        self.verify()
    }
    fn write(&self, receipt: &impl Serialize) -> Result<()> {
        self.verify()?;
        let bytes = serde_json::to_vec(receipt)?;
        ensure!(bytes.len() <= 16 * 1024, "CI receipt exceeds its bound");
        let mut file = File::from(rustix::fs::openat(
            self.file(),
            NAME,
            OFlags::RDWR
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )?);
        let written: Result<Metadata> = (|| {
            let created = file.metadata()?;
            ensure!(
                created.is_file()
                    && created.len() == 0
                    && created.nlink() == 1
                    && created.mode() & 0o7777 == 0o600
                    && created.uid() == rustix::process::getuid().as_raw(),
                "CI receipt type invalid"
            );
            file.write_all(&bytes)?;
            file.sync_all()?;
            let stamp = file.metadata()?;
            ensure!(
                binding(&created, &stamp)
                    && stamp.len() == bytes.len() as u64
                    && stamp.nlink() == 1,
                "CI receipt changed during publication"
            );
            file.seek(SeekFrom::Start(0))?;
            let mut readback = Vec::new();
            Read::by_ref(&mut file)
                .take(16 * 1024 + 1)
                .read_to_end(&mut readback)?;
            ensure!(
                readback == bytes && same(&stamp, &file.metadata()?),
                "CI receipt readback failed"
            );
            self.visible(&stamp)?;
            Ok(stamp)
        })();
        let closed = close(file);
        let stamp = written?;
        closed?;
        self.visible(&stamp)?;
        self.file().sync_all()?;
        self.verify()
    }
    pub fn publish(mut self, receipt: &impl Serialize) -> Result<()> {
        let result = self.write(receipt);
        let checked = self.verify();
        let closed = close(self.file.take().expect("live output directory"));
        result?;
        checked?;
        closed
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
    }
}

/// Byte observations bind the selected executables, without claiming build
/// authentication or a complete source/toolchain/native-library closure.
pub(super) struct Inputs(Vec<(PathBuf, String)>);
impl Inputs {
    pub fn capture(options: &crate::bench::options::Options, executable: &Path) -> Result<Self> {
        let mut inputs = Vec::new();
        for path in [
            &options.baseline_bin,
            &options.candidate_bin,
            &options.generator,
            executable,
        ] {
            inputs.push((
                path.to_path_buf(),
                crate::bench::provenance::hash_file(path)?.0,
            ));
        }
        Ok(Self(inputs))
    }
    pub fn finish(self) -> Result<()> {
        let mut changed = false;
        for (path, expected) in self.0 {
            changed |= !crate::bench::provenance::hash_file(&path)
                .is_ok_and(|(observed, _)| observed == expected);
        }
        ensure!(!changed, "CI benchmark executable changed");
        Ok(())
    }
}
