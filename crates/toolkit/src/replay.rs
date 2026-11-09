//! The reserved output-replay child and the owned group that feeds it.
//!
//! A runner forwards a bounded byte projection to its own inherited stdout or
//! stderr through a child of its retained executable, started with the single
//! argument [`CHILD`]. Only that child may block on the inherited pipe; its
//! parent stays cancellable and retires and reaps the child's group. Every
//! binary that hosts such a runner calls [`route`] before parsing anything else.

use crate::{
    executable::Executable,
    owned_process::{self, Group, Signals},
    tooling::{self, Environment, Failure, Result},
};
use std::{
    ffi::OsString,
    fs::File,
    io::{Read, Write},
    os::unix::process::CommandExt,
    path::Path,
    process::{Command, ExitCode, ExitStatus, Stdio},
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;

/// The reserved child's only argument.
pub const CHILD: &str = "__replay";
/// The largest projection either side accepts. A caller that admits its own
/// output before replaying it bounds it with this value.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Kept back from the caller's deadline for retiring and reaping the child.
pub const RESERVE: Duration = Duration::from_secs(20);
/// Between terminating a failed child's group and finalizing it.
pub const GRACE: Duration = Duration::from_secs(5);

fn failure() -> Failure {
    Failure::new(1, "Release controller preparation failed.")
}
fn close(file: File) -> Result<()> {
    nix::unistd::close(file).map_err(|_| failure())
}

/// Retires an owned child group: whether its cleanup is proven, and the
/// reaped status. Tests substitute their own to inject unproven results.
#[expect(
    async_fn_in_trait,
    reason = "callers await finalizers on their own runtime; none needs a Send future"
)]
pub trait Finalizer {
    async fn finish(&self, group: &mut Group) -> (bool, Result<ExitStatus>);
}
/// Finish the group, then reap only after terminal observation succeeded.
#[derive(Clone, Copy, Debug, Default)]
pub struct Finalize;
impl Finalizer for Finalize {
    async fn finish(&self, group: &mut Group) -> (bool, Result<ExitStatus>) {
        let pid = group.pid();
        let cleaned = owned_process::finish(pid).await;
        // NOWAIT keeps the identity reserved through the final group signal.
        // Never block in reap unless terminal observation has succeeded.
        let terminal =
            tokio::time::timeout(Duration::from_secs(5), owned_process::exited(pid)).await;
        let reaped = if matches!(terminal, Ok(Ok(()))) {
            group.reap().await
        } else {
            Err(Failure::new(1, "Production image command reap failed."))
        };
        (cleaned, reaped)
    }
}

/// A replay's result, whether its child was started, and whether that child's
/// group settled; a started child that did not settle leaves cleanup unproven.
#[derive(Debug)]
pub struct ReplayOutcome {
    pub result: Result<()>,
    pub admitted: bool,
    pub settled: bool,
}

/// Replay `bytes` to this process's stdout, or to its stderr when `stderr` is
/// set, through the reserved child of the current executable. The caller
/// keeps its signal registration through its own final fences.
pub async fn replay_output(
    root: &Path,
    environment: &Environment,
    signals: &mut Signals,
    bytes: &[u8],
    stderr: bool,
    deadline: Instant,
) -> ReplayOutcome {
    match std::env::current_exe() {
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

/// [`replay_output`] through `executable`, retained before the child starts
/// and revalidated after it settles.
pub async fn replay_selected(
    root: &Path,
    environment: &Environment,
    signals: &mut Signals,
    bytes: &[u8],
    stderr: bool,
    deadline: Instant,
    executable: &Path,
) -> ReplayOutcome {
    let mut admitted = false;
    let mut settled = false;
    let result = async {
        let proof = Executable::open(executable)?;
        let selected = match proof.validate() {
            Ok(path) => path.to_owned(),
            Err(error) => {
                let _ = proof.close();
                return Err(error);
            }
        };
        let mut command = tooling::command(root, environment, selected, [CHILD]);
        if stderr {
            command.stdout(std::io::stderr());
        } else {
            command.stdout(Stdio::inherit());
        }
        command.stderr(Stdio::null());
        // A refusal inside the replay owner, before its own dispatch, still
        // counts as admitted: the caller's cleanup proof stays sticky.
        admitted = true;
        let replay = run(command, bytes, signals, deadline).await;
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

/// Feed `bytes` to `command`'s stdin in its own reserved process group, then
/// retire and reap that group. A signal or the deadline, less its reserve,
/// terminates the child; it never blocks this parent.
pub async fn run(
    command: Command,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
) -> Result<()> {
    run_with_finalizer(command, bytes, signals, deadline, &Finalize).await
}

/// [`run`] with an explicit group finalizer.
pub async fn run_with_finalizer(
    mut command: Command,
    bytes: &[u8],
    signals: &mut Signals,
    deadline: Instant,
    finalizer: &impl Finalizer,
) -> Result<()> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err(failure());
    }
    if let Some(code) = signals.pending().await {
        return Err(Failure::new(code, "Release controller replay interrupted."));
    }
    replay_budget(deadline)?;
    command.process_group(0).stdin(Stdio::piped());
    replay_budget(deadline)?;
    let child = command.spawn().map_err(|_| failure())?;
    let mut group = Group::new(child);
    let pid = group.pid();
    drop(command);
    let mut input = Some(
        tokio::process::ChildStdin::from_std(group.child().stdin.take().ok_or_else(failure)?)
            .map_err(|_| failure())?,
    );
    let completed = async {
        input
            .as_mut()
            .unwrap()
            .write_all(bytes)
            .await
            .map_err(|_| failure())?;
        let owned = input
            .take()
            .unwrap()
            .into_owned_fd()
            .map_err(|_| failure())?;
        close(File::from(owned))?;
        owned_process::exited(pid).await
    };
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .and_then(|v| v.checked_sub(RESERVE))
        .unwrap_or_default();
    let result = tokio::select! {
        biased;
        code = signals.next() => Err(Failure::new(code, "Release controller replay interrupted.")),
        result = tokio::time::timeout(remaining, completed) => match result {
            Ok(result) => result,
            Err(_) => Err(Failure::new(124, "Release controller replay timed out.")),
        },
    };
    let closed = input
        .take()
        .map(|input| {
            input
                .into_owned_fd()
                .map_err(|_| failure())
                .and_then(|fd| close(File::from(fd)))
        })
        .transpose();
    if result.is_err() {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::TERM);
        let _ = tokio::time::timeout(GRACE, owned_process::exited(pid)).await;
    }
    let (settled, reaped) = finalizer.finish(&mut group).await;
    result?;
    closed?;
    let status = tooling::shell_status(reaped.map_err(|_| failure())?);
    if settled && status == 0 {
        Ok(())
    } else {
        Err(failure())
    }
}
fn replay_budget(deadline: Instant) -> Result<()> {
    if deadline
        .checked_duration_since(Instant::now())
        .and_then(|value| value.checked_sub(RESERVE))
        .is_none_or(|value| value.is_zero())
    {
        return Err(Failure::new(124, "Release controller replay timed out."));
    }
    Ok(())
}

/// Run the reserved child when `arguments`, without the program name, are
/// exactly [`CHILD`]; otherwise return `None` for the binary's own parser.
/// Call it before registering signals: the child keeps their defaults, so its
/// parent can retire it.
pub fn route(arguments: &[OsString]) -> Option<ExitCode> {
    if arguments.len() != 1 || arguments[0] != CHILD {
        return None;
    }
    Some(match child() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let _ = writeln!(std::io::stderr(), "{}", error.message);
            ExitCode::from(error.code)
        }
    })
}

/// Only this reserved child may block on the inherited runner output pipe.
/// Its parent remains cancellable and owns its final group retirement/reap.
fn child() -> Result<()> {
    let mut input = std::io::stdin().lock();
    let mut output = std::io::stdout().lock();
    let mut bytes = 0_u64;
    let mut buffer = [0; 8192];
    loop {
        let size = input.read(&mut buffer).map_err(|_| failure())?;
        if size == 0 {
            break;
        }
        bytes = bytes.checked_add(size as u64).ok_or_else(failure)?;
        if bytes > MAX_BYTES {
            return Err(failure());
        }
        output.write_all(&buffer[..size]).map_err(|_| failure())?;
    }
    output.flush().map_err(|_| failure())
}
