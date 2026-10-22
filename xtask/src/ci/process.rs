//! Bounded native commands reuse the browser supervisor's reserved group guard.

use super::command::Command;
use crate::ci::browser_suite::{
    cleanup,
    process::{Group, Signals, exited},
};
use crate::tooling::Environment;
use crate::tooling::{self, Failure, Result};
use rustix::process::Signal;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::AsyncReadExt;

pub(super) trait Executor {
    async fn run(&mut self, command: Command) -> Result<Vec<u8>>;
    async fn ready(&mut self) -> Result<()>;
    /// Cumulative proof for spawned CLI groups, not engine resources or root
    /// groups separated by sudo. No child is initially a vacuous clean proof.
    fn cleanup_passed(&self) -> bool {
        true
    }
}

pub(super) struct Native<'a> {
    root: &'a Path,
    environment: &'a Environment,
    signals: Signals,
    interrupted: Option<u8>,
    cleanup_passed: bool,
}

impl<'a> Native<'a> {
    pub(super) fn new(root: &'a Path, environment: &'a Environment, signals: Signals) -> Self {
        Self {
            root,
            environment,
            signals,
            interrupted: None,
            cleanup_passed: true,
        }
    }

    async fn execute(&mut self, mut request: Command) -> Result<Vec<u8>> {
        if !request.cleanup {
            self.ready().await?;
        }
        if request.stdout.is_some() && request.capture.is_none() {
            return Err(Failure::new(
                1,
                "CI file output requires bounded stderr capture.",
            ));
        }
        let mut command = tooling::command(
            self.root,
            self.environment,
            match (&request.executable, request.program) {
                (Some(executable), super::command::Program::CargoAudit) => {
                    executable.validate()?.as_os_str()
                }
                (None, super::command::Program::CargoAudit) | (Some(_), _) => {
                    return Err(Failure::new(
                        1,
                        "Audit execution requires a validated selected executable.",
                    ));
                }
                (None, _) => std::ffi::OsStr::new(request.program.name()),
            },
            &request.arguments,
        );
        use std::os::unix::process::CommandExt;
        command.process_group(0).stdin(Stdio::null());
        let sink = request
            .stdout
            .take()
            .map(std::sync::Arc::try_unwrap)
            .transpose()
            .map_err(|_| Failure::new(1, "CI output sink must have one owner."))?;
        if let Some(file) = &sink {
            command.stdout(Stdio::from(
                file.try_clone()
                    .map_err(|_| Failure::new(1, "CI output sink unavailable."))?,
            ));
            command.stderr(Stdio::piped());
        } else if request.capture.is_some() {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        } else {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
        let child = command.spawn().map_err(|error| {
            Failure::new(
                match error.kind() {
                    std::io::ErrorKind::NotFound => 127,
                    std::io::ErrorKind::PermissionDenied => 126,
                    _ => 1,
                },
                "Cannot start production image command.",
            )
        })?;
        let prior_cleanup = self.cleanup_passed;
        self.cleanup_passed = false;
        let mut group = Group::new(child);
        let pid = group.pid();
        drop(command);
        if let Some(file) = sink {
            nix::unistd::close(file)
                .map_err(|_| Failure::new(1, "CI output sink close failed."))?;
        }
        let reader = if let Some(maximum) = request.capture {
            let stdout = group
                .child()
                .stdout
                .take()
                .map(tokio::process::ChildStdout::from_std)
                .transpose()
                .map_err(|_| Failure::new(1, "CI output unavailable."))?;
            let stderr =
                tokio::process::ChildStderr::from_std(group.child().stderr.take().unwrap())
                    .map_err(|_| Failure::new(1, "Production image output unavailable."))?;
            Some(tokio::spawn(async move {
                let output = async {
                    match stdout {
                        Some(stdout) => read(stdout, maximum).await,
                        None => Ok(Vec::new()),
                    }
                };
                let (output, _) = tokio::try_join!(output, read(stderr, 65536))?;
                Ok::<_, Failure>(output)
            }))
        } else {
            None
        };
        let mut reader = Reader(reader);
        let completed = async {
            let bytes = if let Some(task) = &mut reader.0 {
                let (_, bytes) = tokio::try_join!(exited(pid), async {
                    task.await
                        .map_err(|_| Failure::new(1, "Production image output failed."))?
                })?;
                bytes
            } else {
                exited(pid).await?;
                Vec::new()
            };
            Ok::<_, Failure>(bytes)
        };
        let outcome = if request.cleanup {
            match tokio::time::timeout(request.timeout, completed).await {
                Ok(result) => result,
                Err(_) => Err(Failure::new(124, "Production image command timed out.")),
            }
        } else {
            tokio::select! {
                biased;
                code = self.signals.next() => {
                    self.interrupted = Some(code);
                    Err(Failure::new(code,"Production image command interrupted."))
                },
                result = tokio::time::timeout(request.timeout,completed) => match result {
                    Ok(result)=>result,
                    Err(_)=>Err(Failure::new(124,"Production image command timed out.")),
                },
            }
        };
        if outcome.is_err() {
            let signal = if self.interrupted == Some(130) {
                Signal::INT
            } else {
                Signal::TERM
            };
            let _ = rustix::process::kill_process_group(pid, signal);
            let _ = tokio::time::timeout(request.grace, exited(pid)).await;
        }
        let cleaned = cleanup::finish(pid).await;
        // NOWAIT keeps the identity reserved through the final group signal.
        // A failed signal cannot make blocking wait/reap an unbounded operation.
        let terminal = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
        if !matches!(terminal, Ok(Ok(()))) {
            return Err(Failure::new(1, "Production image command reap failed."));
        }
        let status = group.reap().await?;
        if cleaned {
            self.cleanup_passed = prior_cleanup;
        }
        let output = outcome?;
        if !cleaned {
            return Err(Failure::new(1, "Production image command cleanup failed."));
        }
        let code = tooling::shell_status(status);
        if code != 0 {
            return Err(Failure::new(code, "Production image command failed."));
        }
        Ok(output)
    }
}

struct Reader(Option<tokio::task::JoinHandle<Result<Vec<u8>>>>);
impl Drop for Reader {
    fn drop(&mut self) {
        if let Some(task) = &self.0 {
            task.abort();
        }
    }
}

async fn read(reader: impl tokio::io::AsyncRead + Unpin, maximum: u64) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(maximum + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Failure::new(1, "Production image output failed."))?;
    if bytes.len() as u64 > maximum {
        return Err(Failure::new(
            1,
            "Production image output exceeded its bound.",
        ));
    }
    Ok(bytes)
}

impl Executor for Native<'_> {
    fn cleanup_passed(&self) -> bool {
        self.cleanup_passed
    }
    async fn run(&mut self, request: Command) -> Result<Vec<u8>> {
        self.execute(request).await
    }
    async fn ready(&mut self) -> Result<()> {
        if self.interrupted.is_none() {
            self.interrupted = self.signals.pending().await;
        }
        match self.interrupted {
            Some(code) => Err(Failure::new(code, "Production image command interrupted.")),
            None => Ok(()),
        }
    }
}
