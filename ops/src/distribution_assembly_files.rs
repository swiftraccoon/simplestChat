//! Exclusive flat output anchored to retained directories. Sources reuse the
//! shared snapshot reader; no payload name or namespace comes from an input.

use super::{Digest, Failure};
use crate::files::Snapshot;
use nix::fcntl::{OFlag, open, openat};
use nix::sys::stat::Mode;
use sha2::{Digest as _, Sha256};
use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::{ffi::OsStringExt, fs::MetadataExt},
    path::{Component, Path, PathBuf},
};

const FLAGS: OFlag = OFlag::O_RDONLY
    .union(OFlag::O_NOFOLLOW)
    .union(OFlag::O_NONBLOCK)
    .union(OFlag::O_CLOEXEC);
const PREPARATION: &str = "ops-preparation.json";

fn close(file: File) -> Result<(), Failure> {
    nix::unistd::close(file).map_err(|_| Failure::Close)
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
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}
pub(super) fn input_path(path: &Path) -> bool {
    path.is_absolute()
        && path.as_os_str().len() <= 4096
        && !path
            .as_os_str()
            .as_encoded_bytes()
            .iter()
            .any(|b| matches!(b, 0..=31 | 127))
        && path.components().count() <= 32
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}
fn payload(name: &str) -> bool {
    matches!(
        name,
        "simplestchat-ops"
            | "ops-release.json"
            | "ops-evidence.json"
            | "ops-predicate.json"
            | PREPARATION
    ) || (0..16).any(|index| name == format!("ops-evidence-{index:02}.bin"))
}
fn digest(bytes: &[u8]) -> Digest {
    Digest {
        sha256: hex(Sha256::digest(bytes).as_slice()),
        bytes: bytes.len() as u64,
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub(super) struct Output {
    path: PathBuf,
    directories: Vec<File>,
    stamps: Vec<Metadata>,
    names: Vec<OsString>,
    records: Vec<(String, Metadata)>,
    preparation: Option<Metadata>,
}
impl Output {
    fn open_parent(path: &Path) -> Result<Self, Failure> {
        if !input_path(path) || path.file_name().is_none() {
            return Err(Failure::Output);
        }
        let file = File::from(
            open("/", FLAGS | OFlag::O_DIRECTORY, Mode::empty()).map_err(|_| Failure::Output)?,
        );
        let stamp = match file.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                close(file)?;
                return Err(Failure::Output);
            }
        };
        let mut value = Self {
            path: path.into(),
            directories: vec![file],
            stamps: vec![stamp],
            names: vec![],
            records: vec![],
            preparation: None,
        };
        let opened = (|| {
            for part in path.parent().ok_or(Failure::Output)?.components() {
                if let Component::Normal(name) = part {
                    value.push(name)?;
                }
            }
            Ok(())
        })();
        if let Err(error) = opened {
            let _ = value.close_with(close);
            return Err(error);
        }
        Ok(value)
    }
    fn push(&mut self, name: &std::ffi::OsStr) -> Result<(), Failure> {
        let file = File::from(
            openat(self.file(), name, FLAGS | OFlag::O_DIRECTORY, Mode::empty())
                .map_err(|_| Failure::Output)?,
        );
        let stamp = match file.metadata() {
            Ok(stamp) => stamp,
            Err(_) => {
                close(file)?;
                return Err(Failure::Output);
            }
        };
        self.directories.push(file);
        self.stamps.push(stamp);
        self.names.push(name.into());
        Ok(())
    }
    pub fn create(path: &Path) -> Result<Self, Failure> {
        let mut value = Self::open_parent(path)?;
        let created = (|| {
            rustix::fs::mkdirat(
                value.file(),
                path.file_name().ok_or(Failure::Output)?,
                rustix::fs::Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| Failure::Output)?;
            value.push(path.file_name().ok_or(Failure::Output)?)?;
            value.verify()
        })();
        if let Err(error) = created {
            let _ = value.close_with(close);
            return Err(error);
        }
        Ok(value)
    }
    fn file(&self) -> &File {
        self.directories.last().unwrap()
    }
    fn namespace(&self) -> Result<(), Failure> {
        for (file, stamp) in self.directories.iter().zip(&self.stamps) {
            if !binding(stamp, &file.metadata().map_err(|_| Failure::Changed)?) {
                return Err(Failure::Changed);
            }
        }
        for (index, name) in self.names.iter().enumerate() {
            let file = File::from(
                openat(
                    &self.directories[index],
                    name.as_os_str(),
                    FLAGS | OFlag::O_DIRECTORY,
                    Mode::empty(),
                )
                .map_err(|_| Failure::Changed)?,
            );
            let stamp = file.metadata().map_err(|_| Failure::Changed);
            let closed = close(file);
            if !binding(&self.stamps[index + 1], &stamp?) {
                return Err(Failure::Changed);
            }
            closed?;
        }
        let stamp = self.file().metadata().map_err(|_| Failure::Changed)?;
        if stamp.mode() & 0o777 != 0o700 {
            return Err(Failure::Changed);
        }
        Ok(())
    }
    fn check(&self, name: &str, stamp: &Metadata) -> Result<(), Failure> {
        let file = File::from(
            openat(self.file(), name, FLAGS, Mode::empty()).map_err(|_| Failure::Changed)?,
        );
        let observed = file.metadata().map_err(|_| Failure::Changed);
        let closed = close(file);
        if !same(stamp, &observed?) {
            return Err(Failure::Changed);
        }
        closed
    }
    pub fn verify(&self) -> Result<(), Failure> {
        self.namespace()?;
        let mut names = Vec::new();
        for entry in rustix::fs::Dir::read_from(self.file()).map_err(|_| Failure::Changed)? {
            let entry = entry.map_err(|_| Failure::Changed)?;
            let name = entry.file_name().to_bytes();
            if [b".".as_slice(), b".."].contains(&name) {
                continue;
            }
            if names.len() >= 21 {
                return Err(Failure::Changed);
            }
            names.push(OsString::from_vec(name.to_vec()));
        }
        names.sort();
        let mut expected: Vec<OsString> =
            self.records.iter().map(|(name, _)| name.into()).collect();
        expected.sort();
        if names != expected {
            return Err(Failure::Changed);
        }
        for (name, stamp) in &self.records {
            self.check(name, stamp)?;
        }
        self.namespace()
    }
    fn target(&self, name: &str) -> Result<File, Failure> {
        self.target_with(name, self.stamps.last().unwrap().uid(), close)
    }
    pub(super) fn target_with(
        &self,
        name: &str,
        creator: u32,
        rejected_close: impl FnOnce(File) -> Result<(), Failure>,
    ) -> Result<File, Failure> {
        if !payload(name) || self.records.len() >= 21 {
            return Err(Failure::Output);
        }
        self.verify()?;
        let file = File::from(
            openat(
                self.file(),
                name,
                OFlag::O_WRONLY
                    | OFlag::O_CREAT
                    | OFlag::O_EXCL
                    | OFlag::O_NOFOLLOW
                    | OFlag::O_NONBLOCK
                    | OFlag::O_CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| Failure::Output)?,
        );
        let admitted = (|| {
            let stamp = file.metadata().map_err(|_| Failure::Output)?;
            // An exclusively created regular file is owned by the effective
            // creator UID. Compare that kernel witness with the retained fresh
            // directory before writing any payload; no process-spawning UID
            // query or extra process-binding dependency is needed.
            if !stamp.is_file()
                || stamp.len() != 0
                || stamp.mode() & 0o777 != 0o600
                || stamp.uid() != creator
            {
                return Err(Failure::Output);
            }
            self.namespace()
        })();
        if let Err(error) = admitted {
            let _ = rejected_close(file);
            return Err(error);
        }
        Ok(file)
    }
    fn recorded(
        &mut self,
        name: &str,
        file: File,
        written: Result<Digest, Failure>,
    ) -> Result<Digest, Failure> {
        self.recorded_after(name, file, written, |_| {})
    }
    fn recorded_after(
        &mut self,
        name: &str,
        file: File,
        written: Result<Digest, Failure>,
        after: impl FnOnce(&File),
    ) -> Result<Digest, Failure> {
        let synced = file.sync_all().map_err(|_| Failure::Output);
        after(&file);
        let stamp = file.metadata().map_err(|_| Failure::Output);
        if name == PREPARATION
            && let Ok(stamp) = &stamp
        {
            self.preparation = Some(stamp.clone());
        }
        let closed = close(file);
        let result = written?;
        synced?;
        closed?;
        let stamp = stamp?;
        if !stamp.is_file()
            || stamp.mode() & 0o777 != 0o600
            || stamp.uid() != self.stamps.last().unwrap().uid()
            || stamp.len() != result.bytes
        {
            return Err(Failure::Output);
        }
        self.check(name, &stamp)?;
        self.check_bytes(name, &stamp, &result)?;
        self.records.push((name.into(), stamp));
        self.verify()?;
        Ok(result)
    }
    fn check_bytes(&self, name: &str, stamp: &Metadata, expected: &Digest) -> Result<(), Failure> {
        // Bind the reopened descriptor to the exact writer inode/full stamp.
        // Hashing intended input bytes alone cannot detect an equal-size write
        // made before the first destination stamp was sampled.
        self.namespace()?;
        let mut snapshot =
            Snapshot::open(&self.path.join(name), expected.bytes).map_err(|_| Failure::Changed)?;
        let checked = (|| {
            if !same(
                stamp,
                &snapshot
                    .file_mut()
                    .metadata()
                    .map_err(|_| Failure::Changed)?,
            ) {
                return Err(Failure::Changed);
            }
            let mut stream = snapshot.checked_stream();
            let mut digest = Sha256::new();
            let mut bytes = 0_u64;
            let mut buffer = [0; 65536];
            loop {
                let size = stream.read(&mut buffer).map_err(|_| Failure::Changed)?;
                if size == 0 {
                    break;
                }
                bytes = bytes.checked_add(size as u64).ok_or(Failure::Bounds)?;
                if bytes > expected.bytes {
                    return Err(Failure::Changed);
                }
                digest.update(&buffer[..size]);
            }
            if bytes != expected.bytes
                || hex(Sha256::finalize(digest).as_slice()) != expected.sha256
            {
                return Err(Failure::Changed);
            }
            self.check(name, stamp)?;
            self.namespace()
        })();
        let closed = snapshot.finish().map_err(|_| Failure::Close);
        checked?;
        closed
    }
    pub fn write(&mut self, name: &str, bytes: &[u8]) -> Result<Digest, Failure> {
        if bytes.len() > super::MAX_RECORD {
            return Err(Failure::Bounds);
        }
        let mut file = self.target(name)?;
        let written = file
            .write_all(bytes)
            .map(|()| digest(bytes))
            .map_err(|_| Failure::Output);
        self.recorded(name, file, written)
    }
    #[cfg(test)]
    pub(super) fn write_after(
        &mut self,
        name: &str,
        bytes: &[u8],
        after: impl FnOnce(&File),
    ) -> Result<Digest, Failure> {
        let mut file = self.target(name)?;
        let written = file
            .write_all(bytes)
            .map(|()| digest(bytes))
            .map_err(|_| Failure::Output);
        self.recorded_after(name, file, written, after)
    }
    pub fn copy(
        &mut self,
        name: &str,
        path: &Path,
        expected: &str,
        count: Option<u64>,
        maximum: u64,
    ) -> Result<Digest, Failure> {
        if !input_path(path) {
            return Err(Failure::Input);
        }
        let mut source = Snapshot::open(path, maximum).map_err(|_| Failure::Input)?;
        let copied = (|| {
            let mut file = self.target(name)?;
            let written = (|| {
                let mut digest = Sha256::new();
                let mut bytes = 0_u64;
                let mut buffer = [0; 65536];
                let mut stream = source.checked_stream();
                loop {
                    let size = stream.read(&mut buffer).map_err(|_| Failure::Changed)?;
                    if size == 0 {
                        break;
                    }
                    bytes = bytes.checked_add(size as u64).ok_or(Failure::Bounds)?;
                    if bytes > maximum {
                        return Err(Failure::Bounds);
                    }
                    file.write_all(&buffer[..size])
                        .map_err(|_| Failure::Output)?;
                    digest.update(&buffer[..size]);
                }
                let sha256 = hex(Sha256::finalize(digest).as_slice());
                if sha256 != expected
                    || count.is_some_and(|count| count != bytes)
                    || bytes != source.length()
                {
                    return Err(Failure::Evidence);
                }
                Ok(Digest { sha256, bytes })
            })();
            self.recorded(name, file, written)
        })();
        let closed = source.finish().map_err(|_| Failure::Close);
        let result = copied?;
        closed?;
        Ok(result)
    }
    /// Only remove our own last inventory inode; foreign replacements survive.
    pub fn rollback(&self) {
        if let Some(stamp) = &self.preparation
            && self.check(PREPARATION, stamp).is_ok()
        {
            let _ = rustix::fs::unlinkat(self.file(), PREPARATION, rustix::fs::AtFlags::empty());
        }
    }
    fn close_with(
        self,
        mut closer: impl FnMut(File) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let mut failed = false;
        for file in self.directories.into_iter().rev() {
            failed |= closer(file).is_err();
        }
        if failed { Err(Failure::Close) } else { Ok(()) }
    }
    pub fn finish(self) -> Result<(), Failure> {
        self.finish_with(close)
    }
    pub(super) fn finish_with(
        self,
        closer: impl FnMut(File) -> Result<(), Failure>,
    ) -> Result<(), Failure> {
        let checked = self.verify();
        if checked.is_err() {
            self.rollback();
        }
        let path = self.path.clone();
        let root = self.stamps.last().unwrap().clone();
        let receipt = self.preparation.clone();
        let closed = self.close_with(closer);
        if closed.is_err()
            && let Some(stamp) = receipt
        {
            // Closing the publisher is not atomic with visibility. Reopen only
            // the same nonlink directory and remove only our exact final inode.
            if let Ok(mut reopened) = Self::open_parent(&path) {
                if reopened.push(path.file_name().unwrap()).is_ok()
                    && reopened
                        .stamps
                        .last()
                        .is_some_and(|current| binding(&root, current))
                    && reopened.check(PREPARATION, &stamp).is_ok()
                {
                    let _ = rustix::fs::unlinkat(
                        reopened.file(),
                        PREPARATION,
                        rustix::fs::AtFlags::empty(),
                    );
                }
                let _ = reopened.close_with(close);
            }
        }
        checked?;
        closed
    }
}
