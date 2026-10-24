//! Paired preparation owns the lease until Ansible's group and streams settle.
//! The existing Native registration observes signals; cleanup futures are joined
//! even after an error. A pipe prefix is liveness data, never final completion.

use super::{Executor, Finalize, Finalizer, Native, OwnedIo, executable, owner};
use crate::{
    ci::{
        command::{Command, Program},
        ops_artifact::capacity_vps::{prepare::PrepareInvocation, tools::Invocation},
    },
    owned_process::{Group, exited, exited_now},
    tooling::{self, Failure, Result},
};
use rustix::process::Signal;
use std::{
    fs::{File, Metadata},
    future::Future,
    io::Read,
    os::{
        fd::OwnedFd,
        unix::{
            fs::MetadataExt,
            process::{CommandExt, ExitStatusExt},
        },
    },
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::net::unix::pipe::Sender;

#[cfg(test)]
#[path = "vps_prepare_tests.rs"]
mod tests;

const TAIL: Duration = Duration::from_secs(30);
const RELEASE: Duration = Duration::from_secs(30);
const FRAME: u64 = 576 * 1024;

fn failure(code: u8) -> Failure {
    Failure::new(code, "Private VPS preparation is unproven.")
}
fn keep(primary: &mut Option<Failure>, result: Result<()>) {
    if let Err(error) = result
        && primary.is_none()
    {
        *primary = Some(error);
    }
}
fn signed(status: std::process::ExitStatus) -> Option<i32> {
    status
        .code()
        .or_else(|| status.signal().map(|signal| -signal))
}
struct Role {
    command: Command,
    proof: Option<Arc<Invocation>>,
    io: Option<OwnedIo>,
    extra: Option<File>,
    group: Option<Group>,
    group_done: bool,
    group_clean: bool,
    status: Option<i32>,
    io_done: bool,
    io_good: bool,
}
impl Role {
    fn take(mut command: Command) -> (Self, Result<()>) {
        let proof = command.ops_vps.clone();
        let admitted = proof
            .as_ref()
            .ok_or_else(|| failure(1))
            .and_then(|proof| proof.begin(&command));
        let (io, io_admission) = OwnedIo::take(&mut command);
        let (extra, extra_admission) =
            owner(command.stdout.take(), "VPS output must have one owner.");
        (
            Self {
                command,
                proof,
                io: Some(io),
                extra,
                group: None,
                group_done: false,
                group_clean: false,
                status: None,
                io_done: false,
                io_good: false,
            },
            admitted.and(io_admission).and(extra_admission),
        )
    }
    fn logs(&self) -> Result<&crate::ci::command::SeparateLogs> {
        self.io
            .as_ref()
            .and_then(|io| io.separate.as_ref())
            .ok_or_else(|| failure(1))
    }
    fn failed(&self) -> bool {
        match self.logs() {
            Ok(logs) => logs.stdout.failed() || logs.stderr.failed(),
            Err(_) => true,
        }
    }
    fn progress(&self) -> Result<(u64, String)> {
        self.logs()?.stdout.progress()
    }
    fn held(&self) -> Result<()> {
        let group = self.group.as_ref().ok_or_else(|| failure(1))?;
        if self.failed()
            || exited_now(group.pid())?
            || self.logs()?.stdout.drainer_finished()
            || self.logs()?.stderr.drainer_finished()
        {
            return Err(failure(1));
        }
        Ok(())
    }
    fn prefix(&self, expected: Option<&(u64, String)>) -> Result<()> {
        if self.failed() {
            return Err(failure(1));
        }
        if let Some(expected) = expected
            && &self.progress()? != expected
        {
            return Err(failure(1));
        }
        Ok(())
    }
    fn spawn(&mut self, native: &mut Native<'_>, interactive: bool) -> Result<()> {
        let proof = self.proof.as_ref().ok_or_else(|| failure(1))?;
        let (root, environment) = proof.context(self.command.program, &self.command.arguments)?;
        let logs = self.logs()?;
        let mut command = tooling::command(
            root,
            environment,
            executable(&self.command)?,
            &self.command.arguments,
        );
        command
            .process_group(0)
            .stdin(if interactive {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::from(logs.stdout.writer()?))
            .stderr(Stdio::from(logs.stderr.writer()?));
        proof.validate(self.command.program, &self.command.arguments)?;
        let child = command.spawn().map_err(|_| failure(1))?;
        self.group = Some(Group::new(child));
        native.cleanup_passed = false;
        drop(command);
        let logs = self
            .io
            .as_mut()
            .and_then(|io| io.separate.as_mut())
            .ok_or_else(|| failure(1))?;
        let output = logs.stdout.release_writer();
        let error = logs.stderr.release_writer();
        output.and(error)
    }
    fn sender(&mut self) -> Result<Sender> {
        let input = self
            .group
            .as_mut()
            .and_then(|group| group.child().stdin.take())
            .ok_or_else(|| failure(1))?;
        let file = File::from(OwnedFd::from(input));
        let admitted = (|| {
            let flags = rustix::fs::fcntl_getfl(&file).map_err(|_| failure(1))?;
            rustix::fs::fcntl_setfl(&file, flags | rustix::fs::OFlags::NONBLOCK)
                .map_err(|_| failure(1))?;
            rustix::io::fcntl_setfd(&file, rustix::io::FdFlags::CLOEXEC).map_err(|_| failure(1))
        })();
        if let Err(error) = admitted {
            let _ = nix::unistd::close(file);
            return Err(error);
        }
        // Conversion failure consumes the FD through Tokio's fallback Drop;
        // it remains negative and is not a checked-close claim.
        Sender::from_file(file).map_err(|_| failure(1))
    }
    async fn finish_group(&mut self, terminate: bool, signal: Signal) -> Result<()> {
        if !self.group_done {
            self.group_done = true;
            let mut stdin_closed = Ok(());
            if let Some(group) = &mut self.group {
                if let Some(input) = group.child().stdin.take() {
                    stdin_closed = nix::unistd::close(OwnedFd::from(input)).map_err(|_| failure(1));
                }
                if terminate {
                    let _ = rustix::process::kill_process_group(group.pid(), signal);
                    let _ = tokio::time::timeout(self.command.grace, exited(group.pid())).await;
                }
                let (cleaned, reaped) = Finalize.finish(group).await;
                self.group_clean = cleaned && reaped.is_ok() && stdin_closed.is_ok();
                self.status = reaped.ok().and_then(signed);
                self.group.take();
            } else {
                self.group_clean = true;
            }
            if let Some(proof) = &self.proof {
                proof.observed_exit(self.status, self.group_clean);
            }
            stdin_closed?;
        }
        if self.group_clean && self.status == Some(0) {
            Ok(())
        } else {
            Err(failure(1))
        }
    }
    async fn finish_io(&mut self) -> Result<()> {
        if !self.io_done {
            self.io_done = true;
            let closed = match self.io.take() {
                Some(io) => io.finish().await,
                None => Err(failure(1)),
            };
            if let Some(proof) = &self.proof {
                proof.observed_io(closed.as_ref().ok().cloned().flatten());
            }
            let extra = self
                .extra
                .take()
                .map(|file| nix::unistd::close(file).map_err(|_| failure(1)))
                .transpose();
            self.io_good = closed.is_ok() && extra.is_ok();
            closed?;
            extra?;
        }
        if self.io_good {
            Ok(())
        } else {
            Err(failure(1))
        }
    }
}
async fn observe(
    native: &mut Native<'_>,
    invocation: &PrepareInvocation,
    end: Instant,
) -> Result<()> {
    native.ready().await?;
    invocation.check()?;
    if Instant::now() >= end {
        Err(failure(124))
    } else {
        Ok(())
    }
}
async fn watched<T>(
    native: &mut Native<'_>,
    invocation: &PrepareInvocation,
    lease: &Role,
    prefix: Option<&(u64, String)>,
    held: bool,
    end: Instant,
    future: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::pin!(future);
    loop {
        observe(native, invocation, end).await?;
        lease.prefix(prefix)?;
        if held {
            lease.held()?;
        }
        tokio::select! {
            biased;
            code = native.signals.next() => {
                let original=*native.interrupted.get_or_insert(code);
                return Err(failure(original));
            },
            result = &mut future => {
                let value=result?;
                observe(native,invocation,end).await?;
                lease.prefix(prefix)?;
                if held { lease.held()?; }
                return Ok(value);
            },
            _ = tokio::time::sleep(Duration::from_millis(10)) => {},
        }
    }
}
/// Observe a finalizer without cancelling or dropping it when the lease fails.
async fn settled<T>(
    native: &mut Native<'_>,
    invocation: &PrepareInvocation,
    lease: &Role,
    prefix: Option<&(u64, String)>,
    end: Instant,
    primary: &mut Option<Failure>,
    future: impl Future<Output = T>,
) -> T {
    tokio::pin!(future);
    loop {
        keep(primary, observe(native, invocation, end).await);
        keep(primary, lease.prefix(prefix));
        keep(primary, lease.held());
        tokio::select! {
            biased;
            result=&mut future=>return result,
            code=native.signals.next()=>{
                let original=*native.interrupted.get_or_insert(code);
                keep(primary,Err(failure(original)));
            },
            _=tokio::time::sleep(Duration::from_millis(10))=>{},
        }
    }
}
async fn send(sender: &Sender, bytes: &[u8]) -> Result<()> {
    let mut offset = 0;
    while offset < bytes.len() {
        sender.writable().await.map_err(|_| failure(1))?;
        match sender.try_write(&bytes[offset..]) {
            Ok(0) => return Err(failure(1)),
            Ok(size) => offset += size,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(_) => return Err(failure(1)),
        }
    }
    Ok(())
}
fn same(a: &Metadata, b: &Metadata) -> bool {
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
async fn frame(sender: &Sender, input: &File) -> Result<()> {
    let before = input.metadata().map_err(|_| failure(1))?;
    if !before.is_file() || before.len() == 0 || before.len() > FRAME {
        return Err(failure(1));
    }
    let mut source = input;
    let mut count = 0u64;
    let mut bytes = vec![0; 65536];
    loop {
        if !same(&before, &input.metadata().map_err(|_| failure(1))?) {
            return Err(failure(1));
        }
        let size = source.read(&mut bytes).map_err(|_| failure(1))?;
        if !same(&before, &input.metadata().map_err(|_| failure(1))?) {
            return Err(failure(1));
        }
        if size == 0 {
            break;
        }
        count = count.checked_add(size as u64).ok_or_else(|| failure(1))?;
        if count > before.len() {
            return Err(failure(1));
        }
        send(sender, &bytes[..size]).await?;
    }
    if count == before.len() {
        Ok(())
    } else {
        Err(failure(1))
    }
}
fn close_sender(sender: Sender) -> Result<()> {
    sender
        .into_nonblocking_fd()
        .map_err(|_| failure(1))
        .and_then(|fd| nix::unistd::close(fd).map_err(|_| failure(1)))
}

pub(super) async fn refuse(mut invocation: PrepareInvocation) -> Result<()> {
    if let Ok((lease, ansible)) = invocation.take_commands() {
        let (mut lease, _) = Role::take(lease);
        let (mut ansible, _) = Role::take(ansible);
        let _ = lease.finish_group(false, Signal::TERM).await;
        let _ = ansible.finish_group(false, Signal::TERM).await;
        let _ = tokio::join!(lease.finish_io(), ansible.finish_io());
    }
    let _ = invocation.finish();
    Err(failure(1))
}

pub(super) async fn run(
    native: &mut Native<'_>,
    mut invocation: PrepareInvocation,
    end: Instant,
) -> Result<()> {
    let entered = Instant::now();
    let (lease, ansible) = match invocation.take_commands() {
        Ok(value) => value,
        Err(error) => {
            let _ = invocation.finish();
            return Err(error);
        }
    };
    let (mut lease, lease_admission) = Role::take(lease);
    let (mut ansible, ansible_admission) = Role::take(ansible);
    let prior = native.cleanup_passed;
    let mut sender = None;
    let mut prefix = None;
    let mut primary = None;
    let lease_window = entered
        .checked_add(lease.command.timeout)
        .and_then(|selected| end.checked_sub(TAIL).map(|limit| selected.min(limit)))
        .ok_or_else(|| failure(124));
    let lease_end = lease_window.as_ref().ok().copied().unwrap_or(entered);
    let body: Result<()> = async {
        lease_admission.and(ansible_admission)?;
        lease_window?;
        if lease.command.program != Program::OpsVpsSsh
            || ansible.command.program != Program::OpsVpsAnsible
            || lease.command.grace > Duration::from_secs(5)
            || ansible.command.grace > Duration::from_secs(5)
            || ansible.command.timeout > Duration::from_secs(1500)
        {
            return Err(failure(1));
        }
        let tail = end
            .checked_sub(TAIL + RELEASE + Duration::from_secs(20))
            .ok_or_else(|| failure(124))?
            .min(
                lease_end
                    .checked_sub(RELEASE + Duration::from_secs(20))
                    .ok_or_else(|| failure(124))?,
            );
        observe(native, &invocation, tail).await?;
        if tail.saturating_duration_since(Instant::now())
            < ansible.command.timeout + Duration::from_secs(120)
        {
            return Err(failure(124));
        }
        lease.spawn(native, true)?;
        sender = Some(lease.sender()?);
        let source = lease
            .io
            .as_ref()
            .and_then(|io| io.input.as_ref())
            .ok_or_else(|| failure(1))?;
        watched(
            native,
            &invocation,
            &lease,
            None,
            true,
            (entered + Duration::from_secs(60)).min(tail),
            frame(sender.as_ref().unwrap(), source),
        )
        .await?;
        let ready = async {
            loop {
                let progress = lease.progress()?;
                if let Some(bytes) = invocation.ready_bytes((progress.0, &progress.1))? {
                    invocation.ready(&bytes)?;
                    return Ok(progress);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        };
        prefix = Some(
            watched(
                native,
                &invocation,
                &lease,
                None,
                true,
                (entered + Duration::from_secs(120)).min(tail),
                ready,
            )
            .await?,
        );
        observe(native, &invocation, tail).await?;
        lease.prefix(prefix.as_ref())?;
        lease.held()?;
        if tail.saturating_duration_since(Instant::now()) < ansible.command.timeout {
            return Err(failure(124));
        }
        ansible.spawn(native, false)?;
        let pid = ansible.group.as_ref().unwrap().pid();
        let ansible_end = (Instant::now() + ansible.command.timeout).min(tail);
        watched(
            native,
            &invocation,
            &lease,
            prefix.as_ref(),
            true,
            ansible_end,
            async {
                loop {
                    if ansible.failed() {
                        return Err(failure(1));
                    }
                    tokio::select! {
                        result=exited(pid)=>return result,
                        _=tokio::time::sleep(Duration::from_millis(10))=>{},
                    }
                }
            },
        )
        .await
    }
    .await;
    keep(&mut primary, body);
    let signal = if native.interrupted == Some(130) {
        Signal::INT
    } else {
        Signal::TERM
    };
    let terminate = primary.is_some();
    let result = settled(
        native,
        &invocation,
        &lease,
        prefix.as_ref(),
        end,
        &mut primary,
        ansible.finish_group(terminate, signal),
    )
    .await;
    keep(&mut primary, result);
    let result = settled(
        native,
        &invocation,
        &lease,
        prefix.as_ref(),
        end,
        &mut primary,
        ansible.finish_io(),
    )
    .await;
    keep(&mut primary, result);
    // No release or lease termination precedes Ansible group/stream settlement.
    if let Some(input) = sender.take() {
        if prefix.is_some() {
            let deadline = (Instant::now() + RELEASE).min(lease_end);
            let sent = if Instant::now() < deadline {
                match tokio::time::timeout_at(deadline.into(), send(&input, b"release\n")).await {
                    Ok(result) => result,
                    Err(_) => Err(failure(124)),
                }
            } else {
                Err(failure(124))
            };
            keep(&mut primary, sent);
        }
        keep(&mut primary, close_sender(input));
    }
    if primary.is_none() {
        let pid = lease
            .group
            .as_ref()
            .map(Group::pid)
            .ok_or_else(|| failure(1));
        let waited = match pid {
            Ok(pid) => {
                watched(
                    native,
                    &invocation,
                    &lease,
                    prefix.as_ref(),
                    false,
                    lease_end,
                    exited(pid),
                )
                .await
            }
            Err(error) => Err(error),
        };
        keep(&mut primary, waited);
    }
    let terminate = primary.is_some();
    let signal = if native.interrupted == Some(130) {
        Signal::INT
    } else {
        Signal::TERM
    };
    keep(&mut primary, lease.finish_group(terminate, signal).await);
    keep(&mut primary, lease.finish_io().await);
    native.cleanup_passed = prior && lease.group_clean && ansible.group_clean;
    keep(&mut primary, invocation.finish());
    keep(&mut primary, observe(native, &invocation, end).await);
    match primary {
        Some(error) => Err(error),
        None => Ok(()),
    }
}
