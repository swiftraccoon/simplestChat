//! Complete application-only host deployment and its one bounded rollback.

mod budget;
mod execution;
mod files;
mod options;
mod policy;
pub(super) mod process;
pub(super) mod records;
mod rollback_diagnostic;
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
pub(super) use execution::maintenance::execute as execute_maintenance;

pub(super) struct Options {
    pub revision: String,
    pub quiet_seconds: u64,
    pub whole: Duration,
    pub reserve: Duration,
    /// Restart the release that is already selected, told of the relay.
    pub relay: Option<Relay>,
}
impl Options {
    /// A restart of the selected release told of `relay`: rooms are given
    /// ten minutes to empty, inside the longest whole budget.
    fn relayed(revision: &str, relay: Relay) -> Self {
        Self {
            revision: revision.to_owned(),
            quiet_seconds: 600,
            whole: budget::MAX_WHOLE,
            reserve: Duration::from_secs(60),
            relay: Some(relay),
        }
    }
}
/// The verified relay an application is told of when it is restarted as the
/// release it already runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Relay {
    pub domain: String,
    pub secret: String,
    /// Where the public origin is reached instead of where its name resolves.
    pub address: Option<std::net::IpAddr>,
}
impl Relay {
    /// The three settings the application reads the relay by, in the order
    /// they are appended to its environment; nothing for a hostname or a
    /// secret that could carry anything else into that file.
    pub(crate) fn settings(&self) -> Option<[(&'static str, String); 3]> {
        let domain = &self.domain;
        let admitted = crate::host::domain(domain) && crate::host::hexadecimal(&self.secret, 64);
        admitted.then(|| {
            let addresses = format!(
                "turn:{domain}:3478?transport=udp,turn:{domain}:3478?transport=tcp,\
                 turns:{domain}:5349?transport=tcp"
            );
            [
                ("TURN_URLS", addresses),
                ("TURN_SECRET", self.secret.clone()),
                ("TURN_TTL", "86400".to_owned()),
            ]
        })
    }

    /// `environment` with the three settings appended. One that already
    /// has a line of any of the three names is refused: a setting that is
    /// there is never rotated by a restart.
    pub(crate) fn appended(&self, environment: &str) -> Result<String, &'static str> {
        let settings = self.settings();
        let settings = settings.ok_or("Invalid managed TURN configuration")?;
        let configured = crate::text::python_lines(environment).any(|line| {
            let named = line.split_once('=').map_or(line, |(name, _)| name);
            settings.iter().any(|(name, _)| *name == named)
        });
        if configured {
            return Err("TURN is already configured; rotation requires separate maintenance");
        }
        let lines = settings
            .iter()
            .map(|(name, value)| format!("{name}={value}\n"));
        Ok(environment.to_owned() + &lines.collect::<String>())
    }
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
    pub(super) fn fixed() -> Self {
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
    Source,
    ProxyStop,
    Validate,
}
impl Role {
    fn seconds(self) -> u64 {
        match self {
            Self::Load => 300,
            Self::Dump | Self::Up => 60,
            Self::Stop => 45,
            Self::Wait | Self::Validate => 20,
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
    match operate(started, &options) {
        None => ExitCode::from(2),
        Some(true) => ExitCode::SUCCESS,
        Some(false) => ExitCode::FAILURE,
    }
}

/// Restart the application as the release it already runs, told of the
/// verified relay: the whole app-only deployment of that release, with its
/// backup, its bounded wait for rooms to empty and its one rollback. The
/// report is printed as a deployment's is. Returns whether it passed.
///
/// # Panics
///
/// As [`run`], this must be called outside an entered Tokio runtime.
pub(crate) fn activate(revision: &str, relay: Relay) -> bool {
    operate(Instant::now(), &Options::relayed(revision, relay)) == Some(true)
}

/// One whole deployment on the fixed host inside its own budget.
fn operate(started: Instant, options: &Options) -> Option<bool> {
    super::stage::settled(started, options.whole, || {
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
            options,
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
