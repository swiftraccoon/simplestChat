//! Long-lived command boundary for one owned benchmark. Tests can substitute a
//! host, while native ownership uses the same unreaped process-group guard as
//! bounded capacity commands.

use crate::owned_process::{self, Cancellation, Destination, Limits, Request, Runner};
use anyhow::{Result, ensure};
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

#[async_trait::async_trait]
pub trait Host: Send + Sync {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>>;
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
    let mut child = host.spawn(command)?;
    let observed = loop {
        let poll = if cleanup {
            child.poll().await
        } else {
            tokio::select! {
                result = child.poll() => result,
                _ = host.cancelled() => Err(anyhow::anyhow!("Benchmark interrupted")),
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
                        _ = host.cancelled() => break Err(anyhow::anyhow!("Benchmark interrupted")),
                    }
                }
            }
        }
    };
    let stopped = child.stop(Duration::from_secs(5)).await;
    let exit = observed?;
    stopped?;
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

#[async_trait::async_trait]
impl Host for NativeHost {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        ensure!(
            command.cleanup || self.cancellation.code() == 0,
            "Benchmark interrupted"
        );
        let stream = matches!(command.stdout, Sink::Stream);
        let stdout = if stream {
            Destination::capture()
        } else {
            destination(command.stdout)?
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
            stderr: destination(command.stderr)?,
            cleanup: command.cleanup,
        };
        let child = if stream {
            runner.spawn_stream(&command.executable, &command.arguments, request)?
        } else {
            runner.spawn(&command.executable, &command.arguments, request)?
        };
        Ok(Box::new(child))
    }
    async fn cancelled(&self) -> u8 {
        self.cancellation.cancelled().await
    }
}
