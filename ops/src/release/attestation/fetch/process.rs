//! The two-frame fixed OpenSSH receiver protocol and its reserved child group.
//!
//! Natural receipt/EOF and a current zero exit are separate from forced group
//! retirement. Local settlement never establishes a remote durable outcome.

use super::super::control::{self, Cause, Owner, Result, Window};
use super::super::verify::tool::Environment;
use crate::owned_process::{self, Group};
use std::{
    ffi::OsString,
    io,
    os::unix::process::{CommandExt, ExitStatusExt},
    path::PathBuf,
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::{ChildStdin, ChildStdout},
};

const LINE: usize = 65536;
const REQUEST: usize = 16384;
const TICK: Duration = Duration::from_millis(5);
pub(super) const TAIL: Duration = Duration::from_secs(30);
#[derive(Clone, Default, Debug)]
pub(super) struct Observation {
    pub exit: Option<i32>,
    pub input_closed: bool,
    pub output_closed: bool,
    pub natural_eof: bool,
    pub group_retired: bool,
    pub reaped: bool,
    pub finish_attempted: bool,
    pub reap_attempted: bool,
    pub received: usize,
    pub trailing: usize,
}
pub(super) struct Invocation {
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub environment: Environment,
    pub protocol_end: Instant,
}
pub(super) struct Receiver {
    group: Group,
    input: Option<ChildStdin>,
    output: Option<ChildStdout>,
    buffer: Vec<u8>,
    scratch: Vec<u8>,
    protocol_end: Instant,
    stopped: bool,
    pub observation: Observation,
    #[cfg(test)]
    pub faults: Faults,
}
#[cfg(test)]
#[derive(Default)]
pub(super) struct Faults {
    pub input_close: bool,
    pub output_close: bool,
    pub reap_result: bool,
}
fn guard(owner: &mut Owner, end: Instant) -> Result<()> {
    owner.check(Window::Body)?;
    if owner.now() >= end {
        control::failed(owner, Cause::Deadline)
    } else {
        Ok(())
    }
}
/// ASCII JSON frame: escape Unicode scalar values without re-parsing numbers or
/// replacing any original ASCII lexeme. The input comes from verified JSON.
pub(super) fn frame(json: &[u8]) -> Result<Vec<u8>> {
    let text = std::str::from_utf8(json).map_err(|_| Cause::Policy)?;
    let mut bytes = Vec::new();
    for value in text.trim_end_matches(['\r', '\n']).chars() {
        if value.is_ascii() {
            bytes.push(value as u8);
        } else {
            for unit in value.encode_utf16(&mut [0; 2]).iter() {
                bytes.extend_from_slice(format!("\\u{unit:04x}").as_bytes());
            }
        }
        if bytes.len() >= REQUEST {
            return Err(Cause::Bounds);
        }
    }
    bytes.push(b'\n');
    Ok(bytes)
}
impl Receiver {
    pub async fn start(request: Invocation, owner: &mut Owner) -> Result<Self> {
        guard(owner, request.protocol_end)?;
        if !request.program.is_absolute()
            || request
                .protocol_end
                .checked_add(TAIL)
                .is_none_or(|end| end > owner.body_end())
        {
            return control::failed(owner, Cause::Input);
        }
        let mut command = Command::new(&request.program);
        command
            .args(request.arguments)
            .env_clear()
            .envs(request.environment)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let child = control::io(owner, command.spawn())?;
        let mut group = Group::new(child);
        drop(command);
        let input = group.child().stdin.take().expect("piped input");
        let output = group.child().stdout.take().expect("piped output");
        let mut receiver = Self {
            group,
            input: None,
            output: None,
            buffer: Vec::new(),
            scratch: vec![0; 4096],
            protocol_end: request.protocol_end,
            stopped: false,
            observation: Observation::default(),
            #[cfg(test)]
            faults: Faults::default(),
        };
        // Both independent transfers are attempted. Tokio consumes a failed
        // conversion's standard handle through its own RAII close boundary.
        match ChildStdin::from_std(input) {
            Ok(pipe) => receiver.input = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        match ChildStdout::from_std(output) {
            Ok(pipe) => receiver.output = Some(pipe),
            Err(_) => owner.record(Cause::Io),
        }
        if let Err(cause) = guard(owner, receiver.protocol_end) {
            receiver.finish(owner).await;
            return Err(cause);
        }
        Ok(receiver)
    }
    pub async fn send(&mut self, bytes: &[u8], owner: &mut Owner) -> Result<()> {
        if bytes.is_empty() || bytes.len() > REQUEST || bytes.last() != Some(&b'\n') {
            return control::failed(owner, Cause::Bounds);
        }
        let mut at = 0;
        while at < bytes.len() {
            guard(owner, self.protocol_end)?;
            let event = tokio::select! {
                value = self.input.as_mut().ok_or(Cause::Close)?.write(&bytes[at..]) => Some(value),
                _ = tokio::time::sleep(TICK) => None,
            };
            guard(owner, self.protocol_end)?;
            match event {
                Some(Ok(0)) => return control::failed(owner, Cause::Io),
                Some(Ok(count)) => at += count,
                Some(Err(error)) if error.kind() == io::ErrorKind::Interrupted => {}
                Some(Err(_)) => return control::failed(owner, Cause::Io),
                None => {}
            }
        }
        // ChildStdin is unbuffered. A completed write owns the full frame; no
        // EOF is sent here or after the URL. The peer needs only one LF frame.
        Ok(())
    }
    async fn read(&mut self) -> Result<Option<usize>> {
        let Some(output) = &mut self.output else {
            return Err(Cause::Close);
        };
        match tokio::select! {
            value = output.read(&mut self.scratch) => Some(value),
            _ = tokio::time::sleep(TICK) => None,
        } {
            Some(Ok(count)) => Ok(Some(count)),
            Some(Err(error)) if error.kind() == io::ErrorKind::Interrupted => Ok(None),
            Some(Err(_)) => {
                self.stopped = true;
                Err(Cause::Io)
            }
            None => Ok(None),
        }
    }
    pub async fn receive(&mut self, seconds: u64, owner: &mut Owner) -> Result<Vec<u8>> {
        let end = owner
            .now()
            .checked_add(Duration::from_secs(seconds))
            .ok_or(Cause::Deadline)?
            .min(self.protocol_end);
        loop {
            guard(owner, end)?;
            if let Some(at) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let rest = self.buffer.split_off(at + 1);
                let mut line = std::mem::replace(&mut self.buffer, rest);
                line.pop();
                return Ok(line);
            }
            let event = self.read().await;
            guard(owner, end)?;
            match event {
                Ok(Some(0)) => {
                    self.observation.natural_eof = true;
                    return control::failed(owner, Cause::Policy);
                }
                Ok(Some(count)) => {
                    self.observation.received += count;
                    if self.observation.received > LINE {
                        self.stopped = true;
                        return control::failed(owner, Cause::Bounds);
                    }
                    self.buffer.extend_from_slice(&self.scratch[..count]);
                }
                Ok(None) => {}
                Err(cause) => return control::failed(owner, cause),
            }
        }
    }
    fn close_input(&mut self, owner: &mut Owner) {
        if let Some(pipe) = self.input.take() {
            self.observation.input_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(io::Error::from))
                .is_ok();
            #[cfg(test)]
            {
                self.observation.input_closed &= !self.faults.input_close;
            }
            if !self.observation.input_closed {
                owner.record(Cause::Close);
            }
        }
    }
    /// Attempt every admitted peer, even after protocol/read/close failure.
    /// The returned signed status is private data until the caller binds the
    /// validated receipt and original final owner fences.
    pub async fn finish(&mut self, owner: &mut Owner) -> Option<i32> {
        if self.observation.finish_attempted {
            return self.observation.exit;
        }
        // This capability cannot be resumed by calling finish again after the
        // consuming wait disarms Group, including a failed wait result.
        self.observation.finish_attempted = true;
        self.close_input(owner);
        let body = owner.body_end();
        let exit_end = body
            .checked_sub(Duration::from_secs(15))
            .unwrap_or(body)
            .min(
                owner
                    .now()
                    .checked_add(Duration::from_secs(15))
                    .unwrap_or(body),
            );
        let mut terminal = false;
        while owner.error().is_none() {
            if let Err(cause) = guard(owner, exit_end) {
                owner.record(cause);
                break;
            }
            match owned_process::exited_now(self.group.pid()) {
                Ok(true) => {
                    terminal = true;
                    break;
                }
                Ok(false) => tokio::time::sleep(TICK).await,
                Err(_) => {
                    owner.record(Cause::Io);
                    break;
                }
            }
        }
        let eof_end = body
            .checked_sub(Duration::from_secs(10))
            .unwrap_or(body)
            .min(
                owner
                    .now()
                    .checked_add(Duration::from_secs(5))
                    .unwrap_or(body),
            );
        if terminal {
            while owner.error().is_none() && !self.observation.natural_eof && !self.stopped {
                if let Err(cause) = guard(owner, eof_end) {
                    owner.record(cause);
                    break;
                }
                let event = self.read().await;
                if let Err(cause) = guard(owner, eof_end) {
                    owner.record(cause);
                    break;
                }
                match event {
                    Ok(Some(0)) => self.observation.natural_eof = true,
                    Ok(Some(count)) => {
                        if self
                            .buffer
                            .len()
                            .checked_add(count)
                            .is_none_or(|n| n > LINE)
                        {
                            self.stopped = true;
                            owner.record(Cause::Bounds);
                        } else {
                            self.buffer.extend_from_slice(&self.scratch[..count]);
                        }
                    }
                    Ok(None) => {}
                    Err(cause) => owner.record(cause),
                }
            }
            self.observation.trailing = self.buffer.len();
            if owner.error().is_none()
                && self
                    .buffer
                    .iter()
                    .any(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 11 | 12))
            {
                owner.record(Cause::Policy);
            }
        }
        // TERM is optional for an already terminal leader, but exact-group
        // retirement is mandatory even when descendants closed their pipes.
        let pid = self.group.pid();
        let terminal_now = owned_process::exited_now(pid);
        match terminal_now {
            Ok(false) => {
                let permission =
                    match rustix::process::kill_process_group(pid, rustix::process::Signal::TERM) {
                        Ok(()) | Err(rustix::io::Errno::SRCH) => false,
                        Err(rustix::io::Errno::PERM) => true,
                        Err(_) => {
                            owner.record(Cause::Io);
                            false
                        }
                    };
                let term_end = body
                    .checked_sub(Duration::from_secs(5))
                    .unwrap_or(body)
                    .min(
                        owner
                            .now()
                            .checked_add(Duration::from_secs(5))
                            .unwrap_or(body),
                    );
                while owner.cleanup_before(term_end).is_ok() {
                    match owned_process::exited_now(pid) {
                        Ok(true) => break,
                        Ok(false) => tokio::time::sleep(TICK).await,
                        Err(_) => {
                            owner.record(Cause::Io);
                            break;
                        }
                    }
                }
                // A permission candidate is discharged only by the same final
                // reserved-group proof below. No early failed proof is cleared.
                let _ = permission;
            }
            Ok(true) => {}
            Err(_) => owner.record(Cause::Io),
        }
        self.observation.group_retired = owned_process::finish_before(pid, body).await;
        if !self.observation.group_retired {
            owner.record(Cause::Io);
        }
        self.observation.reap_attempted = true;
        let status = self.group.reap().await;
        #[cfg(test)]
        let status = if self.faults.reap_result {
            // The inert child was actually settled; model an unavailable
            // consuming-wait result after its ownership was disarmed.
            Err(crate::owned_process::Error::Reap)
        } else {
            status
        };
        match status {
            Ok(status) => {
                self.observation.exit = status.code().or_else(|| status.signal().map(|s| -s));
                self.observation.reaped = true;
            }
            Err(_) => owner.record(Cause::Io),
        }
        // Forced EOF is neither observed here nor used to fabricate natural EOF.
        if let Some(pipe) = self.output.take() {
            self.observation.output_closed = pipe
                .into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(io::Error::from))
                .is_ok();
            #[cfg(test)]
            {
                self.observation.output_closed &= !self.faults.output_close;
            }
            if !self.observation.output_closed {
                owner.record(Cause::Close);
            }
        }
        if !self.observation.input_closed
            || !self.observation.output_closed
            || (owner.error().is_none() && (!terminal || !self.observation.natural_eof))
        {
            owner.record(Cause::Close);
        }
        if owner.cleanup_before(body).is_err() {
            owner.record(Cause::Deadline);
        }
        let _ = owner.check(Window::Final);
        self.observation.exit
    }
}
