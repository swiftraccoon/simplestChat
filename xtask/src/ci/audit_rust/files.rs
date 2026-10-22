//! Fresh private scanner storage and bounded descriptor-relative retirement.

use super::plan::Context;
use crate::tooling::{Failure, Result};
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags};
use std::{
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::PathBuf,
};

const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);
pub(super) fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure("Rust audit storage operation failed."))
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure("Rust audit storage close failed."))
}
fn directory(parent: &File, name: &std::ffi::CStr) -> Result<File> {
    rustix::fs::openat(parent, name, DIRECTORY, Mode::empty())
        .map(File::from)
        .map_err(|_| failure("Rust audit directory unavailable."))
}
fn same(left: &Metadata, right: &Metadata) -> bool {
    left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.mode() == right.mode()
        && left.uid() == right.uid()
        && left.gid() == right.gid()
}
fn new_file(parent: &File, name: &str, bytes: &[u8]) -> Result<()> {
    let fd = rustix::fs::openat(
        parent,
        name,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::from_bits_truncate(0o600),
    )
    .map_err(|_| failure("Rust audit evidence creation failed."))?;
    let mut file = File::from(fd);
    let written = io(file.write_all(bytes).and_then(|()| file.sync_all()));
    let closed = close(file);
    written?;
    closed
}

pub(super) struct Owned {
    parent: File,
    directory: File,
    identity: Metadata,
    name: String,
    report: String,
    pub path: PathBuf,
}
impl Owned {
    pub(super) fn new(context: &Context) -> Result<Self> {
        let mut random = io(File::open("/dev/urandom"))?;
        let mut bytes = [0; 16];
        let read = io(random.read_exact(&mut bytes));
        let closed = close(random);
        read?;
        closed?;
        let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
        let name = format!("rust-audit-tools-{token}");
        let parent = rustix::fs::open(&context.temporary, DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| failure("Rust audit runner directory unavailable."))?;
        if rustix::fs::mkdirat(&parent, name.as_str(), Mode::from_bits_truncate(0o700)).is_err() {
            close(parent)?;
            return Err(failure("Fresh Rust audit directory unavailable."));
        }
        let child =
            rustix::fs::openat(&parent, name.as_str(), DIRECTORY, Mode::empty()).map(File::from);
        let directory = match child {
            Ok(file) => file,
            Err(_) => {
                let _ = rustix::fs::unlinkat(&parent, name.as_str(), AtFlags::REMOVEDIR);
                let _ = close(parent);
                return Err(failure("Rust audit directory open failed."));
            }
        };
        let identity = match directory.metadata() {
            Ok(value) => value,
            Err(_) => {
                let _ = close(directory);
                let _ = rustix::fs::unlinkat(&parent, name.as_str(), AtFlags::REMOVEDIR);
                let _ = close(parent);
                return Err(failure("Rust audit directory metadata unavailable."));
            }
        };
        Ok(Self {
            parent,
            directory,
            identity,
            path: context.temporary.join(&name),
            name,
            report: format!("rust-audit-{token}.json"),
        })
    }
    pub(super) fn prepare(&self) -> Result<()> {
        self.check()?;
        new_file(
            &self.directory,
            "intent.json",
            b"{\"operation\":\"rust-audit\"}\n",
        )
    }
    pub(super) fn check(&self) -> Result<()> {
        let visible = rustix::fs::open(&self.path, DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| failure("Rust audit namespace binding unavailable."))?;
        let observed = io(visible.metadata());
        let closed = close(visible);
        if !same(&observed?, &self.identity) {
            return Err(failure("Rust audit namespace changed."));
        }
        closed
    }
    pub(super) fn descriptor(&self) -> Result<File> {
        self.check()?;
        io(self.directory.try_clone())
    }
    pub(super) fn cleanup(self) -> (Reporter, Result<()>) {
        let mut budget = 0;
        let mut passed = remove_contents(&self.directory, 0, &mut budget);
        let visible =
            rustix::fs::openat(&self.parent, self.name.as_str(), DIRECTORY, Mode::empty())
                .map(File::from);
        match visible {
            Ok(file) => {
                let observed = file.metadata();
                let closed = close(file);
                if observed.is_ok_and(|value| same(&value, &self.identity)) {
                    passed &=
                        rustix::fs::unlinkat(&self.parent, self.name.as_str(), AtFlags::REMOVEDIR)
                            .is_ok();
                } else {
                    passed = false;
                }
                passed &= closed.is_ok();
            }
            Err(_) => passed = false,
        }
        passed &= close(self.directory).is_ok();
        let reporter = Reporter {
            parent: self.parent,
            name: self.report,
        };
        (
            reporter,
            if passed {
                Ok(())
            } else {
                Err(failure("Rust audit namespace cleanup failed."))
            },
        )
    }
}

fn remove_contents(root: &File, depth: usize, budget: &mut usize) -> bool {
    if depth > 16 {
        return false;
    }
    let entries = match Dir::read_from(root) {
        Ok(entries) => entries,
        Err(_) => return false,
    };
    let mut passed = true;
    for entry in entries {
        let Ok(entry) = entry else {
            passed = false;
            continue;
        };
        let name = entry.file_name();
        if [b".".as_slice(), b".."].contains(&name.to_bytes()) {
            continue;
        }
        *budget += 1;
        if *budget > 4096 {
            return false;
        }
        let metadata = match rustix::fs::statat(root, name, AtFlags::SYMLINK_NOFOLLOW) {
            Ok(value) => value,
            Err(_) => {
                passed = false;
                continue;
            }
        };
        if FileType::from_raw_mode(metadata.st_mode) == FileType::Directory {
            let child = match directory(root, name) {
                Ok(child) => child,
                Err(_) => {
                    passed = false;
                    continue;
                }
            };
            let initial = child.metadata();
            let parent = root.metadata();
            let admitted = initial.as_ref().is_ok_and(|child| {
                parent
                    .as_ref()
                    .is_ok_and(|parent| child.dev() == parent.dev() && child.uid() == parent.uid())
            });
            if admitted {
                passed &= remove_contents(&child, depth + 1, budget);
            } else {
                passed = false;
            }
            let observed = directory(root, name).and_then(|visible| {
                let observed = io(visible.metadata());
                let closed = close(visible);
                let observed = observed?;
                closed?;
                Ok(observed)
            });
            if admitted
                && observed.is_ok_and(|value| initial.as_ref().is_ok_and(|old| same(&value, old)))
            {
                passed &= rustix::fs::unlinkat(root, name, AtFlags::REMOVEDIR).is_ok();
            } else {
                passed = false;
            }
            passed &= close(child).is_ok();
        } else {
            passed &= rustix::fs::unlinkat(root, name, AtFlags::empty()).is_ok();
        }
    }
    passed
}

pub(super) struct Reporter {
    parent: File,
    name: String,
}
impl Reporter {
    pub(super) fn write(self, value: &impl serde::Serialize) -> Result<()> {
        let result = serde_json::to_vec_pretty(value)
            .map_err(|_| failure("Rust audit report invalid."))
            .and_then(|bytes| new_file(&self.parent, &self.name, &bytes));
        let closed = close(self.parent);
        result?;
        closed
    }
}
