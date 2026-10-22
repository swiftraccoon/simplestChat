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
    /// Forward a bounded byte projection through the existing owned replay CLI.
    async fn replay(&mut self, _: &[u8], _: bool, _: std::time::Instant) -> Result<()> {
        Err(Failure::new(1, "CI output replay is unavailable."))
    }
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

    /// Transfer the existing registration only after all preparation groups
    /// are closed. The next harness owns observing it through its own cleanup.
    pub(super) async fn into_signals(mut self) -> Result<Signals> {
        self.ready().await?;
        if !self.cleanup_passed {
            return Err(Failure::new(1, "CI preparation group cleanup is unproven."));
        }
        Ok(self.signals)
    }

    async fn execute(&mut self, request: Command) -> Result<Vec<u8>> {
        self.execute_with(request, &Finalize).await
    }

    async fn execute_with(
        &mut self,
        mut request: Command,
        finalizer: &impl Finalizer,
    ) -> Result<Vec<u8>> {
        let combined = request
            .combined_log
            .take()
            .map(std::sync::Arc::try_unwrap)
            .transpose()
            .map_err(|_| Failure::new(1, "CI combined log must have one owner."))?;
        let combined_mode = combined.is_some();
        let cleanup = request.cleanup;
        let result = self.execute_body(request, &combined, finalizer).await;
        // This runs on setup/spawn errors too. A body-local Group guard finishes
        // its fallback before these bounded EOF and explicit-close attempts.
        let logged = match combined {
            Some(log) => log.finish().await.and_then(|value| {
                if value.complete {
                    Ok(())
                } else {
                    Err(Failure::new(1, "CI combined log completion is unproven."))
                }
            }),
            None => Ok(()),
        };
        let output = result?;
        logged?;
        if combined_mode && !cleanup {
            self.ready().await?;
        }
        Ok(output)
    }

    async fn execute_body(
        &mut self,
        mut request: Command,
        combined: &Option<super::bounded_log::Log>,
        finalizer: &impl Finalizer,
    ) -> Result<Vec<u8>> {
        if !request.cleanup {
            self.ready().await?;
        }
        if combined.is_some() && (request.stdout.is_some() || request.capture.is_some()) {
            return Err(Failure::new(1, "CI combined output requires its own mode."));
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
            executable(&request)?,
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
        if let Some(log) = combined {
            command
                .stdout(Stdio::from(log.writer()?))
                .stderr(Stdio::from(log.writer()?));
        } else if let Some(file) = &sink {
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
            let bytes = if let Some(log) = combined {
                loop {
                    if log.failed() {
                        return Err(Failure::new(
                            1,
                            "CI combined log exceeded its bound or failed.",
                        ));
                    }
                    tokio::select! {
                        result = exited(pid) => { result?; break Vec::new(); },
                        _ = tokio::time::sleep(Duration::from_millis(10)) => {},
                    }
                }
            } else if let Some(task) = &mut reader.0 {
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
        let (cleaned, reaped) = finalizer.finish(&mut group).await;
        if cleaned && reaped.is_ok() {
            self.cleanup_passed = prior_cleanup;
        }
        // Preserve a known timeout/interruption even if terminal observation or
        // reap subsequently fails. Unproven cleanup remains a sticky false flag.
        let output = outcome?;
        let status = reaped?;
        let code = tooling::shell_status(status);
        if code != 0 {
            return Err(Failure::new(code, "Production image command failed."));
        }
        if !cleaned {
            return Err(Failure::new(1, "Production image command cleanup failed."));
        }
        Ok(output)
    }
}

/// Each selected role admits exactly one matching proof; no proof permits a
/// fallback to PATH or substitution into another executable role.
fn executable(request: &Command) -> Result<&std::ffi::OsStr> {
    use super::command::Program;
    let proofs = [
        request.executable.is_some(),
        request.node.is_some(),
        request.dtls.is_some(),
        request.ops.is_some(),
        request.source_git.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    let selected = matches!(
        request.program,
        Program::CargoAudit
            | Program::Node
            | Program::DtlsPython
            | Program::DtlsWorker
            | Program::OpsCargo
            | Program::OpsRustc
            | Program::OpsGit
    );
    if proofs == 0 && !selected {
        return Ok(std::ffi::OsStr::new(request.program.name()));
    }
    if proofs != 1 {
        return Err(Failure::new(
            1,
            "CI execution requires its validated executable role.",
        ));
    }
    let path = match request.program {
        Program::CargoAudit => request.executable.as_ref().map(|value| value.validate()),
        Program::Node => request.node.as_ref().map(|value| value.validate()),
        Program::DtlsPython => request.dtls.as_ref().map(|value| value.validate(false)),
        Program::DtlsWorker => request.dtls.as_ref().map(|value| value.validate(true)),
        Program::OpsCargo => request
            .ops
            .as_ref()
            .map(|value| value.validate(super::ops_artifact::recipe::Role::Cargo)),
        Program::OpsRustc => request
            .ops
            .as_ref()
            .map(|value| value.validate(super::ops_artifact::recipe::Role::Rustc)),
        Program::OpsGit => request.source_git.as_ref().map(|value| value.validate()),
        _ => None,
    }
    .ok_or_else(|| Failure::new(1, "CI execution requires its validated executable role."))??;
    Ok(path.as_os_str())
}

pub(super) trait Finalizer {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>);
}
pub(super) struct Finalize;
impl Finalizer for Finalize {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let pid = group.pid();
        let cleaned = cleanup::finish(pid).await;
        // NOWAIT keeps the identity reserved through the final group signal.
        // Never block in reap unless terminal observation has succeeded.
        let terminal = tokio::time::timeout(Duration::from_secs(5), exited(pid)).await;
        let reaped = if matches!(terminal, Ok(Ok(()))) {
            group.reap().await
        } else {
            Err(Failure::new(1, "Production image command reap failed."))
        };
        (cleaned, reaped)
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

/// Same retained executable proof and existing replay owner for all callers.
/// The caller keeps its original Signals registration through final fences.
pub(super) struct ReplayOutcome {
    pub result: Result<()>,
    admitted: bool,
    settled: bool,
}
pub(super) async fn replay_output(
    root: &Path,
    environment: &Environment,
    signals: &mut Signals,
    bytes: &[u8],
    stderr: bool,
    deadline: std::time::Instant,
) -> ReplayOutcome {
    let executable = std::env::current_exe();
    match executable {
        Ok(executable) => {
            replay_selected(
                root,
                environment,
                signals,
                bytes,
                stderr,
                deadline,
                &executable,
            )
            .await
        }
        Err(_) => ReplayOutcome {
            admitted: false,
            settled: false,
            result: Err(Failure::new(1, "CI output replay executable unavailable.")),
        },
    }
}
async fn replay_selected(
    root: &Path,
    environment: &Environment,
    signals: &mut Signals,
    bytes: &[u8],
    stderr: bool,
    deadline: std::time::Instant,
    executable: &Path,
) -> ReplayOutcome {
    let mut admitted = false;
    let mut settled = false;
    let result = async {
        let proof = super::release_checks::files::Executable::open(executable)?;
        let selected = match proof.validate() {
            Ok(path) => path.to_owned(),
            Err(error) => {
                let _ = proof.close();
                return Err(error);
            }
        };
        let mut command = tooling::command(
            root,
            environment,
            selected,
            ["ci", "release-controller", "__replay"],
        );
        if stderr {
            command.stdout(std::io::stderr());
        } else {
            command.stdout(Stdio::inherit());
        }
        command.stderr(Stdio::null());
        // Preserve Native's existing sticky-proof boundary, including refusal
        // inside the replay owner before its own dispatch.
        admitted = true;
        let replay =
            super::release_controller::replay::run(command, bytes, signals, deadline).await;
        settled = replay.is_ok();
        let validated = proof.validate().map(|_| ());
        let closed = proof.close();
        replay.and(validated).and(closed)
    }
    .await;
    ReplayOutcome {
        result,
        admitted,
        settled,
    }
}

impl Executor for Native<'_> {
    async fn replay(
        &mut self,
        bytes: &[u8],
        stderr: bool,
        deadline: std::time::Instant,
    ) -> Result<()> {
        self.ready().await?;
        let prior = self.cleanup_passed;
        let replay = replay_output(
            self.root,
            self.environment,
            &mut self.signals,
            bytes,
            stderr,
            deadline,
        )
        .await;
        if replay.admitted {
            self.cleanup_passed = prior && replay.settled;
        }
        if let Err(error) = &replay.result
            && matches!(error.code, 130 | 143)
        {
            self.interrupted = Some(error.code);
        }
        replay.result?;
        self.ready().await
    }
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

#[cfg(test)]
#[path = "process_tests.rs"]
mod tests;
