//! Bounded native commands reuse the browser supervisor's reserved group guard.

use super::command::{Command, SeparateLogs};
use crate::ci::browser_suite::{
    cleanup,
    process::{Group, Signals, exited},
};
use crate::tooling::Environment;
use crate::tooling::{self, Failure, Result};
use rustix::process::Signal;
use std::fs::File;
use std::io::Seek;
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
        let mdns = request.mdns.clone();
        let mdns_admission = mdns.as_ref().map(|v| v.begin()).transpose();
        let (io, admission) = OwnedIo::take(&mut request);
        let owned_io = io.combined.is_some() || io.separate.is_some() || io.input.is_some();
        let cleanup = request.cleanup;
        let result = match admission.and(mdns_admission.map(|_| ())) {
            Ok(()) => self.execute_body(request, &io, finalizer).await,
            Err(error) => Err(error),
        };
        // This runs on setup/spawn errors too. A body-local Group guard finishes
        // its fallback before these bounded EOF and explicit-close attempts.
        let closed = io.finish().await;
        if let Some(mdns) = mdns {
            mdns.closed(closed.is_ok());
        }
        let output = result?;
        closed?;
        if owned_io && !cleanup {
            self.ready().await?;
        }
        Ok(output)
    }

    async fn execute_body(
        &mut self,
        mut request: Command,
        io: &OwnedIo,
        finalizer: &impl Finalizer,
    ) -> Result<Vec<u8>> {
        if !request.cleanup {
            self.ready().await?;
        }
        if io.combined.is_some() && (request.stdout.is_some() || request.capture.is_some()) {
            return Err(Failure::new(1, "CI combined output requires its own mode."));
        }
        if io.separate.is_some()
            && (io.combined.is_some() || request.stdout.is_some() || request.capture.is_some())
        {
            return Err(Failure::new(1, "CI separate output requires its own mode."));
        }
        if request.stdout.is_some() && request.capture.is_none() {
            return Err(Failure::new(
                1,
                "CI file output requires bounded stderr capture.",
            ));
        }
        let (root, environment) = match request.comparison.as_ref() {
            Some(invocation) => invocation.context(
                comparison_role(request.program)
                    .ok_or_else(|| Failure::new(1, "CI comparison role invalid."))?,
            )?,
            None => (self.root, self.environment),
        };
        let mut command =
            tooling::command(root, environment, executable(&request)?, &request.arguments);
        use std::os::unix::process::CommandExt;
        command.process_group(0).stdin(Stdio::null());
        if let Some(input) = &io.input {
            command.stdin(Stdio::from(input_stdio(input)?));
        }
        let sink = request
            .stdout
            .take()
            .map(std::sync::Arc::try_unwrap)
            .transpose()
            .map_err(|_| Failure::new(1, "CI output sink must have one owner."))?;
        if let Some(logs) = &io.separate {
            command
                .stdout(Stdio::from(logs.stdout.writer()?))
                .stderr(Stdio::from(logs.stderr.writer()?));
        } else if let Some(log) = &io.combined {
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
            let bytes = if io.combined.is_some() || io.separate.is_some() {
                loop {
                    if io.combined.as_ref().is_some_and(|log| log.failed())
                        || io
                            .separate
                            .as_ref()
                            .is_some_and(|logs| logs.stdout.failed() || logs.stderr.failed())
                    {
                        return Err(Failure::new(
                            1,
                            if io.separate.is_some() {
                                "CI separate log exceeded its bound or failed."
                            } else {
                                "CI combined log exceeded its bound or failed."
                            },
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
        let mut mdns_timeout = false;
        let mut mdns_interrupted = None;
        let outcome = if request.cleanup {
            match tokio::time::timeout(request.timeout, completed).await {
                Ok(result) => result,
                Err(_) => {
                    mdns_timeout = true;
                    Err(Failure::new(124, "Production image command timed out."))
                }
            }
        } else {
            tokio::select! {
                biased;
                code = self.signals.next() => {
                    self.interrupted = Some(code);
                    mdns_interrupted=Some(code);
                    Err(Failure::new(code,"Production image command interrupted."))
                },
                result = tokio::time::timeout(request.timeout,completed) => match result {
                    Ok(result)=>result,
                    Err(_)=>{mdns_timeout=true;
                        Err(Failure::new(124,"Production image command timed out."))},
                },
            }
        };
        if outcome.is_err() {
            let signal = if self.interrupted == Some(130) {
                Signal::INT
            } else {
                Signal::TERM
            };
            if request.program == super::command::Program::ComparisonBenchmark {
                // bench local owns fresh nested groups: allow its cooperative
                // finalizers before this outer CLI group can be escalated.
                let _ = rustix::process::kill_process(pid, signal);
            } else {
                let _ = rustix::process::kill_process_group(pid, signal);
            }
            let _ = tokio::time::timeout(request.grace, exited(pid)).await;
        }
        let (cleaned, reaped) = finalizer.finish(&mut group).await;
        if cleaned && reaped.is_ok() {
            self.cleanup_passed = prior_cleanup;
        }
        if let Some(mdns) = &request.mdns {
            mdns.outcome(
                reaped
                    .as_ref()
                    .ok()
                    .and_then(std::process::ExitStatus::code),
                mdns_timeout,
                mdns_interrupted,
                cleaned && reaped.is_ok(),
            );
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

struct OwnedIo {
    combined: Option<super::bounded_log::Log>,
    separate: Option<SeparateLogs>,
    input: Option<File>,
}
impl OwnedIo {
    fn take(request: &mut Command) -> (Self, Result<()>) {
        // A borrowed peer refuses dispatch, but cannot skip unique peer closes.
        let (combined, combined_admission) = owner(
            request.combined_log.take(),
            "CI combined log must have one owner.",
        );
        let (separate, separate_admission) = owner(
            request.separate_logs.take(),
            "CI separate logs must have one owner.",
        );
        let (input, input_admission) = owner(request.stdin.take(), "CI input must have one owner.");
        (
            Self {
                combined,
                separate,
                input,
            },
            combined_admission
                .and(separate_admission)
                .and(input_admission),
        )
    }

    async fn finish(self) -> Result<()> {
        let input_closed = self
            .input
            .map(|file| {
                nix::unistd::close(file).map_err(|_| Failure::new(1, "CI input close failed."))
            })
            .transpose();
        let (combined, separate) = tokio::join!(
            finish_log(self.combined, "CI combined log completion is unproven."),
            finish_separate(self.separate)
        );
        input_closed?;
        combined?;
        separate
    }
}

fn owner<T>(value: Option<std::sync::Arc<T>>, message: &str) -> (Option<T>, Result<()>) {
    match value.map(std::sync::Arc::try_unwrap) {
        Some(Ok(value)) => (Some(value), Ok(())),
        Some(Err(_)) => (None, Err(Failure::new(1, message))),
        None => (None, Ok(())),
    }
}

async fn finish_log(log: Option<super::bounded_log::Log>, message: &str) -> Result<()> {
    if let Some(log) = log {
        let value = log.finish().await?;
        if !value.complete {
            return Err(Failure::new(1, message));
        }
    }
    Ok(())
}

async fn finish_separate(logs: Option<SeparateLogs>) -> Result<()> {
    if let Some(logs) = logs {
        let message = "CI separate log completion is unproven.";
        let (stdout, stderr) = tokio::join!(
            finish_log(Some(logs.stdout), message),
            finish_log(Some(logs.stderr), message)
        );
        stdout?;
        stderr?;
    }
    Ok(())
}

/// Settle admitted bounded output and retained input before a caller refuses
/// forwarding. No CLI runs. Unique peers close even when another is borrowed;
/// external aliases remain their caller's responsibility. The legacy stdout
/// File mode is unchanged and is not used by these paired-output commands.
pub(super) async fn settle_unforwarded(mut request: Command) -> Result<()> {
    let (io, admission) = OwnedIo::take(&mut request);
    drop(request);
    let closed = io.finish().await;
    admission.and(closed)
}

fn input_stdio(input: &File) -> Result<File> {
    let invalid = || Failure::new(1, "CI retained input invalid.");
    if !input.metadata().map_err(|_| invalid())?.is_file()
        || rustix::fs::fcntl_getfl(input)
            .map_err(|_| invalid())?
            .intersection(rustix::fs::OFlags::ACCMODE)
            != rustix::fs::OFlags::RDONLY
    {
        return Err(invalid());
    }
    // CUR+0 verifies seekability without resetting the caller-selected cursor.
    let mut retained = input;
    retained.stream_position().map_err(|_| invalid())?;
    let flags = rustix::io::fcntl_getfd(input).map_err(|_| invalid())?;
    rustix::io::fcntl_setfd(input, flags | rustix::io::FdFlags::CLOEXEC).map_err(|_| invalid())?;
    input.try_clone().map_err(|_| invalid())
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
        request.comparison.is_some(),
        request.zstd.is_some(),
        request.load_image.is_some(),
        request.mdns.is_some(),
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
            | Program::ComparisonGit
            | Program::ComparisonCargo
            | Program::ComparisonBenchmark
            | Program::Zstd
            | Program::MdnsInterfaces
            | Program::MdnsPlist
            | Program::MdnsLaunch
            | Program::MdnsLogs
            | Program::MdnsCompiler
            | Program::MdnsProbe
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
        Program::Git | Program::Docker => request
            .load_image
            .as_ref()
            .map(|value| value.validate(request.program)),
        Program::CargoAudit => request.executable.as_ref().map(|value| value.validate()),
        Program::Node => request.node.as_ref().map(|value| value.validate()),
        Program::Zstd => request.zstd.as_ref().map(|value| value.validate()),
        Program::MdnsInterfaces
        | Program::MdnsPlist
        | Program::MdnsLaunch
        | Program::MdnsLogs
        | Program::MdnsCompiler
        | Program::MdnsProbe => request
            .mdns
            .as_ref()
            .map(|value| value.validate(request.program)),
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
        Program::ComparisonGit | Program::ComparisonCargo | Program::ComparisonBenchmark => {
            request.comparison.as_ref().map(|value| {
                value.validate(comparison_role(request.program).expect("comparison role"))
            })
        }
        _ => None,
    }
    .ok_or_else(|| Failure::new(1, "CI execution requires its validated executable role."))??;
    Ok(path.as_os_str())
}

fn comparison_role(
    program: super::command::Program,
) -> Option<super::performance_compare::files::Role> {
    use super::{command::Program, performance_compare::files::Role};
    match program {
        Program::ComparisonGit => Some(Role::Git),
        Program::ComparisonCargo => Some(Role::Cargo),
        Program::ComparisonBenchmark => Some(Role::Benchmark),
        _ => None,
    }
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
