//! Closed selected Git/Python invocations, original source/input witnesses and
//! one-use completed paired captures. No child inherits GitHub credentials.

use super::super::namespace::{self, Budget, Permit};
pub(in crate::ci) use super::super::verify::tools::Captures;
use super::{
    failure,
    files::Output,
    plan::{self, Plan},
    sources::{self, Sources},
    worker::Checkpoint,
};
use crate::{
    ci::{
        command::{Command, Program},
        release_checks::files::{self, Executable},
    },
    tooling::{Environment, Result},
};
use std::{
    ffi::{OsStr, OsString},
    fs::Metadata,
    io::Seek,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

struct Context {
    directory: PathBuf,
    output: PathBuf,
    environment: Environment,
    sources: Arc<Sources>,
}
pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    context: Arc<Context>,
    arguments: Vec<OsString>,
    program: Program,
    body: Duration,
    grace: Duration,
    input: Option<Metadata>,
    clock: Arc<dyn plan::Clock>,
    start: Instant,
    latest: Instant,
    end: Instant,
    last: Mutex<Instant>,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Option<Captures>>,
    consumed: AtomicBool,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpsDebianRuntimeInvocation")
    }
}
impl Invocation {
    fn check_time(&self, dispatch: bool) -> Result<()> {
        let now = self.clock.now();
        let mut last = self.last.lock().map_err(|_| failure(1))?;
        if now < self.start || now < *last {
            return Err(failure(1));
        }
        *last = now;
        if now >= self.end || (dispatch && now > self.latest) {
            return Err(failure(124));
        }
        Ok(())
    }
    fn sources(&self) -> Result<()> {
        self.context.sources.check(&Checkpoint {
            clock: self.clock.clone(),
            start: self.start,
            end: self.end,
            abort: Arc::new(AtomicBool::new(false)),
        })
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        let valid = !self.admitted.swap(true, Ordering::SeqCst)
            && command.program == self.program
            && command.arguments == self.arguments
            && command.timeout == self.body
            && command.grace == self.grace
            && !command.cleanup
            && command.capture.is_none()
            && command.stdout.is_none()
            && command.combined_log.is_none()
            && command.separate_logs.as_ref().is_some_and(|logs| {
                logs.stdout.maximum() == super::CAPTURE && logs.stderr.maximum() == super::CAPTURE
            })
            && command
                .ops_runtime
                .as_ref()
                .is_some_and(|proof| std::ptr::eq(&**proof, self));
        let input = match (&self.input, &command.stdin) {
            (None, None) => self.program == Program::OpsRuntimeGit,
            (Some(expected), Some(file)) => {
                self.program == Program::OpsRuntimePython
                    && file
                        .metadata()
                        .is_ok_and(|stamp| namespace::same(expected, &stamp))
                    && rustix::fs::fcntl_getfl(&**file).is_ok_and(|flags| {
                        flags.intersection(rustix::fs::OFlags::ACCMODE)
                            == rustix::fs::OFlags::RDONLY
                    })
                    && (&**file).stream_position().is_ok_and(|offset| offset == 0)
            }
            _ => false,
        };
        if !valid || !input {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        self.validate(command.program, &command.arguments)
            .map(|_| ())
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        let result = (|| {
            if program != self.program
                || args != self.arguments
                || self.invalid.load(Ordering::SeqCst)
                || !self.admitted.load(Ordering::SeqCst)
            {
                return Err(failure(1));
            }
            self.check_time(true)?;
            self.sources()?;
            let path = self.executable.selected_path().map_err(|_| failure(1))?;
            self.check_time(true)?;
            Ok(path)
        })();
        if result.is_err() {
            self.invalid.store(true, Ordering::SeqCst);
        }
        result
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        args: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, args)?;
        let directory = if program == Program::OpsRuntimePython {
            self.context.sources.directory()
        } else {
            &self.context.directory
        };
        Ok((directory, &self.context.environment))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        match self.captures.lock() {
            Ok(mut stored)
                if stored.is_none()
                    && self.admitted.load(Ordering::SeqCst)
                    && !self.invalid.load(Ordering::SeqCst) =>
            {
                if let Some(value) = value.filter(|v| {
                    v.stdout.bytes <= super::CAPTURE
                        && v.stderr.bytes <= super::CAPTURE
                        && v.stdout.bytes == v.stdout.metadata.len()
                        && v.stderr.bytes == v.stderr.metadata.len()
                }) {
                    *stored = Some(value);
                } else {
                    self.invalid.store(true, Ordering::SeqCst);
                }
            }
            _ => self.invalid.store(true, Ordering::SeqCst),
        }
    }
    pub(super) fn captures(&self) -> Result<Captures> {
        if self.invalid.load(Ordering::SeqCst) || self.consumed.swap(true, Ordering::SeqCst) {
            return Err(failure(1));
        }
        self.captures
            .lock()
            .map_err(|_| failure(1))?
            .take()
            .ok_or_else(|| failure(1))
    }
    pub(super) fn settled(&self) -> Result<()> {
        let result = (|| {
            self.check_time(false)?;
            self.sources()?;
            self.executable.selected_path().map_err(|_| failure(1))?;
            self.check_time(false)
        })();
        if result.is_err() {
            self.invalid.store(true, Ordering::SeqCst);
        }
        result
    }
}

pub(super) struct Tools {
    git: Option<Arc<Executable>>,
    python: Option<Arc<Executable>>,
    context: Arc<Context>,
    _permits: Vec<Permit>,
}
fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
impl Tools {
    pub fn open(
        caller: &Path,
        output: &Output,
        environment: &Environment,
        budget: &Budget,
        checkpoint: &Checkpoint,
    ) -> Result<Self> {
        let permits = budget.reserve(4)?;
        let git = files::select(caller, environment, OsStr::new("git"))?;
        let python = match files::select(caller, environment, OsStr::new("python3")) {
            Ok(value) => value,
            Err(error) => {
                let _ = git.close();
                return Err(error);
            }
        };
        let selected = Sources::open(caller, output, budget, checkpoint);
        let selected = match selected {
            Ok(value) => value,
            Err(error) => {
                let _ = git.close();
                let _ = python.close();
                return Err(error);
            }
        };
        let path = environment
            .get(OsStr::new("PATH"))
            .cloned()
            .ok_or_else(|| failure(1));
        let path = match path {
            Ok(value) => value,
            Err(error) => {
                let _ = git.close();
                let _ = python.close();
                return Err(error);
            }
        };
        let mut child: Environment = [
            ("HOME", output.path.join("home").into_os_string()),
            ("TMPDIR", output.path.join("tmp").into_os_string()),
            ("PATH", path),
            ("LANG", "C.UTF-8".into()),
            ("LC_ALL", "C.UTF-8".into()),
            ("TZ", "UTC".into()),
            ("GIT_CONFIG_GLOBAL", "/dev/null".into()),
            ("GIT_CONFIG_SYSTEM", "/dev/null".into()),
            ("GIT_TERMINAL_PROMPT", "0".into()),
            ("GIT_OPTIONAL_LOCKS", "0".into()),
            ("GIT_LITERAL_PATHSPECS", "1".into()),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value))
        .collect();
        if let Some(value) = environment.get(OsStr::new("RUNNER_TRACKING_ID"))
            && value.to_str().is_some_and(|v| {
                !v.is_empty()
                    && v.len() <= 128
                    && v.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
            })
        {
            child.insert("RUNNER_TRACKING_ID".into(), value.clone());
        }
        Ok(Self {
            git: Some(Arc::new(git)),
            python: Some(Arc::new(python)),
            context: Arc::new(Context {
                directory: caller.into(),
                output: output.path.clone(),
                environment: child,
                sources: Arc::new(selected),
            }),
            _permits: permits,
        })
    }
    pub fn sources(&self) -> Arc<Sources> {
        self.context.sources.clone()
    }
    pub fn git_arguments(index: usize) -> Result<Vec<OsString>> {
        let mut args = strings(&[
            "--no-pager",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
        ]);
        match index {
            0 => args.extend(strings(&["rev-parse", "--verify", "HEAD"])),
            1 => args.extend(strings(&[
                "status",
                "--porcelain=v1",
                "--untracked-files=normal",
            ])),
            2 => {
                args.extend(strings(&["archive", "--format=tar", "HEAD", "--"]));
                args.extend(sources::NAMES.iter().map(OsString::from));
            }
            _ => return Err(failure(1)),
        }
        Ok(args)
    }
    pub fn git_invocation(
        &self,
        index: usize,
        plan: &Plan,
        end: Instant,
    ) -> Result<Arc<Invocation>> {
        self.invocation(Self::git_arguments(index)?, None, plan, end)
    }
    pub fn keeper_invocation(
        &self,
        pins: &super::receipt::Pins,
        input: Metadata,
        plan: &Plan,
        end: Instant,
    ) -> Result<Arc<Invocation>> {
        self.invocation(self.keeper_arguments(pins), Some(input), plan, end)
    }
    pub fn keeper_arguments(&self, pins: &super::receipt::Pins) -> Vec<OsString> {
        pins.arguments(self.context.sources.directory(), &self.context.output)
    }
    fn invocation(
        &self,
        args: Vec<OsString>,
        input: Option<Metadata>,
        plan: &Plan,
        end: Instant,
    ) -> Result<Arc<Invocation>> {
        let keeper = input.is_some();
        if !keeper && !(0..3).any(|i| Self::git_arguments(i).is_ok_and(|known| known == args)) {
            return Err(failure(1));
        }
        if args.len() > 64
            || args
                .iter()
                .map(|s| s.as_encoded_bytes().len())
                .sum::<usize>()
                > 24 * 1024
        {
            return Err(failure(1));
        }
        let body = if keeper {
            plan::KEEPER
        } else {
            Duration::from_secs(5)
        };
        let grace = if keeper {
            plan::GRACE
        } else {
            Duration::from_secs(5)
        };
        let latest = plan.dispatch(end, body, grace)?;
        let start = plan.now()?;
        let executable = if keeper {
            self.python.as_ref()
        } else {
            self.git.as_ref()
        }
        .ok_or_else(|| failure(1))?
        .clone();
        Ok(Arc::new(Invocation {
            executable,
            context: self.context.clone(),
            arguments: args,
            program: if keeper {
                Program::OpsRuntimePython
            } else {
                Program::OpsRuntimeGit
            },
            body,
            grace,
            input,
            clock: plan.clock.clone(),
            start,
            latest,
            end,
            last: Mutex::new(start),
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(None),
            consumed: AtomicBool::new(false),
        }))
    }
    pub fn finish(mut self) -> Result<()> {
        let mut result = Ok(());
        for value in [self.python.take(), self.git.take()].into_iter().flatten() {
            let closed = Arc::try_unwrap(value)
                .map_err(|_| failure(1))
                .and_then(Executable::close);
            result = result.and(closed);
        }
        result
    }
}
