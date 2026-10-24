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
    os::{
        fd::OwnedFd,
        unix::process::{CommandExt, ExitStatusExt},
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

pub(super) struct Invocation {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub body_end: Instant,
    pub settlement_end: Instant,
    pub cleanup: bool,
}
pub(super) struct Completion {
    pub result: Result<()>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub observation: Observation,
}
/// Counts and hashes cover the actual observed prefix. A refused stream stops
/// after its one overflow sentinel; only natural EOF proves the complete stream.
pub(super) struct Observation {
    pub pid: Option<u32>,
    pub exit_status: Option<i32>,
    pub stdin_closed: bool,
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
    pub elapsed: Duration,
    pub cleanup_errors: Vec<Cause>,
}
impl Observation {
    fn new() -> Self {
        Self {
            pid: None,
            exit_status: None,
            stdin_closed: false,
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
}
impl Streams {
    fn new(out_limit: u64) -> Self {
        Self {
            out_limit,
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
        if stdout {
            self.captured.extend_from_slice(&bytes[..accepted]);
        } else {
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
        observed.secondary(owner, Cause::Deadline);
    }
}

pub(super) async fn run(owner: &mut Owner, request: Invocation) -> Completion {
    run_with(owner, request, Faults::default()).await
}
#[cfg(test)]
pub(super) async fn run_stderr_close_fault(owner: &mut Owner, request: Invocation) -> Completion {
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
    let started = owner.now();
    let mut observed = Observation::new();
    let mut streams = Streams::new(out_limit);
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
            return Err(Cause::Deadline);
        }
        let mut command = Command::new(&request.program);
        command
            .args(&request.arguments)
            .env_clear()
            .envs(&request.environment)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn().map_err(|_| Cause::Io)?;
        observed.pid = Some(child.id());
        group = Some(Group::new(child));
        drop(command);
        Ok(())
    })();
    if let Err(cause) = admission {
        owner.record(cause);
    }
    if let Some(group) = &mut group {
        // Transfer every independent pipe before interpreting any close or
        // conversion failure. Stdin is empty and explicitly closed, never null.
        let input: OwnedFd = group.child().stdin.take().expect("piped stdin").into();
        let stdout = group.child().stdout.take().expect("piped stdout");
        let stderr = group.child().stderr.take().expect("piped stderr");
        observed.stdin_close_attempted = true;
        observed.stdin_closed = nix::unistd::close(input).is_ok() && !faults.close(0);
        if !observed.stdin_closed {
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
                break;
            }
            if owner.now() >= request.body_end {
                observed.timed_out = true;
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
            if let Err(cause) = streams.read_one(&mut observed, &faults).await {
                owner.record(cause);
                break;
            }
            // A read can complete after its deadline. Only an EOF fenced in
            // this live body can complete semantic stdout. Cleanup drainage
            // still records final EOF, but never changes these body facts.
            let active = if request.cleanup {
                owner.final_work()
            } else {
                owner.check(Window::Body)
            };
            if active.is_err() || owner.now() >= request.body_end {
                if owner.now() >= request.body_end {
                    observed.timed_out = true;
                    owner.record(Cause::Deadline);
                }
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
            || !observed.stdin_close_attempted
            || !observed.stdout_close_attempted
            || !observed.stderr_close_attempted
            || observed.stdout_limited
            || observed.stderr_limited
            || observed.timed_out
            || !observed.cleanup_errors.is_empty()
            || observed.stdout_bytes > out_limit
            || observed.stderr_bytes > ERR_LIMIT
            || !observed.stdin_closed
            || !observed.stdout_closed
            || !observed.stderr_closed
            || !observed.stdout_eof
            || !observed.stderr_eof
            || observed.exit_status != Some(0))
    {
        owner.record(Cause::Close);
    }
    // Hash finalization and every observation check are inside the same passed
    // settlement interval, not merely the preceding pipe-close attempt.
    let completed = owner.now();
    observed.elapsed = completed.saturating_duration_since(started);
    if completed >= request.settlement_end {
        observed.timed_out = true;
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
) {
    let pid = group.pid();
    if owner.error().is_some() {
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
            if observed.exit_status != Some(0) {
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
