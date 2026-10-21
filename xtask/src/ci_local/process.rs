//! Owned, cancellable child commands. No process-global environment changes.

use super::{evidence, io_failure};
use crate::tooling::{self, Environment, Failure, Result};
use std::ffi::OsStr;
use std::io::Write;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};
use tokio::signal::unix::{Signal, SignalKind};

pub(super) struct Signals {
    interrupt: Option<Signal>,
    terminate: Option<Signal>,
    injected: Option<tokio::sync::mpsc::UnboundedReceiver<u8>>,
}

impl Signals {
    pub(super) fn new() -> Result<Self> {
        Ok(Self {
            interrupt: Some(
                tokio::signal::unix::signal(SignalKind::interrupt()).map_err(io_failure)?,
            ),
            terminate: Some(
                tokio::signal::unix::signal(SignalKind::terminate()).map_err(io_failure)?,
            ),
            injected: None,
        })
    }

    #[cfg(test)]
    pub(super) fn injected(receiver: tokio::sync::mpsc::UnboundedReceiver<u8>) -> Self {
        Self {
            interrupt: None,
            terminate: None,
            injected: Some(receiver),
        }
    }

    async fn next(&mut self) -> u8 {
        if let Some(receiver) = &mut self.injected {
            return match receiver.recv().await {
                Some(signal) => signal,
                None => std::future::pending().await,
            };
        }
        tokio::select! {
            _ = self.interrupt.as_mut().expect("registered interrupt").recv() => 130,
            _ = self.terminate.as_mut().expect("registered terminate").recv() => 143,
        }
    }
}

pub(super) struct Runner {
    pub root: PathBuf,
    pub environment: Environment,
    signals: Signals,
}

enum Output {
    Capture,
    Inherit,
    Tee(PathBuf),
}

struct Execution {
    status: u8,
    bytes: Vec<u8>,
    logging: u8,
}

impl Runner {
    pub(super) fn new(root: &Path, environment: Environment, signals: Signals) -> Self {
        Self {
            root: root.into(),
            environment,
            signals,
        }
    }

    /// Observe cancellation that arrived as the last child completed, before
    /// synchronous receipt/lock work can turn that interruption into success.
    pub(super) async fn pending_signal(&mut self) -> Option<u8> {
        tokio::select! {
            biased;
            signal = self.signals.next() => Some(signal),
            _ = tokio::task::yield_now() => None,
        }
    }

    pub(super) async fn capture<I, S>(
        &mut self,
        program: impl AsRef<OsStr>,
        arguments: I,
    ) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .execute(program.as_ref(), arguments, Output::Capture, None, false)
            .await?;
        checked(program.as_ref(), output.status)?;
        Ok(output.bytes)
    }

    pub(super) async fn quiet<I, S>(
        &mut self,
        program: impl AsRef<OsStr>,
        arguments: I,
    ) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .execute(program.as_ref(), arguments, Output::Capture, None, true)
            .await?;
        checked(program.as_ref(), output.status)?;
        Ok(output.bytes)
    }

    pub(super) async fn run<I, S>(&mut self, program: impl AsRef<OsStr>, arguments: I) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .execute(program.as_ref(), arguments, Output::Inherit, None, false)
            .await?;
        checked(program.as_ref(), output.status)
    }

    pub(super) async fn input<I, S>(
        &mut self,
        program: impl AsRef<OsStr>,
        arguments: I,
        bytes: &[u8],
    ) -> Result<()>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .execute(
                program.as_ref(),
                arguments,
                Output::Inherit,
                Some(bytes),
                false,
            )
            .await?;
        checked(program.as_ref(), output.status)
    }

    pub(super) async fn act<I, S>(
        &mut self,
        program: impl AsRef<OsStr>,
        arguments: I,
        log: &Path,
    ) -> Result<(u8, u8)>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self
            .execute(
                program.as_ref(),
                arguments,
                Output::Tee(log.into()),
                None,
                false,
            )
            .await?;
        Ok((output.status, output.logging))
    }

    async fn execute<I, S>(
        &mut self,
        program: &OsStr,
        arguments: I,
        output: Output,
        input: Option<&[u8]>,
        quiet: bool,
    ) -> Result<Execution>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::from(tooling::command(
            &self.root,
            &self.environment,
            program,
            arguments,
        ));
        command.process_group(0).kill_on_drop(true);
        command.stderr(if quiet {
            Stdio::null()
        } else {
            Stdio::inherit()
        });
        let tee = match &output {
            Output::Capture => {
                command.stdout(Stdio::piped());
                None
            }
            Output::Inherit => {
                command.stdout(Stdio::inherit());
                None
            }
            Output::Tee(path) => {
                let log = evidence::new_file(path)?;
                let (reader, writer) = UnixStream::pair().map_err(io_failure)?;
                reader.set_nonblocking(true).map_err(io_failure)?;
                command.stdout(Stdio::from(OwnedFd::from(
                    writer.try_clone().map_err(io_failure)?,
                )));
                command.stderr(Stdio::from(OwnedFd::from(writer)));
                Some((
                    tokio::net::UnixStream::from_std(reader).map_err(io_failure)?,
                    log,
                ))
            }
        };
        if input.is_some() {
            command.stdin(Stdio::piped());
        } else {
            command.stdin(Stdio::null());
        }
        let mut child = command.spawn().map_err(|error| {
            Failure::new(
                match error.kind() {
                    std::io::ErrorKind::NotFound => 127,
                    std::io::ErrorKind::PermissionDenied => 126,
                    _ => 1,
                },
                format!("{}: {error}", program.to_string_lossy()),
            )
        })?;
        let group = child.id();
        // Command retains configured descriptors after spawn; release its socket
        // writers now so the reader observes EOF when the owned child exits.
        drop(command);
        let writer = input.map(|bytes| {
            let bytes = bytes.to_vec();
            let mut stdin = child.stdin.take().expect("piped input");
            tokio::spawn(async move { stdin.write_all(&bytes).await })
        });
        let reader = if let Some((mut reader, mut log)) = tee {
            Some(tokio::spawn(async move {
                let mut failed = false;
                let mut buffer = [0; 65536];
                loop {
                    let count = reader.read(&mut buffer).await?;
                    if count == 0 {
                        break;
                    }
                    if log.write_all(&buffer[..count]).is_err() {
                        failed = true;
                    }
                    if std::io::stdout()
                        .lock()
                        .write_all(&buffer[..count])
                        .is_err()
                    {
                        failed = true;
                    }
                }
                if log.flush().is_err() {
                    failed = true;
                }
                Ok::<_, std::io::Error>((Vec::new(), u8::from(failed)))
            }))
        } else {
            child.stdout.take().map(|stdout| {
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    stdout
                        .take(16 * 1024 * 1024 + 1)
                        .read_to_end(&mut bytes)
                        .await?;
                    if bytes.len() > 16 * 1024 * 1024 {
                        return Err(std::io::Error::other(
                            "Local CI command output exceeded its bound",
                        ));
                    }
                    Ok((bytes, 0))
                })
            })
        };
        let status = tokio::select! {
            result = child.wait() => result.map(tooling::shell_status).map_err(io_failure)?,
            signal = self.signals.next() => {
                stop(&mut child, group, signal).await;
                if let Some(reader) = reader { reader.abort(); }
                if let Some(writer) = writer { writer.abort(); }
                return Err(Failure::new(signal, "Local CI interrupted."));
            }
        };
        if let Some(writer) = writer {
            let written = writer
                .await
                .map_err(|error| Failure::new(1, error.to_string()))?;
            if status == 0 {
                written.map_err(io_failure)?;
            }
        }
        let (bytes, logging) = if let Some(mut reader) = reader {
            let result = match tokio::time::timeout(Duration::from_secs(5), &mut reader).await {
                Ok(result) => result,
                Err(_) => {
                    reader.abort();
                    if let Some(group) = group {
                        signal_group(group, "-KILL").await;
                    }
                    return Err(Failure::new(1, "Local CI child left an open output stream"));
                }
            };
            result
                .map_err(|error| Failure::new(1, error.to_string()))?
                .map_err(io_failure)?
        } else {
            (Vec::new(), 0)
        };
        Ok(Execution {
            status,
            bytes,
            logging,
        })
    }
}

fn checked(program: &OsStr, code: u8) -> Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(Failure::new(
            code,
            format!("{} failed (status {code})", program.to_string_lossy()),
        ))
    }
}

/// Only a process group created for this exact Child can be signalled.
async fn stop(child: &mut Child, group: Option<u32>, signal: u8) {
    if let Some(pid) = group {
        let name = if signal == 130 { "-INT" } else { "-TERM" };
        signal_group(pid, name).await;
        if tokio::time::timeout(Duration::from_secs(2), child.wait())
            .await
            .is_err()
        {
            signal_group(pid, "-KILL").await;
            let _ = child.wait().await;
        }
    }
}

async fn signal_group(pid: u32, name: &str) {
    let _ = Command::new("/bin/kill")
        .args([name, "--", &format!("-{pid}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}
