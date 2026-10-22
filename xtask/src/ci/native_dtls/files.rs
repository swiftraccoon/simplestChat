//! Anchored private copy/log ownership and retained executable role snapshots.

use super::{
    plan::{self, Context, failure},
    snapshot::{self, DIRECTORY},
};
use crate::{
    ci::{
        audit_rust::files::remove_contents_bounded,
        release_checks::files as selected,
        release_controller::files::{Directory, NamespaceBinding},
    },
    tooling::Result,
};
use rustix::fs::{AtFlags, Mode, OFlags};
use std::{
    cell::RefCell,
    collections::BTreeMap,
    ffi::{CStr, OsStr},
    fs::{File, Metadata},
    io::{Read, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};

pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure(1))
}
pub(super) fn same(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && a.mtime() == b.mtime()
        && a.mtime_nsec() == b.mtime_nsec()
        && a.ctime() == b.ctime()
        && a.ctime_nsec() == b.ctime_nsec()
}
fn mapped<T>(value: Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}

fn private(directory: &Directory) -> Result<()> {
    let metadata = io(directory.file().metadata())?;
    if !metadata.is_dir()
        || metadata.mode() & 0o777 != 0o700
        || metadata.uid() != rustix::process::getuid().as_raw()
    {
        return Err(failure(1));
    }
    mapped(directory.verify())
}

pub(super) struct Owned {
    parent: Directory,
    directory: Directory,
    name: String,
    logs: RefCell<BTreeMap<String, Metadata>>,
    pub path: PathBuf,
}
impl Owned {
    pub fn new(context: &Context) -> Result<Self> {
        Self::new_after(context, |_, _| {})
    }
    fn new_after(context: &Context, after: impl FnOnce(&Directory, &str)) -> Result<Self> {
        let parent = mapped(Directory::open(&context.temporary))?;
        let random = (|| {
            let mut random = io(File::open("/dev/urandom"))?;
            let mut bytes = [0u8; 16];
            let read = io(random.read_exact(&mut bytes));
            let closed = close(random);
            read?;
            closed?;
            Ok(bytes
                .iter()
                .map(|value| format!("{value:02x}"))
                .collect::<String>())
        })();
        let name = match random {
            Ok(value) => format!("simplestchat-native-dtls.{value}"),
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        if rustix::fs::mkdirat(
            parent.file(),
            name.as_str(),
            Mode::from_bits_truncate(0o700),
        )
        .is_err()
        {
            let _ = parent.finish();
            return Err(failure(1));
        }
        after(&parent, &name);
        let directory = Directory::open(&context.temporary).and_then(|value| value.descend(&name));
        match directory {
            Ok(directory) => {
                let owned = Self {
                    path: context.temporary.join(&name),
                    parent,
                    directory,
                    name,
                    logs: RefCell::default(),
                };
                if let Err(error) = owned.check() {
                    let (parent, _) = owned.cleanup(false);
                    let _ = parent.finish();
                    return Err(error);
                }
                Ok(owned)
            }
            Err(_) => {
                // No admitted leaf identity exists: retain an uncertain nonce
                // entry rather than deleting a possibly foreign replacement.
                let _ = parent.finish();
                Err(failure(1))
            }
        }
    }
    pub fn check(&self) -> Result<()> {
        private(&self.directory)
    }
    pub fn prepare(&self, context: &Context) -> Result<()> {
        self.check()?;
        let source = mapped(Directory::open(
            &context.root.join("vendor/mediasoup-sys-0.19.0"),
        ))?;
        let copied = (|| {
            rustix::fs::mkdirat(
                self.directory.file(),
                "worker",
                Mode::from_bits_truncate(0o700),
            )
            .map_err(|_| failure(1))?;
            let target = snapshot::directory(self.directory.file(), c"worker")?;
            let mut snapshot = snapshot::Snapshot::default();
            let copied = (|| {
                snapshot.copy(source.file(), &target, "", 0)?;
                snapshot.verify(source.file())?;
                let meson = super::source::meson(&snapshot.files)?;
                let output = rustix::fs::openat(
                    &target,
                    "meson.build.new",
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_bits_truncate(0o600),
                )
                .map(File::from)
                .map_err(|_| failure(1))?;
                let written = io((&output).write_all(&meson).and_then(|()| output.sync_all()));
                let closed = close(output);
                written?;
                closed?;
                rustix::fs::renameat(&target, "meson.build.new", &target, "meson.build")
                    .map_err(|_| failure(1))?;
                Ok(())
            })();
            let closed = close(target);
            copied?;
            closed
        })();
        let closed = mapped(source.finish());
        copied?;
        closed?;
        self.check()
    }
    pub fn log(&self, name: &str) -> Result<crate::ci::bounded_log::Log> {
        if !plan::LOGS.contains(&name) {
            return Err(failure(1));
        }
        self.check()?;
        let file = mapped(self.directory.create(name))?;
        let metadata = match file.metadata() {
            Ok(value) => value,
            Err(_) => {
                let _ = close(file);
                return Err(failure(1));
            }
        };
        self.logs.borrow_mut().insert(name.to_owned(), metadata);
        crate::ci::bounded_log::Log::start(file, plan::LOG_LIMIT).map_err(|_| failure(1))
    }
    pub fn read_log(&self, name: &str) -> Result<Vec<u8>> {
        if !plan::LOGS.contains(&name) {
            return Err(failure(1));
        }
        self.check()?;
        let original = self
            .logs
            .borrow()
            .get(name)
            .cloned()
            .ok_or_else(|| failure(1))?;
        let file = rustix::fs::openat(
            self.directory.file(),
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| failure(1))?;
        let read = (|| {
            let before = io(file.metadata())?;
            if !before.is_file()
                || before.len() > plan::LOG_LIMIT
                || before.dev() != original.dev()
                || before.ino() != original.ino()
                || before.mode() != original.mode()
                || before.uid() != original.uid()
                || before.gid() != original.gid()
                || before.nlink() != original.nlink()
            {
                return Err(failure(1));
            }
            let mut bytes = Vec::new();
            (&file)
                .take(plan::LOG_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure(1))?;
            if bytes.len() as u64 != before.len() || !same(&before, &io(file.metadata())?) {
                return Err(failure(1));
            }
            mapped(self.directory.check(name, &before))?;
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = read?;
        closed?;
        Ok(bytes)
    }
    pub fn cleanup(self, success: bool) -> (Directory, Result<()>) {
        let mut passed = self.check().is_ok();
        let mut count = 0;
        if passed && success {
            passed &= remove_contents_bounded(self.directory.file(), 0, &mut count, 100_000, 64);
        } else if passed {
            for name in [c"worker", c"out", c"build", c"build-test", c"install"] {
                passed &= remove_child(self.directory.file(), name, &mut count);
            }
        }
        let before = self.directory.file().metadata();
        passed &= self.directory.finish().is_ok();
        if success && passed {
            let visible = rustix::fs::openat(
                self.parent.file(),
                self.name.as_str(),
                DIRECTORY,
                Mode::empty(),
            )
            .map(File::from);
            if let Ok(visible) = visible {
                let observed = visible.metadata();
                passed &= before.as_ref().is_ok_and(|a| {
                    observed.as_ref().is_ok_and(|b| {
                        a.dev() == b.dev()
                            && a.ino() == b.ino()
                            && a.mode() == b.mode()
                            && a.uid() == b.uid()
                            && a.gid() == b.gid()
                    })
                });
                passed &= close(visible).is_ok();
                if passed {
                    passed &= rustix::fs::unlinkat(
                        self.parent.file(),
                        self.name.as_str(),
                        AtFlags::REMOVEDIR,
                    )
                    .is_ok();
                }
            } else {
                passed = false;
            }
        }
        (self.parent, if passed { Ok(()) } else { Err(failure(1)) })
    }
}
fn remove_child(parent: &File, name: &CStr, count: &mut usize) -> bool {
    let child = match rustix::fs::openat(parent, name, DIRECTORY, Mode::empty()) {
        Ok(fd) => File::from(fd),
        Err(rustix::io::Errno::NOENT) => return true,
        Err(_) => return false,
    };
    let before = child.metadata();
    let mut passed = remove_contents_bounded(&child, 0, count, 100_000, 64);
    let visible = snapshot::directory(parent, name);
    if let Ok(visible) = visible {
        let after = visible.metadata();
        passed &= before.as_ref().is_ok_and(|a| {
            after.as_ref().is_ok_and(|b| {
                a.dev() == b.dev()
                    && a.ino() == b.ino()
                    && a.mode() == b.mode()
                    && a.uid() == b.uid()
                    && a.gid() == b.gid()
            })
        });
        passed &= close(visible).is_ok();
        if passed {
            passed &= rustix::fs::unlinkat(parent, name, AtFlags::REMOVEDIR).is_ok();
        }
    } else {
        passed = false;
    }
    passed &= close(child).is_ok();
    passed
}

/// Metadata identity, not authenticated compiler/runtime provenance or FD-exec.
pub(in crate::ci) struct Executable {
    selected: selected::Executable,
    worker: bool,
    namespace: Option<Directory>,
}
impl std::fmt::Debug for Executable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("NativeDtlsExecutable")
    }
}
impl Executable {
    pub(in crate::ci) fn validate(&self, worker: bool) -> Result<&Path> {
        if worker != self.worker {
            return Err(failure(1));
        }
        if let Some(namespace) = &self.namespace {
            mapped(namespace.verify())?;
        }
        if worker {
            mapped(self.selected.validate())
        } else {
            mapped(self.selected.selected_path())
        }
    }
    pub(super) fn worker(owned: &Owned) -> Result<Self> {
        owned.check()?;
        let namespace = mapped(Directory::open(&owned.path).and_then(|v| v.descend("build-test")))?;
        let expected = owned.path.join("build-test/mediasoup-worker-test");
        let selected = selected::Executable::open(&expected).map_err(|_| failure(1));
        let selected = match selected {
            Ok(selected) => selected,
            Err(error) => {
                let _ = namespace.finish();
                return Err(error);
            }
        };
        let proof = Self {
            selected,
            worker: true,
            namespace: Some(namespace),
        };
        if !proof.validate(true).is_ok_and(|path| path == expected) {
            let _ = proof.close();
            return Err(failure(1));
        }
        Ok(proof)
    }
    pub fn close(self) -> Result<()> {
        let selected = mapped(self.selected.close());
        let namespace = self
            .namespace
            .map(Directory::finish)
            .transpose()
            .map_err(|_| failure(1));
        selected?;
        namespace?;
        Ok(())
    }
}
pub(super) struct Tools {
    pub python: Arc<Executable>,
    others: Vec<selected::Executable>,
}
impl Tools {
    pub fn open(context: &Context) -> Result<Self> {
        let mut selected_tools = Vec::new();
        for value in [
            &context.python,
            &context.cc,
            &context.cxx,
            &OsStr::new("pkg-config").to_owned(),
        ] {
            match selected::select(&context.cwd, &context.environment, value) {
                Ok(value) => selected_tools.push(value),
                Err(_) => {
                    for value in selected_tools {
                        let _ = value.close();
                    }
                    return Err(failure(2));
                }
            }
        }
        let python = selected_tools.remove(0);
        Ok(Self {
            python: Arc::new(Executable {
                selected: python,
                worker: false,
                namespace: None,
            }),
            others: selected_tools,
        })
    }
    pub fn validate(&self) -> Result<()> {
        self.python.validate(false)?;
        for value in &self.others {
            mapped(value.validate())?;
        }
        Ok(())
    }
    pub fn close(self) -> Result<()> {
        let mut passed = Arc::try_unwrap(self.python)
            .map_err(|_| failure(1))
            .and_then(Executable::close)
            .is_ok();
        for value in self.others {
            passed &= value.close().is_ok();
        }
        if passed { Ok(()) } else { Err(failure(1)) }
    }
}

/// A late publication/close failure can follow successful build-prefix removal.
/// Re-admit the original parent before retaining bounded logs under a fresh,
/// exclusive private name; never reopen or overwrite an unbound namespace.
pub(super) fn retain_after_failure(
    binding: &NamespaceBinding,
    temporary: &Path,
    prefix: &Path,
    logs: &[Vec<u8>],
) -> Result<()> {
    let parent = mapped(binding.reopen(temporary))?;
    let name = format!(
        "{}.failed",
        prefix
            .file_name()
            .and_then(|v| v.to_str())
            .ok_or_else(|| failure(1))?
    );
    if rustix::fs::mkdirat(
        parent.file(),
        name.as_str(),
        Mode::from_bits_truncate(0o700),
    )
    .is_err()
    {
        let _ = parent.finish();
        return Err(failure(1));
    }
    let directory = mapped(parent.descend(&name))?;
    if let Err(error) = private(&directory) {
        let _ = directory.finish();
        return Err(error);
    }
    let mut passed = true;
    for (name, bytes) in plan::LOGS.iter().zip(logs) {
        if bytes.len() as u64 > plan::LOG_LIMIT {
            passed = false;
            continue;
        }
        match directory.create(name) {
            Ok(file) => {
                passed &= (&file)
                    .write_all(bytes)
                    .and_then(|()| file.sync_all())
                    .is_ok();
                passed &= close(file).is_ok();
            }
            Err(_) => passed = false,
        }
    }
    passed &= directory.finish().is_ok();
    if passed { Ok(()) } else { Err(failure(1)) }
}

#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
