//! One fixed host Docker invocation with original deadlines and explicit settlement.
//!
//! Program and arguments come from the retained Docker role. This adapter
//! establishes neither source selection nor cryptographic verification.

use super::super::control::{Cause, Owner, Result, Window};
use crate::owned_process::{self, Group};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::File,
    io::Seek,
    os::{
        fd::OwnedFd,
        unix::{
            fs::{FileExt, MetadataExt},
            process::{CommandExt, ExitStatusExt},
        },
    },
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
};

pub(super) const OUT_LIMIT: u64 = 2 * 1024 * 1024;
pub(super) const ERR_LIMIT: u64 = 2 * 1024 * 1024;
const BODY: Duration = Duration::from_secs(300);
const TERM: Duration = Duration::from_secs(5);
const REAP: Duration = Duration::from_secs(5);

pub(in crate::release::attestation) struct Invocation {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub body_end: Instant,
    pub settlement_end: Instant,
    pub cleanup: bool,
}
/// Original descriptors remain borrowed from their operation owner. The input
/// duplicate transferred to Command has SDK-owned close status, not a checked
/// original-close claim; its caller must explicitly finish its retained source.
#[derive(Default)]
pub(in crate::release::attestation) struct Io<'a> {
    pub input: Option<&'a File>,
    pub output: Option<Sink<'a>>,
    pub stderr: Option<Sink<'a>>,
    pub capture_output: bool,
    pub allow_nonzero: bool,
}
pub(in crate::release::attestation) struct Sink<'a> {
    pub file: &'a File,
    pub maximum: u64,
}
const BULK_LIMIT: u64 = 4 * 1024 * 1024 * 1024;

pub(in crate::release::attestation) struct Completion {
    pub result: Result<()>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub observation: Observation,
}
/// Counts and hashes cover the actual observed prefix. A refused stream stops
/// after its one overflow sentinel; only natural EOF proves the complete stream.
pub(in crate::release::attestation) struct Observation {
    pub pid: Option<u32>,
    pub exit_status: Option<i32>,
    pub stdin_closed: bool,
    pub stdin_inherited: bool,
    pub stdin_duplicate_sdk_owned: bool,
    pub bulk_bytes: u64,
    pub bulk_sha256: String,
    pub bulk_complete: bool,
    pub stderr_sink_bytes: u64,
    pub stderr_sink_sha256: String,
    pub stderr_sink_complete: bool,
    pub stdout_closed: bool,
    pub stderr_closed: bool,
    pub stdin_close_attempted: bool,
    pub stdout_close_attempted: bool,
    pub stderr_close_attempted: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_sha256: String,
    pub stderr_sha256: String,
    pub stdout_eof: bool,
    pub stderr_eof: bool,
    /// EOF observed and fenced during the live body, before any retirement.
    pub stdout_body_eof: bool,
    pub stderr_body_eof: bool,
    pub stdout_limited: bool,
    pub stderr_limited: bool,
    pub group_retired: bool,
    pub reaped: bool,
    pub timed_out: bool,
    /// Actual deadlines of this invocation, excluding an inherited primary cause.
    pub role_timed_out: bool,
    pub elapsed: Duration,
    pub cleanup_errors: Vec<Cause>,
}
impl Observation {
    fn new() -> Self {
        Self {
            pid: None,
            exit_status: None,
            stdin_closed: false,
            stdin_inherited: false,
            stdin_duplicate_sdk_owned: false,
            bulk_bytes: 0,
            bulk_sha256: String::new(),
            bulk_complete: false,
            stderr_sink_bytes: 0,
            stderr_sink_sha256: String::new(),
            stderr_sink_complete: false,
            stdout_closed: false,
            stderr_closed: false,
            stdin_close_attempted: false,
            stdout_close_attempted: false,
            stderr_close_attempted: false,
            stdout_bytes: 0,
            stderr_bytes: 0,
            stdout_sha256: String::new(),
            stderr_sha256: String::new(),
            stdout_eof: false,
            stderr_eof: false,
            stdout_body_eof: false,
            stderr_body_eof: false,
            stdout_limited: false,
            stderr_limited: false,
            group_retired: false,
            reaped: false,
            timed_out: false,
            role_timed_out: false,
            elapsed: Duration::ZERO,
            cleanup_errors: Vec::new(),
        }
    }
    fn secondary(&mut self, owner: &mut Owner, cause: Cause) {
        // A fixed finite Cause set bounds this collection, including repeated
        // cleanup checks. The original owner always preserves its first cause.
        if !self.cleanup_errors.contains(&cause) {
            self.cleanup_errors.push(cause);
        }
        owner.record(cause);
    }
}
#[derive(Default)]
struct Faults {
    #[cfg(test)]
    input_close: bool,
    #[cfg(test)]
    output_close: bool,
    #[cfg(test)]
    error_close: bool,
    #[cfg(test)]
    output_read: bool,
    #[cfg(test)]
    error_read: bool,
    #[cfg(test)]
    output_conversion: bool,
    #[cfg(test)]
    bulk_write_after: Option<u64>,
    #[cfg(test)]
    stderr_write_after: Option<u64>,
    #[cfg(test)]
    terminal_before_read: bool,
    #[cfg(test)]
    late_bulk_read: bool,
    #[cfg(test)]
    late_stderr_read: bool,
}
impl Faults {
    fn close(&self, stream: u8) -> bool {
        #[cfg(test)]
        {
            match stream {
                0 => self.input_close,
                1 => self.output_close,
                _ => self.error_close,
            }
        }
        #[cfg(not(test))]
        {
            let _ = stream;
            false
        }
    }
    fn read(&self, stdout: bool) -> bool {
        #[cfg(test)]
        {
            if stdout {
                self.output_read
            } else {
                self.error_read
            }
        }
        #[cfg(not(test))]
        {
            let _ = stdout;
            false
        }
    }
    fn conversion(&self) -> bool {
        #[cfg(test)]
        {
            self.output_conversion
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
    out_stopped: bool,
    err_stopped: bool,
    out_buffer: Vec<u8>,
    err_buffer: Vec<u8>,
    out_hash: Sha256,
    err_hash: Sha256,
    captured: Vec<u8>,
    error_capture: Vec<u8>,
    out_limit: u64,
    bulk: bool,
    capture_bulk: bool,
    pending: usize,
    bulk_hash: Sha256,
    stderr_pending: usize,
    stderr_sink_hash: Sha256,
}
impl Streams {
    fn new(out_limit: u64) -> Self {
        Self {
            out_limit,
            bulk: false,
            capture_bulk: false,
            pending: 0,
            bulk_hash: Sha256::new(),
            stderr_pending: 0,
            stderr_sink_hash: Sha256::new(),
            stdout: None,
            stderr: None,
            out_stopped: false,
            err_stopped: false,
            out_buffer: vec![0; 65536],
            err_buffer: vec![0; 65536],
            out_hash: Sha256::new(),
            err_hash: Sha256::new(),
            captured: Vec::new(),
            error_capture: Vec::new(),
        }
    }
    fn complete(&self, observed: &Observation) -> bool {
        observed.stdout_eof && observed.stderr_eof
    }
    fn readable(&self, observed: &Observation) -> bool {
        (self.stdout.is_some() && !self.out_stopped && !observed.stdout_eof)
            || (self.stderr.is_some() && !self.err_stopped && !observed.stderr_eof)
    }
    fn accept(&mut self, stdout: bool, bytes: &[u8], observed: &mut Observation) -> Result<()> {
        let (count, limit, hash) = if stdout {
            (
                &mut observed.stdout_bytes,
                self.out_limit,
                &mut self.out_hash,
            )
        } else {
            (&mut observed.stderr_bytes, ERR_LIMIT, &mut self.err_hash)
        };
        let accepted = limit.saturating_sub(*count).min(bytes.len() as u64) as usize;
        *count += bytes.len() as u64;
        hash.update(bytes);
        if stdout && self.bulk {
            self.pending = accepted;
            if self.capture_bulk {
                self.captured.extend_from_slice(&bytes[..accepted]);
            }
        } else if stdout {
            self.captured.extend_from_slice(&bytes[..accepted]);
        } else {
            self.stderr_pending = accepted;
            self.error_capture.extend_from_slice(&bytes[..accepted]);
        }
        if *count > limit {
            if stdout {
                observed.stdout_limited = true;
                self.out_stopped = true;
            } else {
                observed.stderr_limited = true;
                self.err_stopped = true;
            }
            Err(Cause::Bounds)
        } else {
            Ok(())
        }
    }
    async fn read_one(&mut self, observed: &mut Observation, faults: &Faults) -> Result<()> {
        let out = self.stdout.is_some() && !self.out_stopped && !observed.stdout_eof;
        let err = self.stderr.is_some() && !self.err_stopped && !observed.stderr_eof;
        if out && faults.read(true) {
            self.out_stopped = true;
            return Err(Cause::Io);
        }
        if err && faults.read(false) {
            self.err_stopped = true;
            return Err(Cause::Io);
        }
        let out_max = (self.out_limit.saturating_sub(observed.stdout_bytes) + 1)
            .min(self.out_buffer.len() as u64) as usize;
        let err_max = (ERR_LIMIT.saturating_sub(observed.stderr_bytes) + 1)
            .min(self.err_buffer.len() as u64) as usize;
        let event = {
            // The same nonempty allocations remain in this owner throughout a
            // losing/cancelled read future. Only disjoint borrows cross await.
            let (stdout, stderr) = (&mut self.stdout, &mut self.stderr);
            let (out_buffer, err_buffer) = (&mut self.out_buffer, &mut self.err_buffer);
            tokio::select! {
                read = async { stdout.as_mut().expect("admitted stdout")
                    .read(&mut out_buffer[..out_max]).await }, if out => Some((true, read)),
                read = async { stderr.as_mut().expect("admitted stderr")
                    .read(&mut err_buffer[..err_max]).await }, if err => Some((false, read)),
                _ = tokio::time::sleep(Duration::from_millis(5)) => None,
            }
        };
        match event {
            Some((stdout, Ok(0))) => {
                if stdout {
                    observed.stdout_eof = true;
                } else {
                    observed.stderr_eof = true;
                }
                Ok(())
            }
            Some((stdout, Ok(count))) => {
                let mut buffer = if stdout {
                    std::mem::take(&mut self.out_buffer)
                } else {
                    std::mem::take(&mut self.err_buffer)
                };
                let result = self.accept(stdout, &buffer[..count], observed);
                // No await occurs while either allocation is absent.
                if stdout {
                    std::mem::swap(&mut self.out_buffer, &mut buffer);
                } else {
                    std::mem::swap(&mut self.err_buffer, &mut buffer);
                }
                result
            }
            Some((_, Err(error))) if error.kind() == std::io::ErrorKind::Interrupted => Ok(()),
            Some((stdout, Err(_))) => {
                if stdout {
                    self.out_stopped = true;
                } else {
                    self.err_stopped = true;
                }
                Err(Cause::Io)
            }
            None => Ok(()),
        }
    }
    fn commit(
        &mut self,
        sink: &Sink<'_>,
        owner: &mut Owner,
        observed: &mut Observation,
        end: Instant,
        _faults: &Faults,
    ) -> Result<()> {
        let count = std::mem::take(&mut self.pending);
        let mut offset = 0;
        while offset < count {
            // This is only the already observed live-body prefix, including a
            // bounded prefix preceding an overflow refusal. Never call in drain.
            owner.final_work()?;
            owner
                .cleanup_before(end)
                .inspect_err(|cause| owner.record(*cause))?;
            let finish = count;
            #[cfg(test)]
            let finish = if let Some(after) = _faults.bulk_write_after {
                if observed.bulk_bytes >= after {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
                finish.min(offset + (after - observed.bulk_bytes) as usize)
            } else {
                finish
            };
            let result = sink
                .file
                .write_at(&self.out_buffer[offset..finish], observed.bulk_bytes);
            let written = match result {
                Ok(0) => {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
            };
            self.bulk_hash
                .update(&self.out_buffer[offset..offset + written]);
            offset += written;
            observed.bulk_bytes += written as u64;
            owner.final_work()?;
            owner
                .cleanup_before(end)
                .inspect_err(|cause| owner.record(*cause))?;
        }
        Ok(())
    }
    fn commit_stderr(
        &mut self,
        sink: &Sink<'_>,
        owner: &mut Owner,
        observed: &mut Observation,
        end: Instant,
        _faults: &Faults,
    ) -> Result<()> {
        let count = std::mem::take(&mut self.stderr_pending);
        let mut offset = 0;
        while offset < count {
            owner.final_work()?;
            owner
                .cleanup_before(end)
                .inspect_err(|cause| owner.record(*cause))?;
            let finish = count;
            #[cfg(test)]
            let finish = if let Some(after) = _faults.stderr_write_after {
                if observed.stderr_sink_bytes >= after {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
                finish.min(offset + (after - observed.stderr_sink_bytes) as usize)
            } else {
                finish
            };
            let result = sink
                .file
                .write_at(&self.err_buffer[offset..finish], observed.stderr_sink_bytes);
            let written = match result {
                Ok(0) => {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
                Ok(count) => count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    owner.record(Cause::Io);
                    return Err(Cause::Io);
                }
            };
            self.stderr_sink_hash
                .update(&self.err_buffer[offset..offset + written]);
            offset += written;
            observed.stderr_sink_bytes += written as u64;
            owner.final_work()?;
            owner
                .cleanup_before(end)
                .inspect_err(|cause| owner.record(*cause))?;
        }
        Ok(())
    }
    fn close(&mut self, owner: &mut Owner, observed: &mut Observation, faults: &Faults) {
        if let Some(pipe) = self.stdout.take() {
            observed.stdout_close_attempted = true;
            observed.stdout_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
                && !faults.close(1);
        }
        if let Some(pipe) = self.stderr.take() {
            observed.stderr_close_attempted = true;
            observed.stderr_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
                && !faults.close(2);
        }
        if !observed.stdout_closed || !observed.stderr_closed {
            observed.secondary(owner, Cause::Close);
        }
    }
}
fn hash(value: &Sha256) -> String {
    value
        .clone()
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}
fn check_end(owner: &mut Owner, observed: &mut Observation, end: Instant) {
    if owner.cleanup_before(end).is_err() {
        observed.timed_out = true;
        observed.role_timed_out = true;
        observed.secondary(owner, Cause::Deadline);
    }
}

pub(in crate::release::attestation) async fn run(
    owner: &mut Owner,
    request: Invocation,
) -> Completion {
    run_with(owner, request, Faults::default()).await
}
pub(in crate::release::attestation) async fn run_io(
    owner: &mut Owner,
    request: Invocation,
    io: Io<'_>,
) -> Completion {
    run_io_with(owner, request, Faults::default(), OUT_LIMIT, io).await
}
#[cfg(test)]
pub(in crate::release::attestation) async fn run_stderr_close_fault(
    owner: &mut Owner,
    request: Invocation,
) -> Completion {
    run_with(
        owner,
        request,
        Faults {
            error_close: true,
            ..Faults::default()
        },
    )
    .await
}
async fn run_with(owner: &mut Owner, request: Invocation, faults: Faults) -> Completion {
    run_with_limit(owner, request, faults, OUT_LIMIT).await
}
async fn run_with_limit(
    owner: &mut Owner,
    request: Invocation,
    faults: Faults,
    out_limit: u64,
) -> Completion {
    run_io_with(owner, request, faults, out_limit, Io::default()).await
}
async fn run_io_with(
    owner: &mut Owner,
    request: Invocation,
    faults: Faults,
    capture_limit: u64,
    io: Io<'_>,
) -> Completion {
    let started = owner.now();
    let mut observed = Observation::new();
    let out_limit = io
        .output
        .as_ref()
        .map_or(capture_limit, |sink| sink.maximum);
    let mut streams = Streams::new(out_limit);
    streams.bulk = io.output.is_some();
    streams.capture_bulk = io.capture_output;
    observed.stdin_inherited = io.input.is_some();
    let mut group = None;
    let admission = (|| {
        if request.cleanup {
            owner.final_work()?;
        } else {
            owner.check(Window::Body)?;
        }
        if !request.program.is_absolute() {
            return Err(Cause::Input);
        }
        if request.body_end <= owner.now()
            || request.body_end > started.checked_add(BODY).ok_or(Cause::Input)?
            || request.body_end >= request.settlement_end
            || request.settlement_end
                > request
                    .body_end
                    .checked_add(TERM + REAP)
                    .ok_or(Cause::Input)?
            || request.settlement_end
                > if request.cleanup {
                    owner.end()
                } else {
                    owner.body_end()
                }
            || request.settlement_end > owner.end()
        {
            observed.role_timed_out = true;
            return Err(Cause::Deadline);
        }
        if io.capture_output && (io.output.is_none() || out_limit > OUT_LIMIT) {
            return Err(Cause::Input);
        }
        if let Some(sink) = &io.output {
            let metadata = sink.file.metadata().map_err(|_| Cause::Io)?;
            let flags = rustix::fs::fcntl_getfl(sink.file).map_err(|_| Cause::Io)?;
            let descriptor_flags = rustix::io::fcntl_getfd(sink.file).map_err(|_| Cause::Io)?;
            let mut original = sink.file;
            if original.stream_position().map_err(|_| Cause::Io)? != 0
                || !descriptor_flags.contains(rustix::io::FdFlags::CLOEXEC)
                || sink.maximum == 0
                || sink.maximum > BULK_LIMIT
                || !metadata.is_file()
                || metadata.len() != 0
                || flags.intersection(rustix::fs::OFlags::ACCMODE) == rustix::fs::OFlags::RDONLY
            {
                return Err(Cause::Input);
            }
        }
        if let Some(sink) = &io.stderr {
            let metadata = sink.file.metadata().map_err(|_| Cause::Io)?;
            let flags = rustix::fs::fcntl_getfl(sink.file).map_err(|_| Cause::Io)?;
            let descriptor_flags = rustix::io::fcntl_getfd(sink.file).map_err(|_| Cause::Io)?;
            let mut original = sink.file;
            if original.stream_position().map_err(|_| Cause::Io)? != 0
                || !descriptor_flags.contains(rustix::io::FdFlags::CLOEXEC)
                || sink.maximum != ERR_LIMIT
                || !metadata.is_file()
                || metadata.len() != 0
                || flags.intersection(rustix::fs::OFlags::ACCMODE) == rustix::fs::OFlags::RDONLY
            {
                return Err(Cause::Input);
            }
        }
        if let (Some(output), Some(error)) = (&io.output, &io.stderr) {
            let first = output.file.metadata().map_err(|_| Cause::Io)?;
            let second = error.file.metadata().map_err(|_| Cause::Io)?;
            if first.dev() == second.dev() && first.ino() == second.ino() {
                return Err(Cause::Input);
            }
        }
        let stdin = if let Some(input) = io.input {
            let metadata = input.metadata().map_err(|_| Cause::Io)?;
            let flags = rustix::fs::fcntl_getfl(input).map_err(|_| Cause::Io)?;
            let mut original = input;
            let descriptor_flags = rustix::io::fcntl_getfd(input).map_err(|_| Cause::Io)?;
            if !descriptor_flags.contains(rustix::io::FdFlags::CLOEXEC)
                || !metadata.is_file()
                || flags.intersection(rustix::fs::OFlags::ACCMODE) != rustix::fs::OFlags::RDONLY
                || original.stream_position().map_err(|_| Cause::Io)? != 0
            {
                return Err(Cause::Input);
            }
            let duplicate = input.try_clone().map_err(|_| Cause::Io)?;
            // Command owns this duplicate through spawn/drop. Stable std has no
            // take-stdin API to expose its close status. Original remains owned.
            observed.stdin_duplicate_sdk_owned = true;
            Stdio::from(duplicate)
        } else {
            Stdio::piped()
        };
        let mut command = Command::new(&request.program);
        command
            .args(&request.arguments)
            .env_clear()
            .envs(&request.environment)
            .process_group(0)
            .stdin(stdin)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|_| Cause::Io)?;
        observed.pid = Some(child.id());
        group = Some(Group::new(child));
        drop(command);
        Ok(())
    })();
    if let Err(cause) = admission {
        if cause == Cause::Deadline
            && (owner.now() >= request.body_end
                || owner.now() >= request.settlement_end
                || owner.now() >= owner.end())
        {
            observed.role_timed_out = true;
        }
        owner.record(cause);
    }
    if let Some(group) = &mut group {
        // Transfer every independent pipe before interpreting any close or
        // conversion failure. Default stdin is empty and checked closed;
        // inherited stdin instead retains its explicit SDK-duplicate boundary.
        let input = group.child().stdin.take();
        let stdout = group.child().stdout.take().expect("piped stdout");
        let stderr = group.child().stderr.take().expect("piped stderr");
        if let Some(input) = input {
            let input: OwnedFd = input.into();
            observed.stdin_close_attempted = true;
            observed.stdin_closed = nix::unistd::close(input).is_ok() && !faults.close(0);
            if !observed.stdin_closed {
                owner.record(Cause::Close);
            }
            if io.input.is_some() {
                owner.record(Cause::Input);
            }
        } else if io.input.is_none() {
            owner.record(Cause::Close);
        }
        if faults.conversion() {
            let file: OwnedFd = stdout.into();
            observed.stdout_close_attempted = true;
            observed.stdout_closed = nix::unistd::close(file).is_ok();
            owner.record(Cause::Io);
        } else {
            match ChildStdout::from_std(stdout) {
                Ok(pipe) => streams.stdout = Some(pipe),
                Err(_) => owner.record(Cause::Io),
            }
        }
        match ChildStderr::from_std(stderr) {
            Ok(pipe) => streams.stderr = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        loop {
            let active = if request.cleanup {
                owner.final_work()
            } else {
                owner.check(Window::Body)
            };
            if active.is_err() {
                if owner.now() >= request.body_end || owner.now() >= request.settlement_end {
                    observed.role_timed_out = true;
                }
                break;
            }
            if owner.now() >= request.body_end {
                observed.timed_out = true;
                observed.role_timed_out = true;
                owner.record(Cause::Deadline);
                break;
            }
            match owned_process::exited_now(group.pid()) {
                Ok(true) if streams.complete(&observed) => break,
                Ok(_) => {}
                Err(_) => {
                    owner.record(Cause::Io);
                    break;
                }
            }
            #[cfg(test)]
            if faults.terminal_before_read
                && !matches!(
                    tokio::time::timeout_at(
                        request.body_end.into(),
                        owned_process::exited(group.pid()),
                    )
                    .await,
                    Ok(Ok(()))
                )
            {
                observed.timed_out = true;
                observed.role_timed_out = true;
                owner.record(Cause::Deadline);
                break;
            }
            let read = streams.read_one(&mut observed, &faults).await;
            #[cfg(test)]
            if faults.late_bulk_read && observed.stdout_bytes > 0 {
                owner.observe_at(request.body_end);
            }
            #[cfg(test)]
            if faults.late_stderr_read && observed.stderr_bytes > 0 {
                owner.observe_at(request.body_end);
            }
            let deferred_bound =
                (io.output.is_some() || io.stderr.is_some()) && read == Err(Cause::Bounds);
            if let Err(cause) = read {
                // The overflow is already observed. Retaining its bounded
                // output prefix cannot defer or replace that first refusal.
                owner.record(cause);
                if !deferred_bound {
                    break;
                }
            }
            // A read can complete after its deadline. Only an EOF fenced in
            // this live body can complete semantic stdout. Cleanup drainage
            // still records final EOF, but never changes these body facts.
            let active = if request.cleanup || deferred_bound {
                owner.final_work()
            } else {
                owner.check(Window::Body)
            };
            if active.is_err() || owner.now() >= request.body_end {
                if owner.now() >= request.body_end {
                    observed.timed_out = true;
                    observed.role_timed_out = true;
                    owner.record(Cause::Deadline);
                }
                break;
            }
            if let Some(sink) = &io.output
                && let Err(cause) =
                    streams.commit(sink, owner, &mut observed, request.body_end, &faults)
            {
                owner.record(cause);
                break;
            }
            if let Some(sink) = &io.stderr
                && let Err(cause) =
                    streams.commit_stderr(sink, owner, &mut observed, request.body_end, &faults)
            {
                owner.record(cause);
                break;
            }
            if deferred_bound {
                break;
            }
            observed.stdout_body_eof |= observed.stdout_eof;
            observed.stderr_body_eof |= observed.stderr_eof;
        }
        settle(
            owner,
            group,
            &mut streams,
            &mut observed,
            &faults,
            request.settlement_end,
            io.allow_nonzero,
        )
        .await;
        while !streams.complete(&observed)
            && streams.readable(&observed)
            && owner.now() < request.settlement_end
        {
            let _ = owner.check(Window::Final);
            if let Err(cause) = streams.read_one(&mut observed, &faults).await {
                observed.secondary(owner, cause);
            }
        }
        if !streams.complete(&observed) {
            observed.secondary(owner, Cause::Close);
        }
        streams.close(owner, &mut observed, &faults);
        check_end(owner, &mut observed, request.settlement_end);
    }
    streams.pending = 0;
    streams.stderr_pending = 0;
    observed.bulk_sha256 = hash(&streams.bulk_hash);
    observed.bulk_complete = io.output.is_some()
        && observed.stdout_body_eof
        && !observed.stdout_limited
        && observed.bulk_bytes == observed.stdout_bytes
        && observed.bulk_sha256 == hash(&streams.out_hash);
    observed.stderr_sink_sha256 = hash(&streams.stderr_sink_hash);
    observed.stderr_sink_complete = io.stderr.is_some()
        && observed.stderr_body_eof
        && !observed.stderr_limited
        && observed.stderr_sink_bytes == observed.stderr_bytes
        && observed.stderr_sink_sha256 == hash(&streams.err_hash);
    observed.stdout_sha256 = hash(&streams.out_hash);
    observed.stderr_sha256 = hash(&streams.err_hash);
    observed.timed_out |= owner.error() == Some(Cause::Deadline);
    if request.cleanup {
        let _ = owner.final_work();
    } else {
        let _ = owner.check(Window::Body);
    }
    if owner.error().is_none()
        && (observed.pid.is_none()
            || !observed.group_retired
            || !observed.reaped
            || !(if io.input.is_some() {
                observed.stdin_inherited && observed.stdin_duplicate_sdk_owned
            } else {
                observed.stdin_close_attempted && observed.stdin_closed
            })
            || !observed.stdout_close_attempted
            || !observed.stderr_close_attempted
            || observed.stdout_limited
            || observed.stderr_limited
            || observed.timed_out
            || !observed.cleanup_errors.is_empty()
            || observed.stdout_bytes > out_limit
            || observed.stderr_bytes > ERR_LIMIT
            || !observed.stdout_closed
            || !observed.stderr_closed
            || !observed.stdout_body_eof
            || !observed.stderr_body_eof
            || !observed.stdout_eof
            || !observed.stderr_eof
            || !(if io.allow_nonzero {
                observed.exit_status.is_some()
            } else {
                observed.exit_status == Some(0)
            })
            || io.output.is_some() && !observed.bulk_complete
            || io.stderr.is_some() && !observed.stderr_sink_complete)
    {
        owner.record(Cause::Close);
    }
    // Hash finalization and every observation check are inside the same passed
    // settlement interval, not merely the preceding pipe-close attempt.
    let completed = owner.now();
    observed.elapsed = completed.saturating_duration_since(started);
    if completed >= request.settlement_end {
        observed.timed_out = true;
        observed.role_timed_out = true;
        observed.secondary(owner, Cause::Deadline);
    }
    let result = owner.error().map_or(Ok(()), Err);
    Completion {
        result,
        stdout: streams.captured,
        stderr: streams.error_capture,
        observation: observed,
    }
}
async fn settle(
    owner: &mut Owner,
    group: &mut Group,
    streams: &mut Streams,
    observed: &mut Observation,
    faults: &Faults,
    end: Instant,
    allow_nonzero: bool,
) {
    let pid = group.pid();
    if owner.error().is_some() {
        let terminal = match owned_process::exited_now(pid) {
            Ok(terminal) => terminal,
            Err(_) => {
                observed.secondary(owner, Cause::Io);
                false
            }
        };
        // A terminal leader still reserves the exact group for finish_before.
        // Skip only optional TERM; descendants are always retired before reap.
        if !terminal {
            if owned_process::reserved(pid) {
                match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
                    Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                    Err(_) => observed.secondary(owner, Cause::Io),
                }
            } else {
                observed.secondary(owner, Cause::Close);
            }
            let term_end = end
                .checked_sub(REAP)
                .unwrap_or(end)
                .min(owner.now().checked_add(TERM).unwrap_or(end));
            while owner.now() < term_end {
                let _ = owner.check(Window::Final);
                match owned_process::exited_now(pid) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => {
                        observed.secondary(owner, Cause::Io);
                        break;
                    }
                }
                if let Err(cause) = streams.read_one(observed, faults).await {
                    observed.secondary(owner, cause);
                }
            }
        }
    }
    let reap_end = end.min(owner.now().checked_add(REAP).unwrap_or(end));
    check_end(owner, observed, reap_end);
    // Even after a normal natural EOF, descendants that closed their own pipes
    // must be retired while this caller still owns the unreaped leader identity.
    observed.group_retired = owned_process::finish_before(pid, reap_end).await;
    if !observed.group_retired {
        observed.secondary(owner, Cause::Close);
    }
    let mut terminal = false;
    while owner.now() < reap_end {
        let _ = owner.check(Window::Final);
        match owned_process::exited_now(pid) {
            Ok(true) => {
                terminal = true;
                break;
            }
            Ok(false) => {}
            Err(_) => {
                observed.secondary(owner, Cause::Io);
                break;
            }
        }
        if let Err(cause) = streams.read_one(observed, faults).await {
            observed.secondary(owner, cause);
        }
    }
    if !terminal {
        if owner.now() >= reap_end {
            observed.role_timed_out = true;
        }
        observed.secondary(owner, Cause::Deadline);
        if owned_process::reserved(pid) && group.child().kill().is_err() {
            observed.secondary(owner, Cause::Close);
        }
    }
    // Never cancel/detach this one consuming reap. Blocked kernel wait latency
    // is outside a hard wall guarantee; no group signal follows consumption.
    match group.reap().await {
        Ok(status) => {
            observed.reaped = true;
            observed.exit_status = status
                .code()
                .or_else(|| status.signal().map(|signal| -signal));
            if observed.exit_status != Some(0) && !allow_nonzero {
                owner.record(Cause::Command);
            }
        }
        Err(_) => observed.secondary(owner, Cause::Close),
    }
    check_end(owner, observed, reap_end);
}

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "process_io_tests.rs"]
mod io_tests;

#[cfg(test)]
#[path = "process_restore_io_tests.rs"]
mod restore_io_tests;
