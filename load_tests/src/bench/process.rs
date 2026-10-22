//! Long-lived command boundary for one owned benchmark. Tests can substitute a
//! host, while native ownership uses the same unreaped process-group guard as
//! bounded capacity commands.

#[path = "process_control.rs"]
mod control;

use crate::owned_process::{self, Cancellation, Destination, Limits, Request, Runner};
use anyhow::Result;
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone, Serialize)]
pub struct Exit {
    pub code: Option<i32>,
    pub signal: Option<String>,
}

impl Exit {
    pub fn success(&self) -> bool {
        self.code == Some(0) && self.signal.is_none()
    }
}

pub struct Output {
    pub exit: Exit,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
struct Interrupted(u8);
impl std::fmt::Display for Interrupted {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Benchmark interrupted")
    }
}
impl std::error::Error for Interrupted {}

#[derive(Debug)]
struct ObservedFailure(Exit);
impl std::fmt::Display for ObservedFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Benchmark command failed before retirement completed")
    }
}
impl std::error::Error for ObservedFailure {}

/// Only an observed command outcome supplies a status; diagnostic text and
/// elapsed cleanup time never manufacture a timeout or signal result.
pub(crate) fn failure_status(error: &anyhow::Error) -> Option<u8> {
    if let Some(interrupted) = error.downcast_ref::<Interrupted>() {
        return Some(interrupted.0);
    }
    if let Some(interrupted) = error.downcast_ref::<owned_process::Interrupted>() {
        return Some(interrupted.0);
    }
    if error.is::<owned_process::child::CommandTimedOut>() {
        return Some(124);
    }
    error.downcast_ref::<ObservedFailure>().map(|failure| {
        let exit = &failure.0;
        exit.code.map_or_else(
            || match exit.signal.as_deref() {
                Some("SIGINT") => 130,
                Some("SIGTERM") => 143,
                _ => 1,
            },
            |code| u8::try_from(code).unwrap_or(1),
        )
    })
}

impl From<owned_process::child::Exit> for Exit {
    fn from(exit: owned_process::child::Exit) -> Self {
        Self {
            code: exit.code,
            signal: exit.signal,
        }
    }
}

pub enum Sink {
    Capture,
    File(std::fs::File),
    Stream,
}

pub struct Command {
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    pub directory: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
    pub timeout: Duration,
    pub stdout_limit: u64,
    pub stderr_limit: u64,
    pub input: Vec<u8>,
    pub stdout: Sink,
    pub stderr: Sink,
    /// Reserved for verified owned-resource stop/log/copy/remove operations.
    /// User-controlled command requests cannot establish cleanup ownership.
    pub cleanup: bool,
}

#[async_trait::async_trait]
pub trait Stream: Send {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize>;
}

#[async_trait::async_trait]
impl Stream for owned_process::child::Stdout {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        self.read(buffer).await
    }
}

#[async_trait::async_trait]
pub trait Child: Send {
    fn pid(&self) -> u32;
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>>;
    async fn poll(&mut self) -> Result<Option<Exit>>;
    async fn stop(&mut self, grace: Duration) -> Result<Exit>;
    fn output(&self) -> &[u8];
    fn errors(&self) -> &[u8];
}

#[async_trait::async_trait]
impl Child for owned_process::child::Child {
    fn pid(&self) -> u32 {
        self.pid()
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        self.take_stdout()
            .map(|reader| Box::new(reader) as Box<dyn Stream>)
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        self.poll().await.map(|exit| exit.map(Into::into))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        self.stop(grace).await.map(Into::into)
    }
    fn output(&self) -> &[u8] {
        &self.output
    }
    fn errors(&self) -> &[u8] {
        &self.errors
    }
}

/// The parent endpoint of one inherited standard-input duplex stream. Reads
/// deliver at most the supplied buffer and are cancellation safe. Writes must
/// be awaited to completion; a dropped write is never retried as a new request.
/// The invocation closes this endpoint after retiring its controlled child.
#[async_trait::async_trait]
pub trait Control: Send {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize>;
    async fn write(&mut self, bytes: &[u8]) -> Result<()>;
    fn finish(self: Box<Self>) -> Result<()>;
}

#[async_trait::async_trait]
pub trait Host: Send + Sync {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>>;
    /// Only explicit consumers can request the closed duplex stdin mode.
    /// Existing hosts and byte-input callers retain their previous behavior.
    fn spawn_controlled(&self, _command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        anyhow::bail!("Controlled command mode unavailable")
    }
    async fn cancelled(&self) -> u8;
}

#[derive(Clone, Default)]
pub struct NativeHost {
    cancellation: Cancellation,
}

impl NativeHost {
    pub fn cancel(&self, code: u8) {
        self.cancellation.cancel(code);
    }

    pub fn code(&self) -> u8 {
        self.cancellation.code()
    }
}

/// Run one bounded command without releasing its process-group reservation
/// until its pipes are closed and all owned descendants have been stopped.
/// Every error path still attempts cleanup; only fixed caller errors escape.
pub async fn run(host: &dyn Host, command: Command) -> Result<Output> {
    let cleanup = command.cleanup;
    if !cleanup {
        tokio::select! {
            biased;
            code = host.cancelled() => return Err(Interrupted(code).into()),
            _ = std::future::ready(()) => {},
        }
    }
    let mut child = host.spawn(command)?;
    let observed = loop {
        let poll = if cleanup {
            child.poll().await
        } else {
            tokio::select! {
                result = child.poll() => result,
                code = host.cancelled() => Err(Interrupted(code).into()),
            }
        };
        match poll {
            Ok(Some(exit)) => break Ok(exit),
            Err(error) => break Err(error),
            Ok(None) => {
                if cleanup {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                } else {
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                        code = host.cancelled() => break Err(Interrupted(code).into()),
                    }
                }
            }
        }
    };
    let stopped = child.stop(Duration::from_secs(5)).await;
    let exit = observed?;
    if let Err(error) = stopped {
        if !exit.success() {
            return Err(ObservedFailure(exit).into());
        }
        return Err(error);
    }
    Ok(Output {
        exit,
        stdout: child.output().to_owned(),
        stderr: child.errors().to_owned(),
    })
}

fn destination(sink: Sink) -> Result<Destination> {
    match sink {
        Sink::Capture => Ok(Destination::capture()),
        Sink::File(file) => Ok(Destination::file(file)),
        Sink::Stream => anyhow::bail!("Only stdout may be streamed"),
    }
}

impl NativeHost {
    fn spawn_mode(
        &self,
        command: Command,
        input: Option<std::os::unix::net::UnixStream>,
    ) -> Result<Box<dyn Child>> {
        if !command.cleanup && self.cancellation.code() != 0 {
            if let Some(input) = input {
                let _ = nix::unistd::close(input);
            }
            return Err(Interrupted(self.cancellation.code()).into());
        }
        let stream = matches!(command.stdout, Sink::Stream);
        let stdout = if stream {
            Destination::capture()
        } else {
            match destination(command.stdout) {
                Ok(value) => value,
                Err(error) => {
                    if let Some(input) = input {
                        let _ = nix::unistd::close(input);
                    }
                    return Err(error);
                }
            }
        };
        let stderr = match destination(command.stderr) {
            Ok(value) => value,
            Err(error) => {
                if let Some(input) = input {
                    let _ = nix::unistd::close(input);
                }
                return Err(error);
            }
        };
        let runner = Runner {
            directory: command.directory,
            environment: command.environment,
            cancellation: self.cancellation.clone(),
        };
        let request = Request {
            limits: Limits {
                timeout: command.timeout,
                stdout: command.stdout_limit,
                stderr: command.stderr_limit,
            },
            input: command.input,
            stdout,
            stderr,
            cleanup: command.cleanup,
        };
        let result = if let Some(input) = input {
            runner.spawn_controlled(&command.executable, &command.arguments, request, input)
        } else if stream {
            runner.spawn_stream(&command.executable, &command.arguments, request)
        } else {
            runner.spawn(&command.executable, &command.arguments, request)
        };
        let child = result.map_err(|error| {
            if let Some(interrupted) = error.downcast_ref::<owned_process::Interrupted>() {
                Interrupted(interrupted.0).into()
            } else {
                error
            }
        })?;
        Ok(Box::new(child))
    }
}

#[async_trait::async_trait]
impl Host for NativeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        self.spawn_mode(command, None)
    }
    fn spawn_controlled(&self, command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        anyhow::ensure!(
            command.input.is_empty(),
            "Controlled command requires empty input"
        );
        anyhow::ensure!(
            !matches!(&command.stdout, Sink::Stream),
            "Controlled command requires captured or file stdout"
        );
        if !command.cleanup && self.cancellation.code() != 0 {
            return Err(Interrupted(self.cancellation.code()).into());
        }
        let (parent, child) = std::os::unix::net::UnixStream::pair()?;
        let control = match control::NativeControl::new(parent) {
            Ok(control) => control,
            Err(error) => {
                let _ = nix::unistd::close(child);
                return Err(error);
            }
        };
        match self.spawn_mode(command, Some(child)) {
            Ok(child) => Ok((child, Box::new(control))),
            Err(error) => {
                let _ = Box::new(control).finish();
                Err(error)
            }
        }
    }
    async fn cancelled(&self) -> u8 {
        self.cancellation.cancelled().await
    }
}
