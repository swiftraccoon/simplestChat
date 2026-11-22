//! The fixture's effect boundary: the fixed service paths, one command
//! request and its retained private outcome, and the command bookkeeping
//! every runner shares. A runner executes one request at a time; this owner
//! numbers it, writes its private record, and remembers what the public
//! report may say about it.

use super::files;
use serde::Serialize;
use std::{
    ffi::OsString,
    os::fd::OwnedFd,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// What one command reads beyond its own text: at most 2 MiB of stdout.
pub(super) const MAX_INSPECTION: u64 = 2 * 1024 * 1024;
/// The reap allowance after a command's group is killed.
pub(super) const REAP: Duration = Duration::from_secs(5);

/// The service and benchmark paths of a prepared host, the trust store the
/// fixture CA enters, and the checkout. Production uses [`Paths::fixed`].
#[derive(Clone, Debug)]
pub(super) struct Paths {
    pub config: PathBuf,
    pub root: PathBuf,
    pub work: PathBuf,
    pub bench: PathBuf,
    pub trust: PathBuf,
    pub checkout: PathBuf,
}
impl Paths {
    pub(super) fn fixed(checkout: &Path) -> Self {
        Self {
            config: "/etc/simplestchat-public".into(),
            root: "/srv/simplestchat-public".into(),
            work: "/run/simplestchat-bench".into(),
            bench: "/srv/simplestchat-bench".into(),
            trust: "/usr/local/share/ca-certificates".into(),
            checkout: checkout.to_path_buf(),
        }
    }
    /// The paths that must be absent before the fixture starts.
    pub(super) fn service(&self) -> [&Path; 4] {
        [&self.config, &self.root, &self.work, &self.bench]
    }
}

/// A fixed fixture refusal or failure; every message is fixed text or a
/// command number, never command output, paths or configuration values.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    /// A named check failed.
    Check(&'static str),
    /// A command that had to succeed did not.
    Command(u32),
    /// A command outlived its own bound and was stopped.
    TimedOut(u32),
    /// A signal or the work deadline ended the attempt.
    Interrupted,
    /// The cleanup window has no room left for another command.
    Exhausted,
}
impl std::fmt::Display for Fault {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Check(message) => formatter.write_str(message),
            Self::Command(number) => {
                write!(
                    formatter,
                    "Command {number:03} failed; inspect private logs"
                )
            }
            Self::TimedOut(number) => write!(
                formatter,
                "Command {number:03} exceeded its time limit; inspect private logs"
            ),
            Self::Interrupted => formatter
                .write_str("Container release attempt interrupted or exceeded its work deadline"),
            Self::Exhausted => formatter.write_str("Fixture cleanup deadline exhausted"),
        }
    }
}
impl std::error::Error for Fault {}
pub(super) type Result<T> = std::result::Result<T, Fault>;

/// Refuse with `message` unless `condition` holds.
pub(super) fn require(condition: bool, message: &'static str) -> Result<()> {
    if condition {
        Ok(())
    } else {
        Err(Fault::Check(message))
    }
}

/// One command: its public operation name, its argument vector (the first
/// is the absolute program), its bounds and its input.
#[derive(Debug)]
pub(super) struct Request {
    pub operation: &'static str,
    pub argv: Vec<OsString>,
    pub timeout: Duration,
    pub grace: Duration,
    /// A regular file read as standard input.
    pub input: Option<PathBuf>,
    /// A descriptor the child inherits under its own number.
    pub inherit: Option<OwnedFd>,
    /// The working directory, when it is not the checkout.
    pub directory: Option<PathBuf>,
    /// Settings added to the fixed command environment.
    pub environment: Vec<(OsString, OsString)>,
    /// Whether anything but exit status 0 fails the fixture.
    pub check: bool,
}
impl Request {
    pub(super) fn new(operation: &'static str, argv: Vec<OsString>) -> Self {
        Self {
            operation,
            argv,
            timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            input: None,
            inherit: None,
            directory: None,
            environment: Vec::new(),
            check: true,
        }
    }
}

/// Why a runner stopped a command that had not ended by itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// Its own bound passed.
    Timeout,
    /// A signal or the work deadline ended the attempt.
    Interrupted,
    /// It could not be started at all.
    Unlaunched,
}

/// How one command ended, as its runner observed it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Execution {
    /// The exit status, or the negative signal number; `None` when the
    /// command was stopped or could not be reaped.
    pub status: Option<i32>,
    /// Its group was not proven settled: it was terminated, or a member
    /// could not be reaped, so work it started elsewhere may still run.
    pub stopped: bool,
    pub stop: Option<Stop>,
}

/// When a command may be stopped by something other than its own bound.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Window {
    /// Signals and this deadline end the attempt.
    Work(Instant),
    /// Only this deadline bounds it; signals are latched by the runner.
    Cleanup(Instant),
}

/// Executes one request with its standard streams on the two private files.
pub(super) trait Runner {
    async fn execute(
        &mut self,
        request: &mut Request,
        streams: [std::fs::File; 2],
        timeout: Duration,
        window: Window,
    ) -> Execution;
    fn now(&self) -> Instant;
    /// Wait between readiness probes; `Err` when the window ends it.
    async fn pause(&mut self, duration: Duration, window: Window) -> Result<()>;
    /// Consume a pending signal; it is also latched.
    async fn signalled(&mut self) -> bool;
    /// Whether any signal has arrived.
    fn latched(&self) -> bool;
    fn chown(&mut self, path: &Path, owner: u32) -> Result<()>;
    /// The network namespace of process `pid`, held open.
    fn namespace(&mut self, pid: i64) -> Result<OwnedFd>;
    /// Recheck the retained response-check executable at `path`.
    fn retained(&mut self, path: &Path) -> Result<()>;
    /// Refuse a host that is not a fresh disposable Linux host run as root.
    fn preflight(&mut self, paths: &Paths) -> Result<()>;
}

/// The retained private outcome of one command.
#[derive(Debug)]
pub(super) struct Outcome {
    pub status: Option<i32>,
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}
impl Outcome {
    pub(super) fn succeeded(&self) -> bool {
        self.status == Some(0)
    }
    /// The bounded UTF-8 stdout without surrounding whitespace.
    pub(super) fn text(&self) -> Result<String> {
        let bytes = files::read_bounded(&self.stdout, MAX_INSPECTION)
            .map_err(|_| Fault::Check("Inspection output exceeds its bound"))?;
        let text =
            String::from_utf8(bytes).map_err(|_| Fault::Check("Inspection output is not UTF-8"))?;
        Ok(text.trim().to_owned())
    }
}

/// The public name and status of the most recent command that failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Failed {
    pub number: u32,
    pub operation: &'static str,
    pub exit_status: i32,
}

/// The public name and bound of the most recent command that started.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Started {
    pub number: u32,
    pub operation: &'static str,
    pub timeout_seconds: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Record<'a> {
    command: Vec<String>,
    timeout_seconds: f64,
    exit_status: Option<i32>,
    seconds: f64,
    operation: &'a str,
}

/// The fixture's command owner: numbering, private logs and records, the
/// current window, and what the public report may name.
pub(super) struct Commands<R> {
    pub runner: R,
    directory: PathBuf,
    sequence: u32,
    window: Window,
    /// A stopped command may have left engine-side work running; cleanup
    /// then changes nothing.
    pub settlement_unconfirmed: bool,
    /// A signal or the work deadline interrupted the attempt.
    pub interrupted: bool,
    pub last_failure: Option<Failed>,
    pub last_started: Option<Started>,
}
impl<R: Runner> Commands<R> {
    pub(super) fn new(runner: R, directory: &Path, work: Instant) -> Self {
        Self {
            runner,
            directory: directory.to_path_buf(),
            sequence: 0,
            window: Window::Work(work),
            settlement_unconfirmed: false,
            interrupted: false,
            last_failure: None,
            last_started: None,
        }
    }
    /// Enter the cleanup window: from here signals no longer stop commands,
    /// and each command must fit, with its stop and reap tail, before `end`.
    pub(super) fn cleanup(&mut self, end: Instant) {
        self.window = Window::Cleanup(end);
    }
    pub(super) fn now(&self) -> Instant {
        self.runner.now()
    }
    /// Refuse new work once a signal arrived or the work window is over; in
    /// the cleanup window a signal is only latched.
    pub(super) async fn fence(&mut self) -> Result<()> {
        let signalled = self.runner.signalled().await;
        if let Window::Work(end) = self.window
            && (self.interrupted || signalled || self.runner.now() >= end)
        {
            self.interrupted = true;
            return Err(Fault::Interrupted);
        }
        Ok(())
    }
    pub(super) async fn pause(&mut self, duration: Duration) -> Result<()> {
        let paused = self.runner.pause(duration, self.window).await;
        if paused == Err(Fault::Interrupted) {
            self.interrupted = true;
        }
        paused
    }
    pub(super) fn chown(&mut self, path: &Path, owner: u32) -> Result<()> {
        self.runner.chown(path, owner)
    }
    fn admitted(&self, request: &Request) -> Result<Duration> {
        let Window::Cleanup(end) = self.window else {
            return Ok(request.timeout);
        };
        let left = end
            .checked_duration_since(self.runner.now())
            .and_then(|left| left.checked_sub(request.grace + REAP))
            .filter(|left| !left.is_zero())
            .ok_or(Fault::Exhausted)?;
        Ok(request.timeout.min(left))
    }
    /// Run `request` once: its streams go to new private files and its
    /// record beside them. A nonzero status is remembered by its fixed
    /// operation name; a checked request then fails with its number.
    pub(super) async fn run(&mut self, mut request: Request) -> Result<Outcome> {
        self.fence().await?;
        let timeout = self.admitted(&request)?;
        self.sequence += 1;
        let number = self.sequence;
        let prefix = self.directory.join(format!("command-{number:03}"));
        let (stdout, stderr) = (
            prefix.with_extension("stdout"),
            prefix.with_extension("stderr"),
        );
        let streams = [
            files::create_private(&stdout),
            files::create_private(&stderr),
        ];
        let [Ok(out), Ok(err)] = streams else {
            return Err(Fault::Check("Cannot create private command output"));
        };
        self.last_started = Some(Started {
            number,
            operation: request.operation,
            timeout_seconds: timeout.as_secs(),
        });
        let started = self.runner.now();
        let execution = self
            .runner
            .execute(&mut request, [out, err], timeout, self.window)
            .await;
        if execution.stopped {
            self.settlement_unconfirmed = true;
        }
        let record = Record {
            command: request
                .argv
                .iter()
                .map(|word| word.to_string_lossy().into_owned())
                .collect(),
            timeout_seconds: timeout.as_secs_f64(),
            exit_status: execution.status,
            seconds: rounded(self.runner.now().saturating_duration_since(started)),
            operation: request.operation,
        };
        let recorded = serde_json::to_vec_pretty(&record)
            .map_err(|_| Fault::Check("Cannot record a command"))
            .and_then(|mut bytes| {
                bytes.push(b'\n');
                files::write_new(&prefix.with_extension("json"), &bytes)
                    .map_err(|_| Fault::Check("Cannot record a command"))
            });
        match execution.stop {
            Some(Stop::Interrupted) => {
                self.interrupted = true;
                return Err(Fault::Interrupted);
            }
            Some(Stop::Timeout) => return Err(Fault::TimedOut(number)),
            Some(Stop::Unlaunched) => return Err(Fault::Check("Cannot start a fixture command")),
            None => {}
        }
        recorded?;
        let Some(status) = execution.status else {
            return Err(Fault::Check(
                "Owned command exited without an original status",
            ));
        };
        if status != 0 {
            self.last_failure = Some(Failed {
                number,
                operation: request.operation,
                exit_status: status,
            });
        }
        if status == 0
            && let Window::Cleanup(end) = self.window
            && self.runner.now() >= end
        {
            return Err(Fault::Exhausted);
        }
        if request.check && status != 0 {
            return Err(Fault::Command(number));
        }
        Ok(Outcome {
            status: execution.status,
            stdout,
            stderr,
        })
    }
    /// Run `request` and return its checked stdout text.
    pub(super) async fn text(&mut self, request: Request) -> Result<String> {
        self.run(request).await?.text()
    }
}

/// Seconds with millisecond resolution, as the report states durations.
pub(super) fn rounded(duration: Duration) -> f64 {
    (duration.as_secs_f64() * 1000.0).round() / 1000.0
}
