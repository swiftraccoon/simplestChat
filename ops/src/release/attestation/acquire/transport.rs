//! Fixed curl transport with secret stdin, raw headers and an original FD sink.
//! An API header-stop is separate from normal, complete blob success.

use super::{
    super::{
        control::{self, Cause, Owner, Result, Window},
        files::{Dir, Retained},
    },
    headers,
};
use crate::owned_process::{self, Group};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStderr, ChildStdin, ChildStdout},
};

const ERR_LIMIT: u64 = 65536;
const TERM: Duration = Duration::from_secs(5);
const REAP: Duration = Duration::from_secs(5);
pub(in crate::release::attestation) enum Role {
    Api,
    Blob {
        directory: Dir,
        expected: u64,
        digest: String,
    },
    Receiver {
        directory: Dir,
        expected: u64,
        digest: String,
    },
}
impl Role {
    fn expected(&self) -> Option<u64> {
        match self {
            Self::Api => None,
            Self::Blob { expected, .. } | Self::Receiver { expected, .. } => Some(*expected),
        }
    }
    fn logical(&self) -> u64 {
        if matches!(self, Self::Receiver { .. }) {
            1024 * 1024
        } else {
            65536
        }
    }
}
pub(in crate::release::attestation) struct Invocation {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: BTreeMap<OsString, OsString>,
    pub input: Vec<u8>,
    pub role: Role,
    pub body_end: Instant,
    pub settlement_end: Instant,
    #[cfg(test)]
    pub expire_after: Option<u64>,
}
#[derive(Default, Debug)]
pub(in crate::release::attestation) struct Observation {
    pub exit_status: Option<i32>,
    pub input_complete: bool,
    pub input_closed: bool,
    pub output_closed: bool,
    pub error_closed: bool,
    pub output_eof: bool,
    pub natural_output_eof: bool,
    pub error_eof: bool,
    pub group_retired: bool,
    pub reaped: bool,
    pub planned_header_stop: bool,
    pub body_bytes: u64,
    pub stderr_bytes: u64,
    pub stderr_sha256: String,
    pub receiver_failure: Option<&'static str>,
    #[cfg(test)]
    pub first_io_stage: Option<&'static str>,
    #[cfg(test)]
    pub first_io_errno: Option<i32>,
    #[cfg(test)]
    pub permission_candidate: bool,
}
impl Observation {
    fn io(&mut self, owner: &mut Owner, stage: &'static str, errno: Option<i32>) {
        #[cfg(test)]
        if self.first_io_stage.is_none() {
            self.first_io_stage = Some(stage);
            self.first_io_errno = errno;
        }
        #[cfg(not(test))]
        let _ = (stage, errno);
        owner.record(Cause::Io);
    }
}
pub(in crate::release::attestation) struct Completion {
    pub result: Result<Option<String>>,
    pub file: Option<Retained>,
    pub observation: Observation,
}
#[derive(Default)]
struct Faults {
    #[cfg(test)]
    output_close: bool,
    #[cfg(test)]
    error_close: bool,
    #[cfg(test)]
    interruptions: bool,
    #[cfg(test)]
    terminal_before_stop: bool,
    #[cfg(test)]
    permission_candidate: bool,
    #[cfg(test)]
    retirement_refusal: bool,
    #[cfg(test)]
    late_output: bool,
}
impl Faults {
    fn permission_candidate(&self) -> bool {
        #[cfg(test)]
        {
            self.permission_candidate
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn retirement_refusal(&self) -> bool {
        #[cfg(test)]
        {
            self.retirement_refusal
        }
        #[cfg(not(test))]
        {
            false
        }
    }
    fn close(&self, output: bool) -> bool {
        #[cfg(test)]
        {
            if output {
                self.output_close
            } else {
                self.error_close
            }
        }
        #[cfg(not(test))]
        {
            let _ = output;
            false
        }
    }
}
#[cfg(test)]
#[derive(Clone, Copy)]
pub(in crate::release::attestation) enum ReceiverFault {
    Read,
    Interrupt,
    Deadline,
    Flush,
    Fsync,
}
#[cfg(test)]
thread_local! {
    static RECEIVER_FAULT: std::cell::Cell<Option<ReceiverFault>> = const {
        std::cell::Cell::new(None)
    };
}
#[cfg(test)]
pub(in crate::release::attestation) fn receiver_fault(fault: Option<ReceiverFault>) {
    RECEIVER_FAULT.set(fault);
}
enum Event {
    Input(std::io::Result<usize>),
    Output(std::io::Result<usize>),
    Error(std::io::Result<usize>),
    Tick,
}
struct Streams {
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    error: Option<ChildStderr>,
    input_at: usize,
    #[cfg(test)]
    interruptions: u8,
    #[cfg(test)]
    late_output: bool,
    output_buffer: Vec<u8>,
    error_buffer: Vec<u8>,
    headers: headers::Decoder,
    header_complete: bool,
    location: Option<String>,
    body_hash: Sha256,
    body_buffer: Vec<u8>,
    #[cfg(test)]
    receiver_fault: Option<ReceiverFault>,
    #[cfg(test)]
    expire_after: Option<u64>,
    error_hash: Sha256,
    file: Option<Retained>,
    output_stopped: bool,
    error_stopped: bool,
}
impl Streams {
    fn new() -> Self {
        Self {
            input: None,
            output: None,
            error: None,
            input_at: 0,
            #[cfg(test)]
            interruptions: 0,
            #[cfg(test)]
            late_output: false,
            output_buffer: vec![0; 65536],
            error_buffer: vec![0; 65536],
            headers: headers::Decoder::default(),
            header_complete: false,
            location: None,
            body_hash: Sha256::new(),
            body_buffer: Vec::new(),
            #[cfg(test)]
            receiver_fault: RECEIVER_FAULT.take(),
            #[cfg(test)]
            expire_after: None,
            error_hash: Sha256::new(),
            file: None,
            output_stopped: false,
            error_stopped: false,
        }
    }
    fn close_input(&mut self, owner: &mut Owner, observed: &mut Observation) {
        if let Some(pipe) = self.input.take() {
            observed.input_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok();
            if !observed.input_closed {
                owner.record(Cause::Close);
            }
        }
    }
    fn close(&mut self, owner: &mut Owner, observed: &mut Observation, faults: &Faults) {
        self.close_input(owner, observed);
        if let Some(pipe) = self.output.take() {
            observed.output_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
                && !faults.close(true);
            if !observed.output_closed {
                owner.record(Cause::Close);
            }
        }
        if let Some(pipe) = self.error.take() {
            observed.error_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(std::io::Error::from))
                .is_ok()
                && !faults.close(false);
            if !observed.error_closed {
                owner.record(Cause::Close);
            }
        }
    }
    fn output_bytes(
        &mut self,
        bytes: &[u8],
        role: &Role,
        owner: &mut Owner,
        observed: &mut Observation,
    ) -> Result<()> {
        let body = if self.header_complete {
            bytes
        } else {
            let Some((header, consumed)) = self.headers.push(bytes, owner)? else {
                return Ok(());
            };
            match role {
                Role::Api => {
                    let location = header.location(owner)?;
                    let _ = super::grant::Grant::decode(&location, owner)?;
                    self.location = Some(location);
                }
                Role::Blob {
                    directory,
                    expected,
                    ..
                } => {
                    header.blob(*expected, owner)?;
                    self.file = Some(Retained::create(directory.clone(), "artifact.zip", owner)?);
                }
                Role::Receiver {
                    directory,
                    expected,
                    ..
                } => {
                    if !(200..300).contains(&header.status) {
                        observed.receiver_failure = Some("http_error");
                        return control::failed(owner, Cause::Policy);
                    }
                    self.file = Some(
                        Retained::create(directory.clone(), "artifact.zip", owner).inspect_err(
                            |_| observed.receiver_failure = Some("filesystem_error"),
                        )?,
                    );
                    if let Err(cause) = header.receiver(*expected, owner) {
                        observed.receiver_failure = Some("http_response_rejected");
                        return Err(cause);
                    }
                }
            }
            self.header_complete = true;
            &bytes[consumed..]
        };
        if let Some(expected) = role.expected() {
            let mut remaining = body;
            while !remaining.is_empty() {
                let wanted = expected
                    .saturating_sub(observed.body_bytes)
                    .saturating_add(1)
                    .min(role.logical()) as usize;
                let count = (wanted - self.body_buffer.len()).min(remaining.len());
                self.body_buffer.extend_from_slice(&remaining[..count]);
                remaining = &remaining[count..];
                if self.body_buffer.len() == wanted {
                    self.commit_body(expected, owner, observed)?;
                }
            }
        }
        Ok(())
    }
    fn commit_body(
        &mut self,
        expected: u64,
        owner: &mut Owner,
        observed: &mut Observation,
    ) -> Result<()> {
        if self.body_buffer.is_empty() {
            return Ok(());
        }
        #[cfg(test)]
        if self
            .expire_after
            .is_some_and(|after| observed.body_bytes >= after)
        {
            owner.expire_body();
        }
        owner.check(Window::Body)?;
        let next = observed
            .body_bytes
            .checked_add(self.body_buffer.len() as u64)
            .ok_or(Cause::Bounds)?;
        if next > expected {
            observed.receiver_failure = Some("download_size_mismatch");
            return control::failed(owner, Cause::Bounds);
        }
        self.file
            .as_mut()
            .unwrap()
            .write_chunk(&self.body_buffer, observed.body_bytes, owner)
            .inspect_err(|_| observed.receiver_failure = Some("filesystem_error"))?;
        self.body_hash.update(&self.body_buffer);
        observed.body_bytes = next;
        self.body_buffer.clear();
        Ok(())
    }
    async fn step(
        &mut self,
        request: &Invocation,
        owner: &mut Owner,
        observed: &mut Observation,
        cleanup: bool,
    ) -> Result<()> {
        #[cfg(test)]
        if !cleanup
            && observed.body_bytes >= 1024 * 1024
            && let Some(
                fault @ (ReceiverFault::Read | ReceiverFault::Interrupt | ReceiverFault::Deadline),
            ) = self.receiver_fault
        {
            self.receiver_fault = None;
            self.output_stopped = true;
            return match fault {
                ReceiverFault::Read => control::failed(owner, Cause::Io),
                ReceiverFault::Interrupt => control::failed(owner, Cause::Interrupt),
                ReceiverFault::Deadline => {
                    owner.expire_body();
                    owner.check(Window::Body)
                }
                _ => unreachable!(),
            };
        }
        #[cfg(test)]
        if self.interruptions > 0 {
            self.interruptions -= 1;
            let interrupted = Err(std::io::Error::from(std::io::ErrorKind::Interrupted));
            let event = match self.interruptions {
                2 => Event::Input(interrupted),
                1 => Event::Output(interrupted),
                _ => Event::Error(interrupted),
            };
            return self.event(event, request, owner, observed, cleanup);
        }
        let out = self.output.is_some()
            && !self.output_stopped
            && !observed.output_eof
            && !observed.planned_header_stop;
        let err = self.error.is_some() && !self.error_stopped && !observed.error_eof;
        let input = !cleanup && self.input.is_some();
        let maximum = ERR_LIMIT
            .saturating_sub(observed.stderr_bytes)
            .saturating_add(1)
            .min(self.error_buffer.len() as u64) as usize;
        let event = {
            let (input_pipe, output_pipe, error_pipe) =
                (&mut self.input, &mut self.output, &mut self.error);
            let (output_buffer, error_buffer) = (&mut self.output_buffer, &mut self.error_buffer);
            tokio::select! {
                result = async { input_pipe.as_mut().expect("admitted stdin")
                    .write(&request.input[self.input_at..]).await
                }, if input => Event::Input(result),
                result = async { output_pipe.as_mut().expect("admitted stdout")
                    .read(output_buffer).await }, if out => Event::Output(result),
                result = async { error_pipe.as_mut().expect("admitted stderr")
                    .read(&mut error_buffer[..maximum]).await }, if err => Event::Error(result),
                _ = tokio::time::sleep(Duration::from_millis(5)) => Event::Tick,
            }
        };
        #[cfg(test)]
        if self.late_output
            && self.header_complete
            && matches!(&event,Event::Output(Ok(count)) if *count>0)
        {
            self.late_output = false;
            owner.observe_at(request.body_end + Duration::from_millis(1));
        }
        if !cleanup
            && matches!(
                &event,
                Event::Input(Ok(1..)) | Event::Output(Ok(_)) | Event::Error(Ok(_))
            )
        {
            owner.check(Window::Body)?;
            if owner.now() >= request.body_end {
                return control::failed(owner, Cause::Deadline);
            }
        }
        self.event(event, request, owner, observed, cleanup)
    }
    fn event(
        &mut self,
        event: Event,
        request: &Invocation,
        owner: &mut Owner,
        observed: &mut Observation,
        cleanup: bool,
    ) -> Result<()> {
        match event {
            Event::Input(Err(error)) | Event::Output(Err(error)) | Event::Error(Err(error))
                if error.kind() == std::io::ErrorKind::Interrupted =>
            {
                Ok(())
            }
            Event::Input(Ok(0)) => {
                observed.io(owner, "stdin-zero", None);
                Err(Cause::Io)
            }
            Event::Input(Ok(count)) => {
                self.input_at += count;
                if self.input_at == request.input.len() {
                    observed.input_complete = true;
                    self.close_input(owner, observed);
                }
                Ok(())
            }
            Event::Input(Err(error)) => {
                observed.io(owner, "stdin-write", error.raw_os_error());
                self.close_input(owner, observed);
                Err(Cause::Io)
            }
            Event::Output(Ok(0)) => {
                observed.output_eof = true;
                if !cleanup {
                    observed.natural_output_eof = true;
                    if self.header_complete
                        && let Some(expected) = request.role.expected()
                    {
                        self.commit_body(expected, owner, observed)?;
                        if matches!(request.role, Role::Receiver { .. })
                            && observed.body_bytes != expected
                        {
                            observed.receiver_failure = Some("download_size_mismatch");
                        }
                    }
                }
                Ok(())
            }
            Event::Error(Ok(0)) => {
                observed.error_eof = true;
                Ok(())
            }
            Event::Output(Ok(count)) => {
                if cleanup {
                    return Ok(());
                }
                let buffer = std::mem::take(&mut self.output_buffer);
                let result = self.output_bytes(&buffer[..count], &request.role, owner, observed);
                self.output_buffer = buffer;
                if result.is_err() {
                    self.output_stopped = true;
                }
                result
            }
            Event::Error(Ok(count)) => {
                observed.stderr_bytes += count as u64;
                self.error_hash.update(&self.error_buffer[..count]);
                if observed.stderr_bytes > ERR_LIMIT {
                    self.error_stopped = true;
                    Err(Cause::Bounds)
                } else {
                    Ok(())
                }
            }
            Event::Output(Err(error)) => {
                observed.io(owner, "stdout-read", error.raw_os_error());
                self.output_stopped = true;
                Err(Cause::Io)
            }
            Event::Error(Err(error)) => {
                observed.io(owner, "stderr-read", error.raw_os_error());
                self.error_stopped = true;
                Err(Cause::Io)
            }
            Event::Tick => Ok(()),
        }
    }
}
fn error(owner: &mut Owner, result: Result<()>) {
    if let Err(cause) = result {
        owner.record(cause);
    }
}
fn before(owner: &mut Owner, end: Instant) {
    if owner.cleanup_before(end).is_err() {
        owner.record(Cause::Deadline);
    }
}
pub(in crate::release::attestation) async fn run(
    owner: &mut Owner,
    request: Invocation,
) -> Completion {
    run_with(owner, request, Faults::default()).await
}
async fn run_with(owner: &mut Owner, request: Invocation, faults: Faults) -> Completion {
    let mut streams = Streams::new();
    #[cfg(test)]
    {
        streams.expire_after = request.expire_after;
        streams.interruptions = if faults.interruptions { 3 } else { 0 };
        streams.late_output = faults.late_output;
    }
    let mut observed = Observation::default();
    let mut group = None;
    let admitted = (|| {
        owner.check(Window::Body)?;
        if !request.program.is_absolute() || request.input.is_empty() || request.input.len() > 32768
        {
            return control::failed(owner, Cause::Input);
        }
        if request.body_end <= owner.now()
            || request.body_end >= request.settlement_end
            || request.settlement_end > owner.body_end()
            || request.settlement_end
                > request
                    .body_end
                    .checked_add(TERM + REAP)
                    .ok_or(Cause::Input)?
        {
            return control::failed(owner, Cause::Deadline);
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
        let child = command.spawn().map_err(|error| {
            observed.io(owner, "spawn", error.raw_os_error());
            Cause::Io
        })?;
        group = Some(Group::new(child));
        drop(command);
        Ok(())
    })();
    error(owner, admitted);
    if let Some(group) = &mut group {
        let input = group.child().stdin.take().expect("piped stdin");
        let output = group.child().stdout.take().expect("piped stdout");
        let stderr = group.child().stderr.take().expect("piped stderr");
        match ChildStdin::from_std(input) {
            Ok(pipe) => streams.input = Some(pipe),
            Err(error) => observed.io(owner, "stdin-conversion", error.raw_os_error()),
        }
        match ChildStdout::from_std(output) {
            Ok(pipe) => streams.output = Some(pipe),
            Err(error) => observed.io(owner, "stdout-conversion", error.raw_os_error()),
        }
        match ChildStderr::from_std(stderr) {
            Ok(pipe) => streams.error = Some(pipe),
            Err(error) => observed.io(owner, "stderr-conversion", error.raw_os_error()),
        }
        loop {
            if owner.check(Window::Body).is_err() {
                break;
            }
            if owner.now() >= request.body_end {
                owner.record(Cause::Deadline);
                break;
            }
            // This permission comes only from a parsed actual API header AND
            // the actual completed secret-input writer, never child status/EOF.
            if matches!(request.role, Role::Api)
                && streams.location.is_some()
                && observed.input_complete
                && observed.input_closed
            {
                #[cfg(test)]
                if faults.terminal_before_stop {
                    while matches!(owned_process::exited_now(group.pid()), Ok(false))
                        && owner.check(Window::Body).is_ok()
                        && owner.now() < request.body_end
                    {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                }
                observed.planned_header_stop = true;
                break;
            }
            match owned_process::exited_now(group.pid()) {
                Ok(true) if observed.output_eof && observed.error_eof => break,
                Ok(_) => {}
                Err(_) => {
                    observed.io(owner, "body-nowait", None);
                    break;
                }
            }
            let result = streams.step(&request, owner, &mut observed, false).await;
            error(owner, result);
        }
        streams.close_input(owner, &mut observed);
        let planned = observed.planned_header_stop && owner.error().is_none();
        let permission_candidate = faults.permission_candidate();
        #[cfg(target_os = "macos")]
        let mut permission_candidate = permission_candidate;
        if owner.error().is_some() || planned {
            if owned_process::reserved(group.pid()) {
                match owned_process::exited_now(group.pid()) {
                    Ok(true) => {} // Final group retirement still handles live descendants.
                    Ok(false) => match rustix::process::kill_process_group(
                        group.pid(),
                        rustix::process::Signal::TERM,
                    ) {
                        Ok(()) | Err(rustix::io::Errno::SRCH) => {}
                        #[cfg(target_os = "macos")]
                        Err(rustix::io::Errno::PERM) => {
                            // This can be the terminal-zombie race on Darwin.
                            // Keep it unresolved until the ONE authoritative
                            // reserved-group proof below. No failed early proof
                            // may latch an Io that a later success must clear.
                            permission_candidate = true;
                        }
                        Err(error) => observed.io(owner, "term-signal", Some(error.raw_os_error())),
                    },
                    Err(_) => observed.io(owner, "preterm-nowait", None),
                }
            } else {
                owner.record(Cause::Close);
            }
            let term_end = request
                .settlement_end
                .checked_sub(REAP)
                .unwrap_or(request.settlement_end)
                .min(
                    owner
                        .now()
                        .checked_add(TERM)
                        .unwrap_or(request.settlement_end),
                );
            while owner.now() < term_end {
                let _ = owner.check(Window::Final);
                match owned_process::exited_now(group.pid()) {
                    Ok(true) => break,
                    Ok(false) => {}
                    Err(_) => {
                        observed.io(owner, "term-nowait", None);
                        break;
                    }
                }
                let result = streams.step(&request, owner, &mut observed, true).await;
                error(owner, result);
            }
        }
        let reap_end = request.settlement_end.min(
            owner
                .now()
                .checked_add(REAP)
                .unwrap_or(request.settlement_end),
        );
        before(owner, reap_end);
        #[cfg(test)]
        {
            observed.permission_candidate = permission_candidate;
        }
        let retired = owned_process::finish_before(group.pid(), reap_end).await;
        observed.group_retired = retired && !faults.retirement_refusal();
        if !observed.group_retired {
            if permission_candidate {
                observed.io(owner, "term-permission-final-proof", Some(1));
            }
            owner.record(Cause::Close);
        }
        // One consuming reap; no later signal may refer to this released PID.
        match group.reap().await {
            Ok(status) => {
                observed.reaped = true;
                observed.exit_status = status
                    .code()
                    .or_else(|| status.signal().map(|value| -value));
                if !planned && observed.exit_status != Some(0) {
                    owner.record(Cause::Command);
                }
            }
            Err(_) => owner.record(Cause::Close),
        }
        while (!observed.error_eof || (!planned && !observed.output_eof))
            && owner.now() < request.settlement_end
        {
            let _ = owner.check(Window::Final);
            let result = streams.step(&request, owner, &mut observed, true).await;
            error(owner, result);
            // A refused stream cannot produce a later EOF: its read branch
            // is stopped. Once the independent peer is also EOF/stopped, close
            // both originals now. Missing EOF still refuses positive completion.
            if (observed.error_eof || streams.error_stopped)
                && (planned || observed.output_eof || streams.output_stopped)
            {
                break;
            }
        }
        streams.close(owner, &mut observed, &faults);
        if !observed.input_complete
            || !observed.input_closed
            || !observed.output_closed
            || !observed.error_closed
            || !observed.error_eof
            || (!planned && !observed.output_eof)
        {
            owner.record(Cause::Close);
        }
        before(owner, request.settlement_end);
    }
    if owner.error().is_none() {
        if let Role::Blob {
            expected, digest, ..
        }
        | Role::Receiver {
            expected, digest, ..
        } = &request.role
        {
            let actual = super::super::files::hex(&streams.body_hash.clone().finalize());
            if !streams.header_complete || observed.body_bytes != *expected || actual != *digest {
                if matches!(request.role, Role::Receiver { .. }) {
                    observed.receiver_failure = Some(if observed.body_bytes != *expected {
                        "download_size_mismatch"
                    } else {
                        "zip_checksum_mismatch"
                    });
                }
                owner.record(Cause::Policy);
            } else if let Some(file) = &mut streams.file {
                #[cfg(test)]
                if matches!(
                    streams.receiver_fault,
                    Some(ReceiverFault::Flush | ReceiverFault::Fsync)
                ) {
                    owner.record(Cause::Io);
                }
                let sealed = file.seal(digest, owner);
                if sealed.is_err() && matches!(request.role, Role::Receiver { .. }) {
                    observed.receiver_failure = Some("filesystem_error");
                }
                error(owner, sealed);
            } else {
                owner.record(Cause::Close);
            }
        } else if streams.location.is_none() {
            owner.record(Cause::Policy);
        }
    }
    observed.stderr_sha256 = super::super::files::hex(&streams.error_hash.finalize());
    let _ = owner.check(Window::Body);
    before(owner, request.settlement_end);
    let result = owner.error().map_or_else(|| Ok(streams.location), Err);
    Completion {
        result,
        file: streams.file,
        observation: observed,
    }
}

#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
