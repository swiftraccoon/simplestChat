//! Anchored private records and selected input observations, never a Cargo closure.

use super::plan::{Context, failure};
use crate::{ci::release_controller::files::Directory, tooling::Result};
use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    cell::Cell,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::MetadataExt,
    path::Path,
};

pub(super) const INPUT_LIMIT: u64 = 1024 * 1024;
pub(super) const LOG_LIMIT: u64 = 2 * 1024 * 1024;
const FILE_LIMIT: usize = 96;
const READ: OFlags = OFlags::RDONLY
    .union(OFlags::NOFOLLOW)
    .union(OFlags::NONBLOCK)
    .union(OFlags::CLOEXEC);

fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}
fn encode(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
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
fn private(directory: &Directory) -> Result<()> {
    directory.verify().map_err(|_| failure())?;
    let metadata = directory.file().metadata().map_err(|_| failure())?;
    if metadata.uid() != rustix::process::getuid().as_raw() || metadata.mode() & 0o777 != 0o700 {
        return Err(failure());
    }
    Ok(())
}

struct Input {
    directory: Directory,
    name: &'static str,
    file: File,
    stamp: Metadata,
}
pub(super) struct Inputs {
    files: Vec<Input>,
    pub sha256: String,
}
impl Inputs {
    pub fn read(context: &Context) -> Result<Self> {
        let mut value = Self {
            files: Vec::new(),
            sha256: String::new(),
        };
        let mut hash = Sha256::new();
        let read = (|| {
            for &(parent, name) in context.kind.inputs() {
                let directory =
                    Directory::open(&context.root.join(parent)).map_err(|_| failure())?;
                let file = rustix::fs::openat(directory.file(), name, READ, Mode::empty())
                    .map(File::from)
                    .map_err(|_| failure());
                let mut file = match file {
                    Ok(file) => file,
                    Err(error) => {
                        let _ = directory.finish();
                        return Err(error);
                    }
                };
                let observed = (|| {
                    let stamp = file.metadata().map_err(|_| failure())?;
                    if !stamp.is_file() || stamp.nlink() != 1 || stamp.len() > INPUT_LIMIT {
                        return Err(failure());
                    }
                    let mut bytes = Vec::new();
                    Read::by_ref(&mut file)
                        .take(INPUT_LIMIT + 1)
                        .read_to_end(&mut bytes)
                        .map_err(|_| failure())?;
                    if bytes.len() as u64 != stamp.len()
                        || !same(&stamp, &file.metadata().map_err(|_| failure())?)
                    {
                        return Err(failure());
                    }
                    directory.check(name, &stamp).map_err(|_| failure())?;
                    let relative = Path::new(parent).join(name);
                    let encoded = relative.as_os_str().as_encoded_bytes();
                    hash.update((encoded.len() as u64).to_be_bytes());
                    hash.update(encoded);
                    hash.update((bytes.len() as u64).to_be_bytes());
                    hash.update(&bytes);
                    Ok(stamp)
                })();
                match observed {
                    Ok(stamp) => value.files.push(Input {
                        directory,
                        name,
                        file,
                        stamp,
                    }),
                    Err(error) => {
                        let _ = close(file);
                        let _ = directory.finish();
                        return Err(error);
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = read {
            let _ = value.finish();
            return Err(error);
        }
        value.sha256 = encode(hash.finalize());
        Ok(value)
    }
    pub fn finish(self) -> Result<()> {
        self.finish_with(close)
    }
    fn finish_with(self, mut closer: impl FnMut(File) -> Result<()>) -> Result<()> {
        let mut failed = false;
        for input in self.files {
            failed |= input.directory.check(input.name, &input.stamp).is_err();
            failed |= match input.file.metadata() {
                Ok(stamp) => !same(&input.stamp, &stamp),
                Err(_) => true,
            };
            failed |= closer(input.file).is_err();
            failed |= input.directory.finish().is_err();
        }
        if failed { Err(failure()) } else { Ok(()) }
    }
}

/// Fixed labels only; command arguments, failure text and output never enter metadata.
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CommandRole {
    Context,
    Daemon,
    ImageCatalog,
    ImagePull,
    ImageInspect,
    Create,
    Inspect,
    Start,
    Wait,
    Logs,
    Locate,
    Remove,
    Unclassified,
}
impl CommandRole {
    fn of(command: &crate::ci::command::Command) -> Self {
        use crate::ci::command::Program;
        if command.program != Program::Docker {
            return Self::Unclassified;
        }
        let offset = usize::from(command.arguments.first().is_some_and(|v| v == "--host")) * 2;
        let first = command.arguments.get(offset).and_then(|v| v.to_str());
        let second = command.arguments.get(offset + 1).and_then(|v| v.to_str());
        match (first, second) {
            (Some("context"), Some("inspect")) => Self::Context,
            (Some("info"), _) => Self::Daemon,
            (Some("image"), Some("ls")) => Self::ImageCatalog,
            (Some("pull"), _) => Self::ImagePull,
            (Some("image"), Some("inspect")) => Self::ImageInspect,
            (Some("container"), Some("create")) => Self::Create,
            (Some("container"), Some("inspect")) => Self::Inspect,
            (Some("container"), Some("start")) => Self::Start,
            (Some("container"), Some("wait")) => Self::Wait,
            (Some("logs"), _) => Self::Logs,
            (Some("container"), Some("ls")) => Self::Locate,
            (Some("container"), Some("rm")) => Self::Remove,
            _ => Self::Unclassified,
        }
    }
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum AdapterStage {
    Admission,
    Ready,
    Budget,
    LogAdmission,
    LogPipe,
    PreDispatchBudget,
    Executor,
    LogReadback,
    PostDispatchBudget,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CommandObservation {
    /// Zero-based private log ordinal; failures before log admission allocate no file.
    log_ordinal: usize,
    role: CommandRole,
    cleanup: bool,
}
#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct AdapterFailure {
    command: CommandObservation,
    stage: AdapterStage,
    /// Adapter refusal status; this alone is not an observed child exit.
    code: u8,
    child_exit_code: Option<u8>,
}

pub(super) struct Artifacts {
    pub token: String,
    directory: Directory,
    next: Cell<usize>,
    last_command: Cell<Option<CommandObservation>>,
    first_adapter_failure: Cell<Option<AdapterFailure>>,
}
pub(super) struct Log {
    pub name: String,
    pub stamp: Metadata,
    pub maximum: u64,
}
impl Artifacts {
    pub fn new(context: &Context) -> Result<Self> {
        let mut random = File::open("/dev/urandom").map_err(|_| failure())?;
        let mut bytes = [0u8; 16];
        let read = random.read_exact(&mut bytes).map_err(|_| failure());
        let closed = close(random);
        read?;
        closed?;
        let token = encode(bytes);
        // Only the caller-selected temporary parent resolves ordinary OS aliases.
        let parent_path = context.temporary.canonicalize().map_err(|_| failure())?;
        let parent = Directory::open(&parent_path).map_err(|_| failure())?;
        let name = format!(
            "simplestchat-config-validation-{}-{token}",
            context.kind.label()
        );
        let made = rustix::fs::mkdirat(
            parent.file(),
            name.as_str(),
            Mode::from_bits_truncate(0o700),
        )
        .map_err(|_| failure());
        if let Err(error) = made {
            let _ = parent.finish();
            return Err(error);
        }
        let path = parent_path.join(&name);
        let directory = Directory::open(&path).map_err(|_| failure());
        let closed = parent.finish();
        let directory = directory?;
        let admitted = private(&directory).and(closed.map_err(|_| failure()));
        if let Err(error) = admitted {
            let _ = directory.finish();
            return Err(error);
        }
        Ok(Self {
            token,
            directory,
            next: Cell::new(0),
            last_command: Cell::new(None),
            first_adapter_failure: Cell::new(None),
        })
    }
    pub fn command(&self, command: &crate::ci::command::Command) -> CommandObservation {
        let observation = CommandObservation {
            log_ordinal: self.next.get(),
            role: CommandRole::of(command),
            cleanup: command.cleanup,
        };
        self.last_command.set(Some(observation));
        observation
    }
    pub fn observed<T>(
        &self,
        command: CommandObservation,
        stage: AdapterStage,
        result: Result<T>,
    ) -> Result<T> {
        if let Err(error) = &result
            && self.first_adapter_failure.get().is_none()
        {
            self.first_adapter_failure.set(Some(AdapterFailure {
                command,
                stage,
                code: error.code,
                child_exit_code: error.is_child_exit().then_some(error.code),
            }));
        }
        result
    }
    pub fn last_command(&self) -> Option<CommandObservation> {
        self.last_command.get()
    }
    pub fn first_adapter_failure(&self) -> Option<AdapterFailure> {
        self.first_adapter_failure.get()
    }
    pub fn write(&self, name: &str, bytes: &[u8]) -> Result<()> {
        if !matches!(
            name,
            "ownership.json" | "container.id" | "config-validation.json"
        ) || bytes.len() > 16 * 1024
        {
            return Err(failure());
        }
        private(&self.directory)?;
        let mut file = self.directory.create(name).map_err(|_| failure())?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| failure());
        let stamp = file.metadata().map_err(|_| failure());
        let closed = close(file);
        written?;
        closed?;
        let stamp = stamp?;
        if !stamp.is_file()
            || stamp.nlink() != 1
            || stamp.mode() & 0o777 != 0o600
            || stamp.uid() != rustix::process::getuid().as_raw()
        {
            return Err(failure());
        }
        self.directory.check(name, &stamp).map_err(|_| failure())?;
        private(&self.directory)
    }
    pub fn log(&self, maximum: u64) -> Result<(File, Log)> {
        self.log_with(maximum, || {})
    }
    fn log_with(&self, maximum: u64, before_open: impl FnOnce()) -> Result<(File, Log)> {
        private(&self.directory)?;
        let next = self.next.get();
        if next >= FILE_LIMIT || maximum == 0 || maximum > LOG_LIMIT {
            return Err(failure());
        }
        self.next.set(next + 1);
        let name = format!("command-{next:03}.log");
        before_open();
        let file = File::from(
            rustix::fs::openat(
                self.directory.file(),
                name.as_str(),
                OFlags::RDWR
                    | OFlags::CREATE
                    | OFlags::EXCL
                    | OFlags::NOFOLLOW
                    | OFlags::NONBLOCK
                    | OFlags::CLOEXEC,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(|_| failure())?,
        );
        let admitted = file.metadata().map_err(|_| failure()).and_then(|stamp| {
            if !stamp.is_file()
                || stamp.len() != 0
                || stamp.nlink() != 1
                || stamp.mode() & 0o777 != 0o600
                || stamp.uid() != rustix::process::getuid().as_raw()
            {
                return Err(failure());
            }
            self.directory.check(&name, &stamp).map_err(|_| failure())?;
            Ok(stamp)
        });
        match admitted {
            Ok(stamp) => Ok((
                file,
                Log {
                    name,
                    stamp,
                    maximum,
                },
            )),
            Err(error) => {
                let _ = close(file);
                Err(error)
            }
        }
    }
    pub fn read_log(&self, log: &Log) -> Result<Vec<u8>> {
        private(&self.directory)?;
        let mut file = File::from(
            rustix::fs::openat(
                self.directory.file(),
                log.name.as_str(),
                READ,
                Mode::empty(),
            )
            .map_err(|_| failure())?,
        );
        let result = (|| {
            let stamp = file.metadata().map_err(|_| failure())?;
            if !stamp.is_file()
                || !binding(&log.stamp, &stamp)
                || stamp.nlink() != 1
                || stamp.len() > log.maximum
            {
                return Err(failure());
            }
            let mut bytes = Vec::new();
            file.seek(SeekFrom::Start(0)).map_err(|_| failure())?;
            Read::by_ref(&mut file)
                .take(log.maximum + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| failure())?;
            if bytes.len() as u64 != stamp.len()
                || !same(&stamp, &file.metadata().map_err(|_| failure())?)
            {
                return Err(failure());
            }
            self.directory
                .check(&log.name, &stamp)
                .map_err(|_| failure())?;
            private(&self.directory)?;
            Ok(bytes)
        })();
        let closed = close(file);
        let bytes = result?;
        closed?;
        Ok(bytes)
    }
    pub fn finish(self) -> Result<()> {
        let checked = private(&self.directory);
        let closed = self.directory.finish().map_err(|_| failure());
        checked.and(closed)
    }
}
#[cfg(test)]
#[path = "files_tests.rs"]
mod tests;
