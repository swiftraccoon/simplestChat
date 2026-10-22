//! Retained namespaces, selected tools and one stable baseline-binary transfer.
//! These are optimistic bindings, not FD-exec or locks against same-UID writes.

use super::plan::{self, Plan};
use crate::{
    ci::{release_checks::files as selected, release_controller::files::Directory},
    tooling::{Environment, Result},
};
use rustix::fs::{AtFlags, Mode, OFlags};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);
pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| plan::failure(1))
}
pub(super) fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| plan::failure(1))
}
pub(super) fn binding(a: &Metadata, b: &Metadata) -> bool {
    a.dev() == b.dev()
        && a.ino() == b.ino()
        && a.mode() == b.mode()
        && a.uid() == b.uid()
        && a.gid() == b.gid()
}
fn stamp(a: &Metadata, b: &Metadata) -> bool {
    binding(a, b)
        && a.nlink() == b.nlink()
        && a.len() == b.len()
        && (a.mtime(), a.mtime_nsec(), a.ctime(), a.ctime_nsec())
            == (b.mtime(), b.mtime_nsec(), b.ctime(), b.ctime_nsec())
}

pub(super) struct Scope {
    pub path: PathBuf,
    parent: Option<Arc<Directory>>,
    name: Option<String>,
    file: Option<File>,
    initial: Metadata,
    created: Mutex<BTreeMap<String, Metadata>>,
    #[cfg(test)]
    removal: Mutex<Option<Arc<std::sync::atomic::AtomicBool>>>,
}
impl Scope {
    pub fn existing(path: &Path) -> Result<Arc<Self>> {
        let parent = Directory::open(path)?;
        let file = match io(parent.file().try_clone()) {
            Ok(file) => file,
            Err(error) => {
                let _ = parent.finish();
                return Err(error);
            }
        };
        let initial = match io(file.metadata()) {
            Ok(value) => value,
            Err(error) => {
                let _ = close(file);
                let _ = parent.finish();
                return Err(error);
            }
        };
        Ok(Arc::new(Self {
            path: path.into(),
            parent: Some(Arc::new(parent)),
            name: None,
            file: Some(file),
            initial,
            created: Mutex::default(),
            #[cfg(test)]
            removal: Mutex::default(),
        }))
    }
    pub fn fresh(parent: &Arc<Self>, name: &str) -> Result<Arc<Self>> {
        parent.check()?;
        rustix::fs::mkdirat(parent.file(), name, Mode::from_bits_truncate(0o700))
            .map_err(|_| plan::failure(1))?;
        let file = rustix::fs::openat(parent.file(), name, READ | OFlags::DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure(1))?;
        let result = (|| {
            let initial = io(file.metadata())?;
            if !initial.is_dir()
                || initial.mode() & 0o7777 != 0o700
                || initial.uid() != rustix::process::getuid().as_raw()
            {
                return Err(plan::failure(1));
            }
            parent.check()?;
            Ok(initial)
        })();
        let initial = match result {
            Ok(v) => v,
            Err(e) => {
                let _ = close(file);
                return Err(e);
            }
        };
        // Retain the same physical parent chain independently of its Scope.
        let parent_dir = match Directory::open(&parent.path) {
            Ok(v) => v,
            Err(e) => {
                let _ = close(file);
                return Err(e);
            }
        };
        let scope = Arc::new(Self {
            path: parent.path.join(name),
            parent: Some(Arc::new(parent_dir)),
            name: Some(name.into()),
            file: Some(file),
            initial,
            created: Mutex::default(),
            #[cfg(test)]
            removal: Mutex::default(),
        });
        scope.check()?;
        Ok(scope)
    }
    fn parent(&self) -> &Directory {
        self.parent.as_ref().expect("live parent")
    }
    pub fn file(&self) -> &File {
        self.file.as_ref().expect("live comparison directory")
    }
    pub fn check(&self) -> Result<()> {
        self.parent().verify()?;
        if !binding(&self.initial, &io(self.file().metadata())?) {
            return Err(plan::failure(1));
        }
        if let Some(name) = &self.name {
            let visible = rustix::fs::openat(
                self.parent().file(),
                name.as_str(),
                READ | OFlags::DIRECTORY,
                Mode::empty(),
            )
            .map(File::from)
            .map_err(|_| plan::failure(1))?;
            let observed = io(visible.metadata());
            let closed = close(visible);
            if !binding(&self.initial, &observed?) {
                return Err(plan::failure(1));
            }
            closed?;
        }
        Ok(())
    }
    pub fn absent(&self, name: &str) -> Result<bool> {
        self.check()?;
        match rustix::fs::statat(self.file(), name, AtFlags::SYMLINK_NOFOLLOW) {
            Err(rustix::io::Errno::NOENT) => Ok(true),
            Ok(_) => Ok(false),
            Err(_) => Err(plan::failure(1)),
        }
    }
    pub fn create(&self, name: &str) -> Result<File> {
        self.check()?;
        let file = rustix::fs::openat(
            self.file(),
            name,
            OFlags::RDWR
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK
                | OFlags::CLOEXEC,
            Mode::from_bits_truncate(0o600),
        )
        .map(File::from)
        .map_err(|_| plan::failure(1))?;
        let admitted = (|| {
            let metadata = io(file.metadata())?;
            if !metadata.is_file()
                || metadata.len() != 0
                || metadata.nlink() != 1
                || metadata.mode() & 0o7777 != 0o600
                || metadata.uid() != rustix::process::getuid().as_raw()
            {
                return Err(plan::failure(1));
            }
            self.check()?;
            Ok(metadata)
        })();
        let metadata = match admitted {
            Ok(v) => v,
            Err(e) => {
                let _ = close(file);
                return Err(e);
            }
        };
        self.created
            .lock()
            .expect("comparison creation registry")
            .insert(name.into(), metadata);
        Ok(file)
    }
    pub fn json(&self, name: &str, value: &impl serde::Serialize) -> Result<()> {
        let bytes = serde_json::to_vec(value).map_err(|_| plan::failure(1))?;
        if bytes.len() > 16 * 1024 {
            return Err(plan::failure(1));
        }
        let mut file = self.create(name)?;
        let result = io(file.write_all(&bytes).and_then(|()| file.sync_all()));
        let metadata = io(file.metadata());
        let closed = close(file);
        result?;
        closed?;
        self.visible(name, &metadata?)?;
        self.check()
    }
    fn visible(&self, name: &str, expected: &Metadata) -> Result<()> {
        let file = rustix::fs::openat(self.file(), name, READ, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure(1))?;
        let observed = io(file.metadata());
        let closed = close(file);
        if !stamp(expected, &observed?) {
            return Err(plan::failure(1));
        }
        if self
            .created
            .lock()
            .expect("comparison creation registry")
            .get(name)
            .is_some_and(|initial| !binding(initial, expected))
        {
            return Err(plan::failure(1));
        }
        closed?;
        self.check()
    }
    pub fn read(&self, name: &str, maximum: u64) -> Result<Vec<u8>> {
        self.check()?;
        let mut file = rustix::fs::openat(self.file(), name, READ, Mode::empty())
            .map(File::from)
            .map_err(|_| plan::failure(1))?;
        let result = (|| {
            let before = io(file.metadata())?;
            if !before.is_file()
                || before.nlink() != 1
                || before.len() > maximum
                || before.mode() & 0o7777 != 0o600
                || before.uid() != rustix::process::getuid().as_raw()
            {
                return Err(plan::failure(1));
            }
            let mut bytes = Vec::new();
            io(Read::by_ref(&mut file)
                .take(maximum + 1)
                .read_to_end(&mut bytes))?;
            if bytes.len() as u64 != before.len() || !stamp(&before, &io(file.metadata())?) {
                return Err(plan::failure(1));
            }
            self.visible(name, &before)?;
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = result?;
        closed?;
        Ok(bytes)
    }
    pub fn remove_leaf(&self) -> Result<()> {
        self.parent().verify()?;
        let name = self.name.as_ref().ok_or_else(|| plan::failure(1))?;
        match rustix::fs::statat(
            self.parent().file(),
            name.as_str(),
            AtFlags::SYMLINK_NOFOLLOW,
        ) {
            Err(rustix::io::Errno::NOENT) => {
                return if self.unlinked()? {
                    Ok(())
                } else {
                    Err(plan::failure(1))
                };
            }
            Err(_) => return Err(plan::failure(1)),
            Ok(_) => {}
        }
        self.check()?;
        if !crate::ci::audit_rust::files::remove_contents_bounded(
            self.file(),
            0,
            &mut 0,
            100_000,
            64,
        ) {
            return Err(plan::failure(1));
        }
        self.check()?;
        rustix::fs::unlinkat(self.parent().file(), name.as_str(), AtFlags::REMOVEDIR)
            .map_err(|_| plan::failure(1))
    }
    fn unlinked(&self) -> Result<bool> {
        #[cfg(test)]
        if let Some(witness) = self
            .removal
            .lock()
            .expect("fixture removal witness")
            .as_ref()
        {
            return Ok(witness.load(std::sync::atomic::Ordering::SeqCst));
        }
        Ok(io(self.file().metadata())?.nlink() == 0)
    }
    #[cfg(test)]
    pub fn removal_witness(&self, witness: Arc<std::sync::atomic::AtomicBool>) {
        *self.removal.lock().expect("fixture removal witness") = Some(witness);
    }
    pub fn finish(mut self, removed: bool) -> Result<()> {
        let checked = if removed {
            self.parent()
                .verify()
                .and_then(|()| match self.name.as_ref() {
                    Some(name) => match rustix::fs::statat(
                        self.parent().file(),
                        name.as_str(),
                        AtFlags::SYMLINK_NOFOLLOW,
                    ) {
                        Err(rustix::io::Errno::NOENT) => {
                            if self.unlinked()? {
                                Ok(())
                            } else {
                                Err(plan::failure(1))
                            }
                        }
                        _ => Err(plan::failure(1)),
                    },
                    None => Err(plan::failure(1)),
                })
        } else {
            self.check()
        };
        let closed = close(self.file.take().expect("live comparison directory"));
        let parent = match Arc::try_unwrap(self.parent.take().expect("live parent")) {
            Ok(parent) => parent.finish(),
            Err(_) => Err(plan::failure(1)),
        };
        checked?;
        closed?;
        parent
    }
}

pub(super) fn finish(scope: Arc<Scope>, removed: bool) -> Result<()> {
    Arc::try_unwrap(scope)
        .map_err(|_| plan::failure(1))?
        .finish(removed)
}
impl Drop for Scope {
    fn drop(&mut self) {
        if let Some(file) = self.file.take() {
            let _ = close(file);
        }
        if let Some(parent) = self.parent.take()
            && let Ok(parent) = Arc::try_unwrap(parent)
        {
            let _ = parent.finish();
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::ci) enum Role {
    Git,
    Cargo,
    Benchmark,
}
pub(super) struct Tool {
    role: Role,
    executable: Option<selected::Executable>,
}
impl Tool {
    pub fn select(plan: &Plan, role: Role) -> Result<Arc<Self>> {
        Self::select_at(plan, role, &plan.root)
    }
    pub fn select_at(plan: &Plan, role: Role, root: &Path) -> Result<Arc<Self>> {
        let name = match role {
            Role::Git => "git",
            Role::Cargo => "cargo",
            _ => return Err(plan::failure(1)),
        };
        Ok(Arc::new(Self {
            role,
            executable: Some(selected::select(root, &plan.environment, OsStr::new(name))?),
        }))
    }
    pub fn benchmark(plan: &Plan) -> Result<Arc<Self>> {
        Ok(Arc::new(Self {
            role: Role::Benchmark,
            executable: Some(selected::Executable::open(&plan.generator())?),
        }))
    }
    pub fn close(mut self) -> Result<()> {
        self.executable.take().expect("live tool").close()
    }
}
impl Drop for Tool {
    fn drop(&mut self) {
        if let Some(value) = self.executable.take() {
            let _ = value.close();
        }
    }
}

/// A closed comparison role carries its fixed cwd/environment alongside the
/// same retained tool proof. No other Native role can adopt this context.
pub(in crate::ci) struct Invocation {
    tool: Arc<Tool>,
    scope: Arc<Scope>,
    environment: Environment,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelectedComparisonInvocation")
    }
}
impl Invocation {
    pub(super) fn new(tool: Arc<Tool>, scope: Arc<Scope>, environment: Environment) -> Arc<Self> {
        Arc::new(Self {
            tool,
            scope,
            environment,
        })
    }
    pub fn validate(&self, role: Role) -> Result<&Path> {
        if self.tool.role != role {
            return Err(plan::failure(1));
        }
        self.scope.check()?;
        // Cargo commonly names a rustup shim: retaining its selected basename
        // preserves dispatch and per-worktree toolchain/configuration discovery.
        self.tool
            .executable
            .as_ref()
            .expect("live tool")
            .selected_path()
    }
    pub fn context(&self, role: Role) -> Result<(&Path, &Environment)> {
        self.validate(role)?;
        Ok((&self.scope.path, &self.environment))
    }
}

pub(super) fn close_tool(tool: Arc<Tool>) -> Result<()> {
    Arc::try_unwrap(tool).map_err(|_| plan::failure(1))?.close()
}

pub(super) fn nonce() -> Result<String> {
    let mut source = io(File::open("/dev/urandom"))?;
    let mut bytes = [0u8; 16];
    let read = io(source.read_exact(&mut bytes));
    let closed = close(source);
    read?;
    closed?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let text: String = bytes.iter().map(|v| format!("{v:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &text[..8],
        &text[8..12],
        &text[12..16],
        &text[16..20],
        &text[20..]
    ))
}

pub(super) fn copy_baseline(plan: &Plan, baseline: &Arc<Scope>) -> Result<String> {
    baseline.check()?;
    rustix::fs::mkdirat(baseline.file(), "target", Mode::from_bits_truncate(0o700))
        .map_err(|_| plan::failure(1))?;
    let target = Directory::open(&baseline.path.join("target"))?;
    let made = rustix::fs::mkdirat(target.file(), "release", Mode::from_bits_truncate(0o700))
        .map_err(|_| plan::failure(1));
    let target_closed = target.finish();
    made?;
    target_closed?;
    let source_directory = Directory::open(&plan.target.join("release"))?;
    let target_directory = match Directory::open(&baseline.path.join("target/release")) {
        Ok(v) => v,
        Err(e) => {
            let _ = source_directory.finish();
            return Err(e);
        }
    };
    let result = (|| {
        let mut source =
            rustix::fs::openat(source_directory.file(), "simplestChat", READ, Mode::empty())
                .map(File::from)
                .map_err(|_| plan::failure(1))?;
        let copied = (|| {
            let initial = io(source.metadata())?;
            if !initial.is_file()
                || initial.len() == 0
                || initial.len() > 2 * 1024 * 1024 * 1024
                || initial.mode() & 0o111 == 0
            {
                return Err(plan::failure(1));
            }
            let mut output = rustix::fs::openat(
                target_directory.file(),
                "simplestChat",
                OFlags::RDWR | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map(File::from)
            .map_err(|_| plan::failure(1))?;
            let copied = (|| {
                let created = io(output.metadata())?;
                let mut digest = Sha256::new();
                let mut size = 0;
                let mut buffer = [0u8; 65536];
                loop {
                    let count = io(source.read(&mut buffer))?;
                    if count == 0 {
                        break;
                    }
                    size += count as u64;
                    if size > initial.len() {
                        return Err(plan::failure(1));
                    }
                    io(output.write_all(&buffer[..count]))?;
                    digest.update(&buffer[..count]);
                }
                if size != initial.len() || !stamp(&initial, &io(source.metadata())?) {
                    return Err(plan::failure(1));
                }
                io(output.sync_all())?;
                rustix::fs::fchmod(&output, Mode::from_bits_truncate(0o755))
                    .map_err(|_| plan::failure(1))?;
                let out = io(output.metadata())?;
                if created.dev() != out.dev()
                    || created.ino() != out.ino()
                    || created.uid() != out.uid()
                    || created.gid() != out.gid()
                    || out.mode() & 0o7777 != 0o755
                    || out.nlink() != 1
                    || out.len() != size
                {
                    return Err(plan::failure(1));
                }
                io(output.seek(SeekFrom::Start(0)))?;
                let mut readback = Sha256::new();
                let mut read = 0;
                loop {
                    let count = io(output.read(&mut buffer))?;
                    if count == 0 {
                        break;
                    }
                    read += count as u64;
                    if read > size {
                        return Err(plan::failure(1));
                    }
                    readback.update(&buffer[..count]);
                }
                let expected = digest.finalize();
                if read != size
                    || readback.finalize() != expected
                    || !stamp(&out, &io(output.metadata())?)
                {
                    return Err(plan::failure(1));
                }
                target_directory.check("simplestChat", &out)?;
                source_directory.check("simplestChat", &initial)?;
                let digest = expected
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>();
                Ok((digest, initial))
            })();
            let closed = close(output);
            let copied = copied?;
            closed?;
            Ok(copied)
        })();
        let closed = close(source);
        let (digest, initial) = copied?;
        closed?;
        source_directory.check("simplestChat", &initial)?;
        rustix::fs::unlinkat(source_directory.file(), "simplestChat", AtFlags::empty())
            .map_err(|_| plan::failure(1))?;
        baseline.check()?;
        Ok(digest)
    })();
    let closed_source = source_directory.finish();
    let closed_target = target_directory.finish();
    let digest = result?;
    closed_source?;
    closed_target?;
    Ok(digest)
}
