//! The one fixed guest exchange, with exclusive child and original pipe ownership.

use super::super::{Error, Result, control::Control, process::Runner};
use super::{Vm, retire};
use serde_json::{Value, json};
use simplestchat_ops::owned_process::{Group, exited_now};
use std::{
    fs::File,
    io::{self, Read, Write},
    os::{fd::OwnedFd, unix::process::ExitStatusExt},
    process::Stdio,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const MAX: usize = 131072;
const FRAME: usize = 16384;
const POLL: Duration = Duration::from_millis(100);

fn frame(value: &Value) -> Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value).map_err(|_| Error::Input)?;
    bytes.push(b'\n');
    if bytes.len() > FRAME {
        Err(Error::Refused)
    } else {
        Ok(bytes)
    }
}
// The frozen receiver bounds incoming/cumulative records at131072; only
// outgoing request frames use16384. Cleanup never decodes a positive record.
fn decoded(bytes: &[u8]) -> Result<Value> {
    if bytes.len() > MAX {
        return Err(Error::Refused);
    }
    let value = super::super::decode(bytes)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(Error::Refused)
    }
}

pub(super) struct Pipe {
    file: Option<File>,
    flags: Option<rustix::fs::OFlags>,
    pub eof: bool,
    pub closed: bool,
    pub bytes: usize,
}
impl Pipe {
    pub fn new(file: File, control: &mut Control) -> Result<Self> {
        let mut pipe = Self {
            file: Some(file),
            flags: None,
            eof: false,
            closed: false,
            bytes: 0,
        };
        let result = (|| {
            let file = pipe.file.as_ref().ok_or(Error::Close)?;
            let flags = rustix::fs::fcntl_getfl(file).map_err(|_| Error::Io)?;
            pipe.flags = Some(flags);
            rustix::fs::fcntl_setfl(file, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| Error::Io)
        })();
        if let Err(error) = result {
            control.record(error);
            if pipe.finish().is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(pipe)
    }
    pub fn read(&mut self, bytes: &mut [u8]) -> Result<Option<usize>> {
        let file = self.file.as_mut().ok_or(Error::Close)?;
        match file.read(bytes) {
            Ok(count) => {
                self.eof |= count == 0;
                self.bytes = self.bytes.checked_add(count).ok_or(Error::Refused)?;
                Ok(Some(count))
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(None)
            }
            Err(_) => Err(Error::Io),
        }
    }
    fn write(&mut self, bytes: &[u8]) -> Result<usize> {
        match self.file.as_mut().ok_or(Error::Close)?.write(bytes) {
            Ok(0) => Err(Error::Io),
            Ok(count) => Ok(count),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                Ok(0)
            }
            Err(_) => Err(Error::Io),
        }
    }
    pub fn finish(&mut self) -> Result<()> {
        let Some(file) = self.file.take() else {
            return if self.closed {
                Ok(())
            } else {
                Err(Error::Close)
            };
        };
        let flags = self
            .flags
            .take()
            .map(|flags| rustix::fs::fcntl_setfl(&file, flags).map_err(|_| Error::Close))
            .unwrap_or(Ok(()));
        let closed = nix::unistd::close(file).map_err(|_| Error::Close);
        self.closed = closed.is_ok();
        flags.and(closed)
    }
}

pub(in super::super) struct Session {
    group: Option<Group>,
    input: Option<Pipe>,
    output: Option<Pipe>,
    error: Option<Pipe>,
    pending: Vec<u8>,
    offset: usize,
    buffer: Vec<u8>,
    original_end: Instant,
    health: Arc<AtomicBool>,
    failure: Arc<Mutex<Option<Error>>>,
    first: Option<Error>,
    status: Option<i32>,
    group_retired: bool,
    reaped: bool,
    closed: bool,
}
impl Session {
    pub async fn start(vm: &Vm, runner: &mut Runner, control: &mut Control) -> Result<Self> {
        if control.settling {
            return Err(Error::Refused);
        }
        let args = vm.ssh(
            runner,
            control,
            &[
                "/usr/bin/sudo".into(),
                "-n".into(),
                "/usr/local/libexec/simplestchat-public/simplestchat-ops".into(),
                "release".into(),
                "maintain-prerequisite".into(),
                "guest".into(),
            ],
        )?;
        let mut command = runner.command("ssh", &args, control)?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        control.check()?;
        let child = command.spawn().map_err(|_| Error::Io)?;
        let mut session = Self {
            group: Some(Group::new(child)),
            input: None,
            output: None,
            error: None,
            pending: Vec::new(),
            offset: 0,
            buffer: Vec::new(),
            original_end: control.end(),
            health: vm.health(),
            failure: vm.failure(),
            first: None,
            status: None,
            group_retired: false,
            reaped: false,
            closed: false,
        };
        let admitted = session.admit(control);
        if let Err(error) = admitted {
            session.first = Some(error);
            control.record(error);
            if session.close(control).await.is_err() {
                control.cleanup_failed = true;
            }
            return Err(error);
        }
        Ok(session)
    }
    fn admit(&mut self, control: &mut Control) -> Result<()> {
        // Remove/register all returned peers before any flag operation can fail.
        let child = self.group.as_mut().ok_or(Error::Close)?.child();
        let input = child.stdin.take().map(|p| File::from(OwnedFd::from(p)));
        let output = child.stdout.take().map(|p| File::from(OwnedFd::from(p)));
        let error = child.stderr.take().map(|p| File::from(OwnedFd::from(p)));
        let mut first = None;
        for (file, slot) in [
            (input, &mut self.input),
            (output, &mut self.output),
            (error, &mut self.error),
        ] {
            match file.map(|file| Pipe::new(file, control)).transpose() {
                Ok(Some(pipe)) => *slot = Some(pipe),
                Ok(None) => {
                    first.get_or_insert(Error::Close);
                    control.record(Error::Close);
                }
                Err(error) => {
                    first.get_or_insert(error);
                    control.record(error);
                }
            }
        }
        if let Some(error) = first {
            return Err(error);
        }
        control.check()
    }
    fn check(&mut self, control: &mut Control, end: Instant) -> Result<()> {
        if let Some(error) = *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
        {
            self.first.get_or_insert(error);
            control.record(error);
        }
        let checked = control.check();
        if !control.settling {
            if let Some(error) = control.first {
                return Err(error);
            }
            if !self.health.load(Ordering::SeqCst) {
                return Err(Error::Refused);
            }
        }
        checked?;
        if Instant::now() >= end.min(self.original_end) {
            Err(Error::Deadline)
        } else {
            Ok(())
        }
    }
    async fn step(&mut self, control: &mut Control, end: Instant) -> Result<()> {
        self.check(control, end)?;
        let mut progress = false;
        if self.offset < self.pending.len() {
            let until = (self.offset + FRAME).min(self.pending.len());
            let count = self
                .input
                .as_mut()
                .ok_or(Error::Close)?
                .write(&self.pending[self.offset..until])?;
            self.offset += count;
            progress |= count > 0;
            self.check(control, end)?;
        }
        let mut bytes = [0_u8; 65536];
        for stdout in [true, false] {
            let pipe = if stdout {
                &mut self.output
            } else {
                &mut self.error
            };
            let pipe = pipe.as_mut().ok_or(Error::Close)?;
            if !pipe.eof {
                let count = pipe.read(&mut bytes)?;
                progress |= count.is_some();
                if pipe.bytes > MAX {
                    return Err(Error::Refused);
                }
                if stdout && let Some(count) = count {
                    self.buffer.extend_from_slice(&bytes[..count]);
                }
                self.check(control, end)?;
            }
        }
        if progress {
            tokio::task::yield_now().await;
        } else {
            tokio::time::sleep(POLL.min(end.saturating_duration_since(Instant::now()))).await;
        }
        self.check(control, end)
    }
    pub async fn send(&mut self, value: &Value, control: &mut Control) -> Result<()> {
        let result = async {
            if control.settling || self.closed || self.offset != self.pending.len() {
                return Err(Error::Refused);
            }
            let active_end = control.body_end();
            self.check(control, active_end)?;
            self.pending = frame(value)?;
            self.offset = 0;
            let end = (Instant::now() + Duration::from_secs(30)).min(control.body_end());
            while self.offset < self.pending.len() {
                self.step(control, end).await?;
            }
            self.pending.clear();
            self.offset = 0;
            self.check(control, end)
        }
        .await;
        if let Err(error) = result {
            self.first.get_or_insert(error);
            control.record(error);
        }
        result
    }
    pub async fn line(&mut self, control: &mut Control, end: Instant) -> Result<Value> {
        let result = async {
            if control.settling {
                return Err(Error::Refused);
            }
            self.check(control, end)?;
            loop {
                if let Some(newline) = self.buffer.iter().position(|b| *b == b'\n') {
                    let tail = self.buffer.split_off(newline + 1);
                    let raw = std::mem::replace(&mut self.buffer, tail);
                    let value = decoded(&raw[..newline])?;
                    self.check(control, end)?;
                    return Ok(value);
                }
                if self.output.as_ref().is_some_and(|pipe| pipe.eof) {
                    return Err(Error::Refused);
                }
                self.step(control, end).await?;
            }
        }
        .await;
        if let Err(error) = result {
            self.first.get_or_insert(error);
            control.record(error);
        }
        result
    }
    pub async fn finish(&mut self, control: &mut Control) -> Result<i32> {
        let result = async {
            if self.offset != self.pending.len() {
                return Err(Error::Refused);
            }
            if let Some(pipe) = &mut self.input {
                pipe.finish()?;
            }
            let end = (Instant::now() + Duration::from_secs(20)).min(control.body_end());
            loop {
                self.check(control, end)?;
                let terminal = self
                    .group
                    .as_ref()
                    .map(|g| exited_now(g.pid()))
                    .transpose()
                    .map_err(|_| Error::Close)?
                    .unwrap_or(false);
                let eof = self.output.as_ref().is_some_and(|p| p.eof)
                    && self.error.as_ref().is_some_and(|p| p.eof);
                self.check(control, end)?;
                if terminal && eof {
                    break;
                }
                self.step(control, end).await?;
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.first.get_or_insert(error);
            control.record(error);
        }
        let closed = self.close(control).await;
        result?;
        closed?;
        if !self.buffer.is_empty() || self.error.as_ref().is_none_or(|p| p.bytes != 0) {
            return Err(Error::Refused);
        }
        self.status.ok_or(Error::Close)
    }
    pub async fn close(&mut self, control: &mut Control) -> Result<()> {
        if !self.closed {
            self.closed = true;
            if let Some(group) = &mut self.group {
                let (retired, reaped) =
                    retire(group, self.original_end, Duration::from_secs(1)).await;
                self.group_retired = retired;
                if !retired {
                    self.first.get_or_insert(Error::Close);
                }
                match reaped {
                    Ok(status) => {
                        self.reaped = true;
                        let raw = status.code().or_else(|| status.signal().map(|n| -n));
                        self.status = raw;
                        if let Some(status) = raw.filter(|n| *n != 0) {
                            self.first.get_or_insert(Error::Child(status));
                        } else if raw.is_none() {
                            self.first.get_or_insert(Error::Close);
                        }
                    }
                    Err(error) => {
                        self.first.get_or_insert(error);
                    }
                }
            }
            // After the exact group is retired/reaped, attempt remaining natural
            // pipe bytes/EOF once before checked closes. This cannot publish a frame.
            let mut bytes = [0_u8; 65536];
            for pipe in [&mut self.output, &mut self.error].into_iter().flatten() {
                if !pipe.eof {
                    if let Err(error) = pipe.read(&mut bytes) {
                        self.first.get_or_insert(error);
                    }
                    if pipe.bytes > MAX {
                        self.first.get_or_insert(Error::Refused);
                    }
                }
            }
            for pipe in [&mut self.input, &mut self.output, &mut self.error]
                .into_iter()
                .flatten()
            {
                if let Err(error) = pipe.finish() {
                    self.first.get_or_insert(error);
                }
            }
            if let Err(error) = control.check() {
                self.first.get_or_insert(error);
            }
            if Instant::now() >= self.original_end {
                self.first.get_or_insert(Error::Deadline);
            }
        }
        if let Some(error) = self.first {
            control.record(error);
            Err(error)
        } else {
            Ok(())
        }
    }
    pub fn diagnostics(&self) -> Value {
        json!({"rawStatus":self.status,"groupRetired":self.group_retired,"reaped":self.reaped,
            "stdinClosed":self.input.as_ref().is_some_and(|p|p.closed),
            "stdoutClosed":self.output.as_ref().is_some_and(|p|p.closed),
            "stderrClosed":self.error.as_ref().is_some_and(|p|p.closed),
            "stdoutEof":self.output.as_ref().is_some_and(|p|p.eof),
            "stderrEof":self.error.as_ref().is_some_and(|p|p.eof)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn earlier_actor_io_is_imported_before_an_expired_local_phase() {
        let mut control = Control::new(Instant::now()).unwrap();
        let end = control.start;
        let mut session = Session {
            group: None,
            input: None,
            output: None,
            error: None,
            pending: Vec::new(),
            offset: 0,
            buffer: Vec::new(),
            original_end: control.end(),
            health: Arc::new(AtomicBool::new(false)),
            failure: Arc::new(Mutex::new(Some(Error::Io))),
            first: None,
            status: None,
            group_retired: false,
            reaped: false,
            closed: false,
        };
        let observed = session.check(&mut control, end);
        let primary = control.first;
        control.settle();
        let finished = control.finish();
        assert_eq!(observed, Err(Error::Io));
        assert_eq!(primary, Some(Error::Io));
        assert_eq!(finished, Err(Error::Io));
        assert!(session.input.is_none() && session.output.is_none() && session.error.is_none());
    }
    #[test]
    fn fixed_frame_and_total_record_bounds_are_independent() {
        let small = json!({"value":""});
        let overhead = frame(&small).unwrap().len();
        assert_eq!(
            frame(&json!({"value":"x".repeat(FRAME-overhead)}))
                .unwrap()
                .len(),
            FRAME
        );
        assert_eq!(
            frame(&json!({"value":"x".repeat(FRAME-overhead+1)})),
            Err(Error::Refused)
        );
        assert!(decoded(br#"{"ready":true}"#).is_ok());
        let inbound = serde_json::to_vec(&json!({"opaque":"x".repeat(FRAME)})).unwrap();
        assert!(inbound.len() > FRAME && decoded(&inbound).is_ok());
        assert!(decoded(br#"{"ready":false,"ready":true}"#).is_err());
        assert_eq!(decoded(b"[]"), Err(Error::Refused));
        assert_eq!(decoded(&vec![b' '; MAX + 1]), Err(Error::Refused));
    }
    #[test]
    fn admitted_pipe_restores_shared_flags_and_closes_before_assertions() {
        let mut control = Control::new(Instant::now()).unwrap();
        let mut original = None;
        let mut alias = None;
        let mut pipe = None;
        let body: Result<(rustix::fs::OFlags, rustix::fs::OFlags)> = (|| {
            original = Some(
                File::options()
                    .read(true)
                    .write(true)
                    .open("/dev/null")
                    .map_err(|_| Error::Io)?,
            );
            let source = original.as_ref().ok_or(Error::Close)?;
            alias = Some(source.try_clone().map_err(|_| Error::Io)?);
            let before = rustix::fs::fcntl_getfl(alias.as_ref().ok_or(Error::Close)?)
                .map_err(|_| Error::Io)?;
            pipe = Some(Pipe::new(
                original.take().ok_or(Error::Close)?,
                &mut control,
            )?);
            pipe.as_mut().ok_or(Error::Close)?.finish()?;
            let after = rustix::fs::fcntl_getfl(alias.as_ref().ok_or(Error::Close)?)
                .map_err(|_| Error::Io)?;
            Ok((before, after))
        })();
        let closed = pipe.as_ref().is_some_and(|p| p.closed);
        let second = pipe.as_mut().map(Pipe::finish);
        let mut cleanup = Ok(());
        for file in [original.take(), alias.take()].into_iter().flatten() {
            let result = nix::unistd::close(file).map_err(|_| Error::Close);
            cleanup = cleanup.and(result);
        }
        if let Err(error) = body {
            control.record(error);
        }
        control.settle();
        let finished = control.finish();
        let (before, after) = body.unwrap();
        assert_eq!(before, after);
        assert!(closed);
        assert_eq!(second, Some(Ok(())));
        assert_eq!(cleanup, Ok(()));
        assert_eq!(finished, Ok(()));
    }
}
