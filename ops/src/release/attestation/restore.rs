//! Complete standalone release-backup restore verification on the fixed host.
mod execution;
mod options;
#[cfg(test)]
mod oracle_tests;
mod policy;
mod process;
#[cfg(test)]
mod tests;

use super::control::{Cause, Owner};
use options::Options;
use std::{ffi::OsString, fs::File, path::PathBuf, time::Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicTarget {
    InitialRestore,
    CreatedRestore,
    FinalRestore,
    Outcome,
    Freshness,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicPoint {
    Admission,
    FileSync,
    DirectorySync,
}
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    SnapshotRead,
    SnapshotWrite,
    SnapshotFlush,
    SnapshotSync,
    SnapshotUnlink,
    InterruptBeforeRestore,
    TerminateBeforeRestore,
    Atomic(AtomicTarget, AtomicPoint),
}
pub(super) struct Access {
    pub root: PathBuf,
    pub config: PathBuf,
    pub work: PathBuf,
    pub state: PathBuf,
    pub library: PathBuf,
    pub docker: PathBuf,
    pub socket: Option<PathBuf>,
    pub uid: u32,
    #[cfg(test)]
    pub nonce: Option<[u8; 16]>,
    #[cfg(test)]
    pub timestamp: Option<String>,
    #[cfg(test)]
    pub virtual_poll: bool,
    #[cfg(test)]
    pub fault: Option<Fault>,
}
impl Access {
    fn fixed() -> Self {
        Self {
            root: "/srv/simplestchat-public".into(),
            config: "/etc/simplestchat-public".into(),
            work: "/run/simplestchat-bench".into(),
            state: "/var/lib/simplestchat-monitoring".into(),
            library: "/usr/local/libexec/simplestchat-public".into(),
            docker: "/usr/bin/docker".into(),
            socket: Some("/var/run/docker.sock".into()),
            uid: 0,
            #[cfg(test)]
            nonce: None,
            #[cfg(test)]
            timestamp: None,
            #[cfg(test)]
            virtual_poll: false,
            #[cfg(test)]
            fault: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Inspect,
    Prior,
    Create,
    Start,
    Ready,
    Sql,
    Restore,
    Ledger,
    Verify,
    Amcheck,
    CleanupSelect,
    CleanupInspect,
    CleanupRemove,
    CleanupConfirm,
}
impl Role {
    fn seconds(self) -> u64 {
        match self {
            Self::Ready => 3,
            Self::Sql | Self::Ledger | Self::Verify => 45,
            Self::Restore | Self::Amcheck => 125,
            _ => 30,
        }
    }
    fn cleanup(self) -> bool {
        matches!(
            self,
            Self::CleanupSelect | Self::CleanupInspect | Self::CleanupRemove | Self::CleanupConfirm
        )
    }
}
pub(super) struct Invocation<'a> {
    pub role: Role,
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub input: Option<&'a File>,
    pub stdout: &'a File,
    pub stderr: &'a File,
    pub body_end: Instant,
    pub settlement_end: Instant,
}
pub(super) struct CommandOutput {
    pub stdout: Vec<u8>,
    pub status: Option<i32>,
    pub settled: bool,
    pub stdout_complete: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub error: Option<Cause>,
}
pub(super) trait Executor {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput;
}
pub(super) struct Outcome {
    pub passed: bool,
    pub failure: Option<String>,
    #[cfg(test)]
    pub fault_hits: usize,
    #[cfg(test)]
    pub snapshot_unlinks: usize,
    guard: Option<execution::Guard>,
}
impl Outcome {
    pub fn finish(mut self, owner: &mut Owner) {
        if let Some(guard) = self.guard.take() {
            guard.finish(owner);
        }
    }
}
const USAGE: &str = concat!(
    "Usage: simplestchat-ops release backup verify --release-attempt release.NAME ",
    "--timeout-seconds N --finalization-seconds M\n",
    "       simplestchat-ops release backup verify --help|--licenses\n",
    "Root on the fixed prepared Linux host; explicit ASCII seconds 0<M<N<=1800.\n",
    "Verify a recorded release backup in an isolated disposable PostgreSQL container.\n",
    "No live database restore, nightly/offsite selector or operational caller switch.\n",
);
const SUCCESS: &[u8] = b"Isolated restore verified and disposable database removed.\n";
const FAILURE: &[u8] = b"Restore verification failed; inspect private restore evidence.\n";

fn refused(owner: Option<Owner>) -> (bool, Option<Owner>) {
    use std::io::Write;
    let _ = std::io::stderr().write_all(FAILURE);
    (false, owner)
}

/// Verify one recorded release backup and publish freshness only after cleanup.
/// The explicit native deadline includes options, all finalizers and stdout.
/// Existing Python callers and authenticated host bootstrap remain separate.
/// A nonzero final status invalidates any earlier success line or partial bytes.
///
/// # Panics
///
/// An admitted operation must run outside an entered Tokio runtime; constructing
/// or dropping the owned runtime inside another runtime can panic.
///
/// ```
/// use simplestchat_ops::release::backup::verify;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(verify::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> std::process::ExitCode {
    use std::{io::Write, process::ExitCode};
    let started = Instant::now();
    if arguments.len() == 1 && arguments[0] == "--licenses" {
        return if super::acquire::licenses::stdout().is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    if arguments.len() == 1 && matches!(arguments[0].to_str(), Some("--help" | "-h")) {
        return if super::prepare::stdout(USAGE.as_bytes()).is_ok() {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    let options = match Options::parse(arguments) {
        Ok(options) => options,
        Err(options::Parse::Selection) => {
            let _ = std::io::stderr().write_all(FAILURE);
            return ExitCode::FAILURE;
        }
        Err(options::Parse::Syntax) => {
            let _ = std::io::stderr().write_all(USAGE.as_bytes());
            return ExitCode::from(2);
        }
    };
    super::stage::lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(owner) => owner,
            Err(_) => return refused(None),
        };
        if rustix::process::geteuid().as_raw() != 0 || rustix::system::uname().sysname() != c"Linux"
        {
            owner.record(Cause::Policy);
            return refused(Some(owner));
        }
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(_) => {
                owner.record(Cause::Io);
                return refused(Some(owner));
            }
        };
        let outcome = runtime.block_on(execution::execute(
            &options,
            &Access::fixed(),
            &mut owner,
            &mut process::Native,
        ));
        drop(runtime);
        let passed = outcome.passed && outcome.failure.is_none();
        if passed {
            if super::prepare::stdout(SUCCESS).is_err() {
                owner.record(Cause::Io);
            }
        } else {
            let _ = std::io::stderr().write_all(FAILURE);
        }
        outcome.finish(&mut owner);
        (passed && owner.error().is_none(), Some(owner))
    })
}
