//! Release-only Git/Docker dispatch over the shared reserved child owner.
//!
//! The operation supplies original admitted tools, cwd, environment, log and an
//! absolute body deadline. This private adapter creates no selection authority.

use super::control::{Failure, Owner, Result};
use crate::owned_process::{self, Group};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{File, Metadata},
    io::Write,
    os::unix::{fs::MetadataExt, process::CommandExt, process::ExitStatusExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, process::ChildStderr, process::ChildStdout};

const STREAM_LIMIT: u64 = 2 * 1024 * 1024;
const INSPECTION_LIMIT: u64 = 2 * 1024 * 1024;
const RETIRE: Duration = Duration::from_secs(20);
const TERM: Duration = Duration::from_secs(15);
const DRAIN: Duration = Duration::from_secs(6);
const FINAL_TAIL: Duration = Duration::from_secs(14);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Role {
    Git,
    Docker,
}

/// Live original handles are never serializable, cloneable or reconstructed
/// from command evidence. Only the release operation constructs this value.
pub(super) struct Invocation {
    pub role: Role,
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub cwd: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub body_end: Instant,
    pub log: File,
    /// Original archive writer loan. Binary stdout never enters the log.
    pub archive: Option<File>,
    pub capture: bool,
    pub allow_failure: bool,
}

#[derive(Clone)]
pub(super) struct Seal {
    pub bytes: u64,
    pub sha256: String,
    pub metadata: Metadata,
}
pub(super) struct Report {
    pub status: i32,
    pub inspection: Vec<u8>,
    pub log: Seal,
}
pub(super) struct Completion {
    pub result: Result<Report>,
    pub observation: Observation,
}

/// Partial facts remain available after every refusal, including a real reap
/// followed by a close failure. A seal or status alone is not command success.
pub(super) struct Observation {
    pub pid: Option<u32>,
    pub exit_status: Option<i32>,
    pub timed_out: bool,
    pub output_limited: bool,
    pub inspection_limited: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub log: Option<Seal>,
    pub archive: Option<Seal>,
    pub group_retired: bool,
    pub reaped: bool,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    pub stdout_closed: bool,
    pub stderr_closed: bool,
    pub log_closed: bool,
    pub archive_closed: bool,
    pub elapsed: Duration,
    pub cleanup_errors: Vec<Failure>,
}
impl Observation {
    fn new() -> Self {
        Self {
            pid: None,
            exit_status: None,
            timed_out: false,
            output_limited: false,
            inspection_limited: false,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_sha256: String::new(),
            stderr_sha256: String::new(),
            log: None,
            archive: None,
            group_retired: false,
            reaped: false,
            stdout_eof: false,
            stderr_eof: false,
            stdout_closed: false,
            stderr_closed: false,
            log_closed: false,
            archive_closed: true,
            elapsed: Duration::ZERO,
            cleanup_errors: Vec::new(),
        }
    }
    fn cleanup_error(&mut self, owner: &mut Owner, failure: Failure) {
        self.cleanup_errors.push(failure);
        owner.record(failure);
    }
}

#[derive(Default)]
struct Faults {
    #[cfg(test)]
    stdout_close: bool,
    #[cfg(test)]
    log_close: bool,
    #[cfg(test)]
    archive_close: bool,
}
impl Faults {
    fn archive_close(&self) -> bool {
        #[cfg(test)]
        {
            self.archive_close
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn stdout_close(&self) -> bool {
        #[cfg(test)]
        {
            self.stdout_close
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn log_close(&self) -> bool {
        #[cfg(test)]
        {
            self.log_close
        }
        #[cfg(not(test))]
        {
            false
        }
    }
}

struct Streams {
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    stdout_stopped: bool,
    stderr_stopped: bool,
    out_buffer: Vec<u8>,
    err_buffer: Vec<u8>,
    out_hash: Sha256,
    err_hash: Sha256,
    log_hash: Sha256,
    log_bytes: u64,
    inspection: Vec<u8>,
    capture: bool,
    write_failed: bool,
    archive: Option<File>,
    archive_write_failed: bool,
}
impl Streams {
    fn new(capture: bool) -> Self {
        Self {
            stdout: None,
            stderr: None,
            stdout_stopped: false,
            stderr_stopped: false,
            out_buffer: vec![0_u8; 65536],
            err_buffer: vec![0_u8; 65536],
            out_hash: Sha256::new(),
            err_hash: Sha256::new(),
            log_hash: Sha256::new(),
            log_bytes: 0,
            inspection: Vec::new(),
            capture,
            write_failed: false,
            archive: None,
            archive_write_failed: false,
        }
    }
    fn complete(&self, observation: &Observation) -> bool {
        observation.stdout_eof && observation.stderr_eof
    }
    fn readable(&self, observation: &Observation) -> bool {
        (self.stdout.is_some() && !observation.stdout_eof && !self.stdout_stopped)
            || (self.stderr.is_some() && !observation.stderr_eof && !self.stderr_stopped)
    }
    fn accept(
        &mut self,
        stdout: bool,
        bytes: &[u8],
        log: &mut File,
        observation: &mut Observation,
    ) -> Result<()> {
        let counter = if stdout {
            &mut observation.stdout_bytes
        } else {
            &mut observation.stderr_bytes
        };
        let archive = stdout && self.archive.is_some();
        let limit = if archive {
            crate::release::archive::MAX_ARCHIVE_BYTES
        } else {
            STREAM_LIMIT
        };
        let accepted = limit.saturating_sub(*counter).min(bytes.len() as u64) as usize;
        let written = if archive {
            self.archive
                .as_mut()
                .expect("selected original archive writer")
                .write_all(&bytes[..accepted])
        } else {
            log.write_all(&bytes[..accepted])
        };
        if written.is_err() {
            if archive {
                self.archive_write_failed = true;
            } else {
                self.write_failed = true;
            }
            self.stdout_stopped = true;
            self.stderr_stopped = true;
            return Err(Failure::Io);
        }
        *counter += accepted as u64;
        if stdout {
            self.out_hash.update(&bytes[..accepted]);
        } else {
            self.err_hash.update(&bytes[..accepted]);
        }
        if !archive {
            self.log_hash.update(&bytes[..accepted]);
            self.log_bytes += accepted as u64;
        }
        if self.capture && !archive {
            let remaining = (INSPECTION_LIMIT as usize).saturating_sub(self.inspection.len());
            self.inspection
                .extend_from_slice(&bytes[..accepted.min(remaining)]);
            observation.inspection_limited = self.log_bytes > INSPECTION_LIMIT;
        }
        if accepted != bytes.len() {
            observation.output_limited = true;
            if stdout {
                self.stdout_stopped = true;
            } else {
                self.stderr_stopped = true;
            }
            return Err(Failure::Bounds);
        }
        Ok(())
    }

    /// A single owner writes diagnostics in observed read order and, for save,
    /// binary stdout into the original archive. Independent reads prevent one
    /// full pipe from blocking its sibling's progress. File writes are synchronous
    /// bounded chunks; the next owner check fences their elapsed time.
    async fn read_one(&mut self, log: &mut File, observation: &mut Observation) -> Result<()> {
        enum Read {
            Out(std::io::Result<usize>),
            Err(std::io::Result<usize>),
            Tick,
        }
        let out_active = self.stdout.is_some() && !observation.stdout_eof && !self.stdout_stopped;
        let err_active = self.stderr.is_some() && !observation.stderr_eof && !self.stderr_stopped;
        let event = {
            // Both allocations stay in Streams during the await. Dropping this
            // pending future releases only these borrows, never its buffers.
            let stdout = &mut self.stdout;
            let stderr = &mut self.stderr;
            let out = &mut self.out_buffer;
            let err = &mut self.err_buffer;
            tokio::select! {
                size = async {
                    stdout.as_mut().expect("admitted stdout").read(out.as_mut_slice()).await
                }, if out_active => Read::Out(size),
                size = async {
                    stderr.as_mut().expect("admitted stderr").read(err.as_mut_slice()).await
                }, if err_active => Read::Err(size),
                _ = tokio::time::sleep(Duration::from_millis(10)) => Read::Tick,
            }
        };
        match event {
            Read::Out(Ok(0)) => {
                observation.stdout_eof = true;
                Ok(())
            }
            Read::Err(Ok(0)) => {
                observation.stderr_eof = true;
                Ok(())
            }
            Read::Out(Ok(size)) => {
                let out = std::mem::take(&mut self.out_buffer);
                let result = self.accept(true, &out[..size], log, observation);
                self.out_buffer = out;
                result
            }
            Read::Err(Ok(size)) => {
                let err = std::mem::take(&mut self.err_buffer);
                let result = self.accept(false, &err[..size], log, observation);
                self.err_buffer = err;
                result
            }
            Read::Out(Err(_)) => {
                self.stdout_stopped = true;
                Err(Failure::Io)
            }
            Read::Err(Err(_)) => {
                self.stderr_stopped = true;
                Err(Failure::Io)
            }
            Read::Tick => Ok(()),
        }
    }
}

fn hash(value: &Sha256) -> String {
    value
        .clone()
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn log_admission(log: &File) -> Result<()> {
    let metadata = log.metadata().map_err(|_| Failure::Io)?;
    if !metadata.is_file()
        || metadata.len() != 0
        || metadata.mode() & 0o7777 != 0o600
        || metadata.uid() != rustix::process::getuid().as_raw()
        || metadata.nlink() != 1
    {
        return Err(Failure::Input);
    }
    Ok(())
}
fn bounded_end(owner: &Owner, cap: Duration, retained: Duration) -> Instant {
    owner
        .end()
        .checked_sub(retained)
        .unwrap_or_else(|| owner.now())
        .min(owner.now().checked_add(cap).unwrap_or_else(|| owner.now()))
}

/// Always returns the actual partial observation after every admitted finalizer.
pub(super) async fn run(owner: &mut Owner, invocation: Invocation) -> Completion {
    run_with(owner, invocation, Faults::default()).await
}
async fn run_with(owner: &mut Owner, invocation: Invocation, faults: Faults) -> Completion {
    let started = owner.now();
    let Invocation {
        role,
        program,
        arguments,
        cwd,
        environment,
        body_end,
        mut log,
        archive,
        capture,
        allow_failure,
    } = invocation;
    let mut observation = Observation::new();
    let mut streams = Streams::new(capture);
    let has_archive = archive.is_some();
    observation.archive_closed = !has_archive;
    streams.archive = archive;
    let mut group = None;
    let mut completed_io_end = None;
    let admission = (|| {
        owner.check()?;
        if !program.is_absolute() || !cwd.is_absolute() {
            return Err(Failure::Input);
        }
        log_admission(&log)?;
        if let Some(archive) = &streams.archive {
            if role != Role::Docker || capture {
                return Err(Failure::Input);
            }
            log_admission(archive)?;
            let archive_metadata = archive.metadata().map_err(|_| Failure::Io)?;
            let log_metadata = log.metadata().map_err(|_| Failure::Io)?;
            if archive_metadata.dev() == log_metadata.dev()
                && archive_metadata.ino() == log_metadata.ino()
            {
                return Err(Failure::Input);
            }
        }
        // The operation already clips each body by its future phase reserves.
        // Refuse a malformed/late request rather than spend its finalizer time.
        if body_end <= owner.now()
            || body_end
                > owner
                    .end()
                    .checked_sub(RETIRE + DRAIN + FINAL_TAIL)
                    .unwrap_or(started)
        {
            return Err(Failure::Deadline);
        }
        let mut command = Command::new(program);
        command
            .args(arguments)
            .current_dir(cwd)
            .env_clear()
            .envs(environment)
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|_| Failure::Io)?;
        observation.pid = Some(child.id());
        group = Some(Group::new(child));
        drop(command);
        Ok(())
    })();
    if let Err(failure) = admission {
        observation.timed_out = failure == Failure::Deadline;
        owner.record(failure);
    }
    if let Some(group) = &mut group {
        let stdout = group.child().stdout.take().expect("created piped stdout");
        let stderr = group.child().stderr.take().expect("created piped stderr");
        // Conversion errors consume their argument through Tokio's Drop path;
        // they remain negative, and the independent peer is still attempted.
        match ChildStdout::from_std(stdout) {
            Ok(pipe) => streams.stdout = Some(pipe),
            Err(_) => owner.record(Failure::Io),
        }
        match ChildStderr::from_std(stderr) {
            Ok(pipe) => streams.stderr = Some(pipe),
            Err(_) => owner.record(Failure::Io),
        }
        let work = async {
            loop {
                owner.check()?;
                if owner.now() >= body_end {
                    observation.timed_out = true;
                    return Err(Failure::Deadline);
                }
                let exited = owned_process::exited_now(group.pid()).map_err(|_| Failure::Io)?;
                if streams.complete(&observation) && exited {
                    if observation.inspection_limited {
                        return Err(Failure::Bounds);
                    }
                    return Ok(());
                }
                streams.read_one(&mut log, &mut observation).await?;
            }
        }
        .await;
        if let Err(failure) = work {
            owner.record(failure);
        }
        let retire_end = body_end.checked_add(RETIRE).unwrap_or(body_end);
        retire(
            owner,
            group,
            &mut streams,
            &mut log,
            &mut observation,
            retire_end,
            allow_failure,
        )
        .await;
        let drain_end = bounded_end(owner, DRAIN, FINAL_TAIL)
            .min(body_end.checked_add(RETIRE + DRAIN).unwrap_or(body_end));
        completed_io_end = Some(drain_end);
        while !streams.complete(&observation)
            && streams.readable(&observation)
            && owner.now() < drain_end
        {
            let _ = owner.check();
            if let Err(failure) = streams.read_one(&mut log, &mut observation).await {
                observation.cleanup_error(owner, failure);
            }
            if streams.stdout_stopped && streams.stderr_stopped {
                break;
            }
        }
        if !streams.complete(&observation) {
            observation.cleanup_error(owner, Failure::Close);
        }
        close_pipes(owner, &mut streams, &mut observation, &faults);
    }
    finish_archive(owner, &mut streams, &mut observation, &faults);
    finish_log(owner, log, &streams, &mut observation, &faults);
    if completed_io_end.is_some_and(|end| owner.now() >= end) {
        observation.cleanup_error(owner, Failure::Deadline);
    }
    observation.stdout_sha256 = hash(&streams.out_hash);
    observation.stderr_sha256 = hash(&streams.err_hash);
    observation.elapsed = owner.now().saturating_duration_since(started);
    let final_check = owner.check();
    let result = final_check.and_then(|()| {
        let status = observation.exit_status.ok_or(Failure::Command)?;
        let log = observation.log.clone().ok_or(Failure::Close)?;
        if !observation.group_retired
            || !observation.reaped
            || !observation.stdout_eof
            || !observation.stderr_eof
            || !observation.stdout_closed
            || !observation.stderr_closed
            || !observation.log_closed
            || !observation.archive_closed
            || (has_archive && observation.archive.is_none())
        {
            return Err(Failure::Close);
        }
        Ok(Report {
            status,
            inspection: streams.inspection,
            log,
        })
    });
    if let Err(failure) = result.as_ref() {
        owner.record(*failure);
    }
    Completion {
        result,
        observation,
    }
}

async fn retire(
    owner: &mut Owner,
    group: &mut Group,
    streams: &mut Streams,
    log: &mut File,
    observation: &mut Observation,
    original_end: Instant,
    allow_failure: bool,
) {
    let end = bounded_end(owner, RETIRE, DRAIN + FINAL_TAIL).min(original_end);
    let _ = owner.check();
    let pid = group.pid();
    if owner.primary().is_some() {
        match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
            Ok(()) | Err(rustix::io::Errno::SRCH) => {}
            Err(_) => observation.cleanup_error(owner, Failure::Io),
        }
        let term_end = end
            .checked_sub(Duration::from_secs(5))
            .unwrap_or_else(|| owner.now())
            .min(owner.now().checked_add(TERM).unwrap_or_else(|| owner.now()));
        while owner.now() < term_end {
            let _ = owner.check();
            match owned_process::exited_now(pid) {
                Ok(true) => break,
                Ok(false) => {}
                Err(_) => {
                    observation.cleanup_error(owner, Failure::Io);
                    break;
                }
            }
            if let Err(failure) = streams.read_one(log, observation).await {
                observation.cleanup_error(owner, failure);
            }
        }
    }
    // Always finish the still-reserved group, even after a normal leader exit.
    // A background descendant must not survive because it closed its pipe.
    observation.group_retired = owned_process::finish_before(pid, end).await;
    if !observation.group_retired {
        observation.cleanup_error(owner, Failure::Close);
    }
    let _ = owner.check();
    let mut terminal = false;
    while owner.now() < end {
        let _ = owner.check();
        match owned_process::exited_now(pid) {
            Ok(true) => {
                terminal = true;
                break;
            }
            Ok(false) => {}
            Err(_) => {
                observation.cleanup_error(owner, Failure::Io);
                break;
            }
        }
        if let Err(failure) = streams.read_one(log, observation).await {
            observation.cleanup_error(owner, failure);
        }
    }
    if !terminal {
        observation.cleanup_error(owner, Failure::Deadline);
        // This is still our unreaped direct child. Attempt its independent
        // finalizer even if group proof failed. No later group signal follows.
        if group.child().kill().is_err() {
            observation.cleanup_error(owner, Failure::Close);
        }
        // Kernel wait/reap latency is not a hard wall bound. Do not detach this
        // admitted peer or claim Drop as successful cleanup after a timeout.
        if owned_process::exited(pid).await.is_err() {
            observation.cleanup_error(owner, Failure::Close);
        }
    }
    match group.reap().await {
        Ok(status) => {
            observation.exit_status = status.code().or_else(|| status.signal().map(|n| -n));
            observation.reaped = true;
            if observation.exit_status.is_some_and(|status| status != 0) && !allow_failure {
                // Latch the actual consuming status before any later deadline,
                // pending-owner or close observation. Child 124/130/143 remain
                // ordinary command failures.
                owner.record(Failure::Command);
            }
        }
        Err(_) => observation.cleanup_error(owner, Failure::Close),
    }
    if owner.now() >= end {
        observation.cleanup_error(owner, Failure::Deadline);
    }
}

fn close_pipes(
    owner: &mut Owner,
    streams: &mut Streams,
    observation: &mut Observation,
    faults: &Faults,
) {
    if let Some(pipe) = streams.stdout.take() {
        observation.stdout_closed = pipe
            .into_owned_fd()
            .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
            .is_ok();
        if faults.stdout_close() {
            observation.stdout_closed = false;
        }
        if !observation.stdout_closed {
            observation.cleanup_error(owner, Failure::Close);
        }
    }
    if let Some(pipe) = streams.stderr.take() {
        observation.stderr_closed = pipe
            .into_owned_fd()
            .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
            .is_ok();
        if !observation.stderr_closed {
            observation.cleanup_error(owner, Failure::Close);
        }
    }
}
fn finish_archive(
    owner: &mut Owner,
    streams: &mut Streams,
    observation: &mut Observation,
    faults: &Faults,
) {
    let Some(mut archive) = streams.archive.take() else {
        return;
    };
    let flushed = archive.flush().and_then(|()| archive.sync_all());
    if flushed.is_err() {
        observation.cleanup_error(owner, Failure::Io);
    }
    match archive.metadata() {
        Ok(metadata)
            if metadata.len() == observation.stdout_bytes && !streams.archive_write_failed =>
        {
            observation.archive = Some(Seal {
                bytes: observation.stdout_bytes,
                sha256: hash(&streams.out_hash),
                metadata,
            });
        }
        _ => observation.cleanup_error(owner, Failure::Io),
    }
    let closed = nix::unistd::close(archive);
    observation.archive_closed = closed.is_ok() && !faults.archive_close();
    if !observation.archive_closed {
        observation.cleanup_error(owner, Failure::Close);
    }
}

fn finish_log(
    owner: &mut Owner,
    mut log: File,
    streams: &Streams,
    observation: &mut Observation,
    faults: &Faults,
) {
    let flushed = log.flush().and_then(|()| log.sync_all());
    let metadata = log.metadata();
    let closed = nix::unistd::close(log);
    observation.log_closed = closed.is_ok() && !faults.log_close();
    if flushed.is_err() {
        observation.cleanup_error(owner, Failure::Io);
    }
    if !observation.log_closed {
        observation.cleanup_error(owner, Failure::Close);
    }
    match metadata {
        Ok(metadata) if metadata.len() == streams.log_bytes && !streams.write_failed => {
            observation.log = Some(Seal {
                bytes: streams.log_bytes,
                sha256: hash(&streams.log_hash),
                metadata,
            });
        }
        _ => observation.cleanup_error(owner, Failure::Io),
    }
}

#[cfg(test)]
#[path = "build_process_tests.rs"]
mod tests;
