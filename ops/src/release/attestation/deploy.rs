//! Complete application-only host deployment and its one bounded rollback.

mod budget;
mod execution;
mod files;
mod options;
#[cfg(test)]
mod oracle_tests;
mod policy;
pub(super) mod process;
pub(super) mod records;
#[cfg(test)]
mod tests;

use super::control::{Cause, Owner};
use std::{
    ffi::OsString,
    fs::File,
    path::PathBuf,
    time::{Duration, Instant},
};

use execution::execute;

pub(super) struct Options {
    pub revision: String,
    pub quiet_seconds: u64,
    pub whole: Duration,
    pub reserve: Duration,
}
pub(super) struct Access {
    pub root: PathBuf,
    pub config: PathBuf,
    pub work: PathBuf,
    pub docker: PathBuf,
    pub curl: PathBuf,
    pub socket: Option<PathBuf>,
    pub uid: u32,
    #[cfg(test)]
    pub free_bytes: Option<u64>,
    #[cfg(test)]
    pub wall_seconds: Option<u64>,
    #[cfg(test)]
    fault: Option<files::Fault>,
    #[cfg(test)]
    pub virtual_poll: bool,
}
impl Access {
    fn fixed() -> Self {
        Self {
            root: "/srv/simplestchat-public".into(),
            config: "/etc/simplestchat-public".into(),
            work: "/run/simplestchat-bench".into(),
            docker: "/usr/bin/docker".into(),
            curl: "/usr/bin/curl".into(),
            socket: Some("/var/run/docker.sock".into()),
            uid: 0,
            #[cfg(test)]
            free_bytes: None,
            #[cfg(test)]
            wall_seconds: None,
            #[cfg(test)]
            fault: None,
            #[cfg(test)]
            virtual_poll: false,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Load,
    Inspect,
    Create,
    Start,
    Wait,
    Logs,
    Remove,
    Compose,
    Query,
    Size,
    Dump,
    List,
    Stop,
    Up,
    AssetCreate,
    AssetCopy,
    AssetRemove,
    Ready,
    Metrics,
}
impl Role {
    fn seconds(self) -> u64 {
        match self {
            Self::Load => 300,
            Self::Dump | Self::Up => 60,
            Self::Stop => 45,
            Self::Wait => 20,
            Self::Size => 15,
            Self::Ready | Self::Metrics => 5,
            _ => 30,
        }
    }
}
pub(super) struct Bulk<'a> {
    pub file: &'a File,
    pub maximum: u64,
}
pub(super) struct Invocation<'a> {
    pub role: Role,
    pub program: PathBuf,
    pub arguments: Vec<OsString>,
    pub body_end: Instant,
    pub settlement_end: Instant,
    pub cleanup: bool,
    pub input: Option<&'a File>,
    pub output: Option<Bulk<'a>>,
}
pub(super) struct CommandOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: Option<i32>,
    pub error: Option<Cause>,
    pub settled: bool,
    pub stdout_complete: bool,
    pub bulk_bytes: u64,
    pub bulk_sha256: String,
    pub bulk_complete: bool,
}
pub(super) trait Executor {
    async fn run(&mut self, owner: &mut Owner, request: Invocation<'_>) -> CommandOutput;
}
pub(super) struct Outcome {
    pub bytes: Vec<u8>,
    pub passed: bool,
}

const USAGE: &str = concat!(
    "Usage: simplestchat-ops release deploy --revision SHA40 ",
    "[--quiet-seconds Q] --timeout-seconds N --finalization-seconds M\n",
    "\nRoot on the fixed prepared host; explicit 0<M<N<=1800 and 0<=Q<=600.\n",
    "Stage, validate the unchanged schema, retain assets and a listed live backup,\n",
    "then replace only the app with one bounded image/config rollback on failure.\n",
    "No migrations, database restore, maintenance or operational caller switch.\n",
);

/// Deploy on the fixed host with one original deadline and known recovery debt.
/// A nonzero exit invalidates any complete or partial earlier stdout report.
///
/// # Panics
///
/// A valid operation must run outside an entered Tokio runtime. Runtime start
/// or drop inside another runtime can panic.
///
/// ```
/// use simplestchat_ops::release::deploy;
/// use std::{ffi::OsString, process::ExitCode};
/// assert_eq!(deploy::run(&[OsString::from("--help")]), ExitCode::SUCCESS);
/// ```
pub fn run(arguments: &[OsString]) -> std::process::ExitCode {
    use std::process::ExitCode;
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
        Ok(value) => value,
        Err(()) => {
            use std::io::Write;
            let _ = std::io::stderr().write_all(USAGE.as_bytes());
            return ExitCode::from(2);
        }
    };
    super::stage::lifecycle(started, options.whole, || {
        let mut owner = match Owner::new(started, options.whole, options.reserve) {
            Ok(value) => value,
            Err(_) => return (false, None),
        };
        if rustix::process::geteuid().as_raw() != 0 || rustix::system::uname().sysname() != c"Linux"
        {
            owner.record(Cause::Policy);
            return (false, Some(owner));
        }
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
        {
            Ok(value) => value,
            Err(_) => {
                owner.record(Cause::Io);
                return (false, Some(owner));
            }
        };
        let outcome = runtime.block_on(execute(
            &options,
            &Access::fixed(),
            &mut owner,
            &mut process::Native,
            started,
        ));
        drop(runtime);
        let written = super::prepare::stdout(&outcome.bytes);
        if written.is_err() {
            owner.record(Cause::Io);
        }
        (outcome.passed && written.is_ok(), Some(owner))
    })
}
