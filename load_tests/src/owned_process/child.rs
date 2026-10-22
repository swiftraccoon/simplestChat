//! Long-lived owned commands. Polling never reaps the leader, so the process
//! group identity remains reserved until explicit stop or the final drop guard.

use super::{Interrupted, Request, Runner, group::Group, io::read_limited};
use anyhow::{Result, ensure};
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use rustix::process::{WaitId, WaitIdOptions, waitid};
use serde::Serialize;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::task::JoinHandle;

#[derive(Debug)]
pub(crate) struct CommandTimedOut;
impl std::fmt::Display for CommandTimedOut {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("command_timed_out")
    }
}
impl std::error::Error for CommandTimedOut {}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Exit {
    pub(crate) code: Option<i32>,
    pub(crate) signal: Option<String>,
}

fn signal_name(signal: i32) -> String {
    Signal::try_from(signal).map_or_else(|_| format!("SIG{signal}"), |signal| signal.to_string())
}

fn peek(pid: Pid) -> Result<Option<Exit>> {
    let pid = rustix::process::Pid::from_raw(pid.as_raw())
        .ok_or_else(|| anyhow::anyhow!("command_identity_unavailable"))?;
    Ok(waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )?
    .map(|status| Exit {
        code: status.exit_status(),
        signal: status.terminating_signal().map(signal_name),
    }))
}

/// A streaming consumer gets only bounded stdout. An overflow byte is never
/// delivered; the owner must stop its command after a parser/read error.
pub(crate) struct Stdout {
    reader: tokio::process::ChildStdout,
    remaining: u64,
    failed: bool,
}

impl Stdout {
    pub(crate) async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        ensure!(!self.failed, "command_stdout_limit_exceeded");
        if buffer.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut extra = [0; 1];
            let count = self.reader.read(&mut extra).await?;
            self.failed = count != 0;
            ensure!(!self.failed, "command_stdout_limit_exceeded");
            return Ok(0);
        }
        let maximum = (self.remaining.min(buffer.len() as u64)) as usize;
        let count = self.reader.read(&mut buffer[..maximum]).await?;
        self.remaining -= count as u64;
        Ok(count)
    }
}

type ReadTask = JoinHandle<Result<Vec<u8>>>;

pub(crate) struct Child {
    group: Group,
    deadline: Instant,
    stdin: Option<JoinHandle<Result<()>>>,
    stdout: Option<ReadTask>,
    stderr: Option<ReadTask>,
    stream: Option<Stdout>,
    retired: Option<Exit>,
    failed: bool,
    pub(crate) output: Vec<u8>,
    pub(crate) errors: Vec<u8>,
}

impl Child {
    pub(crate) fn pid(&self) -> u32 {
        self.group.pid.as_raw() as u32
    }
    pub(crate) fn take_stdout(&mut self) -> Option<Stdout> {
        self.stream.take()
    }

    async fn finished(task: &mut Option<ReadTask>, retained: &mut Vec<u8>) -> Result<()> {
        if task.as_ref().is_some_and(JoinHandle::is_finished) {
            *retained = task.take().unwrap().await??;
        }
        Ok(())
    }

    /// Preserve early I/O errors and terminal signals without releasing the
    /// group reservation. A successful exit still requires a later stop/reap.
    pub(crate) async fn poll(&mut self) -> Result<Option<Exit>> {
        ensure!(!self.failed, "command_stream_close_failed");
        if let Some(exit) = &self.retired {
            return Ok(Some(exit.clone()));
        }
        ensure!(self.group.reserved(), "command_group_already_reaped");
        let streams = async {
            if self.stdin.as_ref().is_some_and(JoinHandle::is_finished) {
                self.stdin.take().unwrap().await??;
            }
            Self::finished(&mut self.stdout, &mut self.output).await?;
            Self::finished(&mut self.stderr, &mut self.errors).await
        }
        .await;
        if let Err(error) = streams {
            self.failed = true;
            return Err(error);
        }
        let status = peek(self.group.pid)?;
        if status.is_none() && Instant::now() >= self.deadline {
            return Err(CommandTimedOut.into());
        }
        Ok(status)
    }

    /// Stop the reserved group, retain its actual leader result, drain/close
    /// every owned sink and reap exactly once. Cancellation never skips cleanup.
    pub(crate) async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        if let Some(exit) = &self.retired {
            ensure!(!self.failed, "command_stream_close_failed");
            return Ok(exit.clone());
        }
        ensure!(self.group.reserved(), "command_group_already_reaped");
        let status = match super::cleanup::stop_with_grace(&mut self.group, grace).await {
            Ok(status) => status,
            Err(error) => {
                self.failed = true;
                return Err(error);
            }
        };
        let exit = Exit {
            code: status.code(),
            signal: status.signal().map(signal_name),
        };
        self.retired = Some(exit.clone());
        let input = if let Some(task) = self.stdin.take() {
            finish_task(task).await
        } else {
            Ok(())
        };
        let output = if let Some(task) = self.stdout.take() {
            finish_task(task).await
        } else {
            Ok(Vec::new())
        };
        let errors = if let Some(task) = self.stderr.take() {
            finish_task(task).await
        } else {
            Ok(Vec::new())
        };
        if let Ok(output) = &output
            && !output.is_empty()
        {
            self.output.clone_from(output);
        }
        if let Ok(errors) = &errors
            && !errors.is_empty()
        {
            self.errors.clone_from(errors);
        }
        self.failed |= input.is_err() || output.is_err() || errors.is_err();
        input?;
        output?;
        errors?;
        ensure!(!self.failed, "command_stream_close_failed");
        Ok(exit)
    }
}

async fn finish_task<T>(mut task: JoinHandle<Result<T>>) -> Result<T> {
    match tokio::time::timeout(Duration::from_secs(5), &mut task).await {
        Ok(result) => result?,
        Err(_) => {
            task.abort();
            let _ = task.await;
            anyhow::bail!("command_stream_close_timed_out");
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Some(task) = &self.stdin {
            task.abort();
        }
        if let Some(task) = &self.stdout {
            task.abort();
        }
        if let Some(task) = &self.stderr {
            task.abort();
        }
        // Group's drop runs next, killing before any wait can release its ID.
    }
}

impl Runner {
    pub(crate) fn spawn(
        &self,
        executable: &Path,
        arguments: &[String],
        request: Request,
    ) -> Result<Child> {
        self.spawn_owned(executable, arguments, request, false)
    }

    pub(crate) fn spawn_stream(
        &self,
        executable: &Path,
        arguments: &[String],
        request: Request,
    ) -> Result<Child> {
        ensure!(
            matches!(&request.stdout, super::Destination::Capture(bytes) if bytes.is_empty()),
            "command_stream_requires_empty_capture_sink"
        );
        self.spawn_owned(executable, arguments, request, true)
    }

    fn spawn_owned(
        &self,
        executable: &Path,
        arguments: &[String],
        request: Request,
        stream: bool,
    ) -> Result<Child> {
        if !request.cleanup && self.cancellation.code() != 0 {
            return Err(Interrupted(self.cancellation.code()).into());
        }
        let mut command = Command::new(executable);
        command
            .args(arguments)
            .current_dir(&self.directory)
            .env_clear()
            .envs(&self.environment)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let child = command.spawn()?;
        let raw = i32::try_from(child.id())
            .ok()
            .filter(|pid| *pid > 1)
            .ok_or_else(|| anyhow::anyhow!("command_identity_unavailable"))?;
        let mut group = Group::new(child, Pid::from_raw(raw));
        let mut stdin = tokio::process::ChildStdin::from_std(group.child().stdin.take().unwrap())?;
        let stdout = tokio::process::ChildStdout::from_std(group.child().stdout.take().unwrap())?;
        let stderr = tokio::process::ChildStderr::from_std(group.child().stderr.take().unwrap())?;
        let stdin = tokio::spawn(async move {
            stdin.write_all(&request.input).await?;
            drop(stdin);
            Ok(())
        });
        let (stdout, reader) = if stream {
            (
                None,
                Some(Stdout {
                    reader: stdout,
                    remaining: request.limits.stdout,
                    failed: false,
                }),
            )
        } else {
            (
                Some(tokio::spawn(read_limited(
                    stdout,
                    request.stdout,
                    request.limits.stdout,
                    "stdout",
                ))),
                None,
            )
        };
        Ok(Child {
            group,
            deadline: Instant::now() + request.limits.timeout,
            stdin: Some(stdin),
            stdout,
            stderr: Some(tokio::spawn(read_limited(
                stderr,
                request.stderr,
                request.limits.stderr,
                "stderr",
            ))),
            stream: reader,
            retired: None,
            failed: false,
            output: Vec::new(),
            errors: Vec::new(),
        })
    }
}
