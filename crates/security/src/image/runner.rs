//! The engine's commands as the image gate runs them: each leads a process
//! group of its own with a fixed environment, is bounded in time and in
//! what it prints, and is kept as its command line, a private log of both
//! of its streams and how it ended, numbered in the order of the commands.

use crate::{
    Fault, Result,
    context::{self, Failure},
    document::Document,
    files,
    image::{archive, sandbox::Engine},
    require,
};
use std::{
    ffi::{OsStr, OsString},
    fs::{File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

/// What a group that refused a signal is recorded as, as the reference
/// records the error.
const REFUSED: &str = "PermissionError: [Errno 1] Operation not permitted";

/// The bounds of the engine's commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Bounds {
    /// The most bytes a command may print to either of its streams.
    pub stream: u64,
    /// The most bytes of its log that are read back as its answer.
    pub inspected: u64,
    /// How long a command that is stopped has before it is killed.
    pub grace: Duration,
}

impl Bounds {
    /// The reviewed bounds.
    pub const REVIEWED: Self = Self {
        stream: 2 << 20,
        inspected: 2 << 20,
        grace: Duration::from_secs(15),
    };
}

/// The engine's commands, run for real.
#[derive(Clone, Debug)]
pub struct Runner {
    prefix: Vec<OsString>,
    environment: Vec<(String, OsString)>,
    root: PathBuf,
    logs: PathBuf,
    sequence: u32,
    bounds: Bounds,
}

/// The last step of the path `program`, as a log is named after it.
fn named(program: &OsStr) -> String {
    let name = Path::new(program).file_name();
    name.map_or_else(String::new, |name| name.to_string_lossy().into_owned())
}

/// `text` without the white space the reference trims from an answer: its
/// own, which takes in the four separators of files, groups, records and
/// units as well.
fn stripped(text: &str) -> &str {
    text.trim_matches(|character: char| {
        character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character)
    })
}

/// Publish `value` as the new private record `path`, sorted as the
/// reference writes it.
fn written(path: &Path, value: Document) -> Result<()> {
    archive::new_file(path, value.sorted().pretty().as_bytes())
}

impl Runner {
    /// A runner of the engine that `prefix` reaches, whose commands have
    /// exactly the environment `environment`, run in `root`, are logged in
    /// `logs` and are held to `bounds`.
    pub fn new(
        prefix: Vec<OsString>,
        environment: Vec<(String, OsString)>,
        root: &Path,
        logs: &Path,
        bounds: Bounds,
    ) -> Self {
        Self {
            prefix,
            environment,
            root: root.to_path_buf(),
            logs: logs.to_path_buf(),
            sequence: 0,
            bounds,
        }
    }

    /// How a command that began at `started` ended as `ended`, as its
    /// outcome is recorded.
    fn outcome(ended: &context::Ended, started: Instant) -> Document {
        let code = match ended.outcome {
            Err(Failure::Fixed(code)) => Some(code),
            _ => None,
        };
        let stopped = ended.stopped;
        let status = match ended.outcome {
            Ok(status) => Some(status),
            Err(_) => stopped.and_then(|stopped| stopped.status),
        };
        let denied = stopped.is_some_and(|stopped| stopped.denied);
        let cleanup = stopped
            .filter(|_| !denied)
            .map_or(Document::Null, |stopped| {
                Document::record([
                    ("groupSignalDenied", false.into()),
                    ("forcedKill", stopped.forced.into()),
                ])
            });
        let seconds = (started.elapsed().as_secs_f64() * 1000.0).round() / 1000.0;
        let whole = |value: i64| Document::Number(value.into());
        Document::record([
            (
                "pid",
                ended.pid.map_or(Document::Null, |pid| whole(pid.into())),
            ),
            (
                "exitStatus",
                status.map_or(Document::Null, |status| whole(status.into())),
            ),
            ("timedOut", (code == Some("command_timed_out")).into()),
            (
                "outputLimited",
                code.is_some_and(|code| code.contains("limit_exceeded"))
                    .into(),
            ),
            (
                "elapsedSeconds",
                serde_json::Number::from_f64(seconds).map_or(Document::Null, Document::Number),
            ),
            ("cleanup", cleanup),
            (
                "cleanupError",
                if denied {
                    REFUSED.into()
                } else {
                    Document::Null
                },
            ),
        ])
    }
}

impl Engine for Runner {
    fn name(&self) -> &str {
        self.prefix
            .first()
            .and_then(|program| Path::new(program).file_name())
            .and_then(OsStr::to_str)
            .unwrap_or_default()
    }

    fn log(&self) -> Option<PathBuf> {
        let program = self.prefix.first().map(|program| named(program));
        let name = format!(
            "{:02}-{}.log",
            self.sequence + 1,
            program.unwrap_or_default()
        );
        Some(self.logs.join(name))
    }

    fn command(
        &mut self,
        arguments: &[OsString],
        seconds: u64,
        failing: bool,
        healthy: &mut dyn FnMut() -> bool,
    ) -> Result<(i32, String)> {
        let argv: Vec<OsString> = self.prefix.iter().chain(arguments).cloned().collect();
        let (program, rest) = argv.split_first().ok_or(Fault("invalid_scanner_command"))?;
        self.sequence += 1;
        let name = format!("{:02}-{}", self.sequence, named(program));
        let path = self.logs.join(format!("{name}.log"));
        let mut options = OpenOptions::new();
        options.append(true).create_new(true).mode(0o600);
        let log: File = options
            .open(&path)
            .map_err(|error| files::unusable(&error))?;
        let shown = argv
            .iter()
            .map(|item| item.to_string_lossy().into_owned().into());
        let line = Document::record([
            ("argv", Document::List(shown.collect())),
            ("timeoutSeconds", seconds.into()),
        ]);
        written(&self.logs.join(format!("{name}.command.json")), line)?;
        let started = Instant::now();
        let mut command = Command::new(program);
        command
            .args(rest)
            .current_dir(&self.root)
            .env_clear()
            .envs(self.environment.iter().map(|(name, value)| (name, value)));
        let both = log.try_clone().map_err(|error| files::unusable(&error))?;
        let patience = (Duration::from_secs(seconds), self.bounds.grace);
        let room = (self.bounds.stream, self.bounds.stream);
        let ended = context::ended(&mut command, &[], patience, (log, both), room, healthy);
        let outcome = Self::outcome(&ended, started);
        let denied = ended.stopped.is_some_and(|stopped| stopped.denied);
        written(&self.logs.join(format!("{name}.outcome.json")), outcome)?;
        require(!denied, "image_engine_cleanup_uncertain")?;
        let status = ended.outcome.map_err(|failure| match failure {
            Failure::Fixed(code) | Failure::Command(code) | Failure::Prerequisite(code) => {
                Fault(code)
            }
        })?;
        let size = path
            .metadata()
            .map_err(|error| files::unusable(&error))?
            .len();
        require(
            size <= self.bounds.inspected,
            "image_engine_output_too_large",
        )?;
        let printed = std::fs::read(&path).map_err(|error| files::unusable(&error))?;
        require(status == 0 || failing, "image_engine_command_failed")?;
        let text = String::from_utf8(printed).map_err(|_| Fault("image_engine_output_not_text"))?;
        Ok((status, stripped(&text).to_owned()))
    }

    fn branch(&self, logs: &Path) -> Box<dyn Engine> {
        Box::new(Self {
            logs: logs.to_path_buf(),
            sequence: 0,
            ..self.clone()
        })
    }
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
