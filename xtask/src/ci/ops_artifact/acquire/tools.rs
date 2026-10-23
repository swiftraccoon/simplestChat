//! Five fixed gh calls. Only the original completed paired captures are readable.

use super::super::namespace::{Budget, Permit};
use super::{failure, plan, request::Request};
use crate::{
    ci::{
        command::{Command, Program},
        release_checks::files::Executable,
    },
    tooling::{Environment, Result},
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

pub(in crate::ci) use super::super::verify::tools::Captures;

struct Context {
    directory: PathBuf,
    environment: Environment,
    arguments: [Vec<OsString>; 5],
}
struct Dispatch {
    clock: Arc<dyn plan::Clock>,
    start: Instant,
    latest: Instant,
    last: Instant,
}
enum Received {
    Pending,
    Complete(Box<Captures>),
    Invalid,
}
pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    context: Arc<Context>,
    index: usize,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Received>,
    dispatch: Mutex<Option<Dispatch>>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpsArtifactAcquisitionInvocation")
    }
}
impl Invocation {
    fn dispatch_check(&self) -> Result<()> {
        let mut dispatch = self.dispatch.lock().map_err(|_| failure(1))?;
        let dispatch = dispatch.as_mut().ok_or_else(|| failure(1))?;
        let now = dispatch.clock.now();
        if now < dispatch.start || now < dispatch.last {
            return Err(failure(1));
        }
        if now > dispatch.latest {
            return Err(failure(124));
        }
        dispatch.last = now;
        Ok(())
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        if self.admitted.swap(true, Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        let valid = command.program == Program::OpsGh
            && command.arguments == self.context.arguments[self.index]
            && command.timeout == plan::CAPS[self.index]
            && command.grace == plan::GRACE
            && !command.cleanup
            && command.capture.is_none()
            && command.stdout.is_none()
            && command.combined_log.is_none()
            && command.stdin.is_none()
            && command.separate_logs.is_some()
            && command.separate_logs.as_ref().is_some_and(|logs| {
                logs.stdout.maximum()
                    == [4096, super::SMALL, super::SMALL, super::SMALL, super::ZIP][self.index]
                    && logs.stderr.maximum() == super::SMALL
            })
            && command
                .ops_acquire
                .as_ref()
                .is_some_and(|proof| std::ptr::eq(&**proof, self));
        if !valid {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        self.validate(command.program, &command.arguments)
            .map(|_| ())
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        if program != Program::OpsGh
            || args != self.context.arguments[self.index]
            || self.invalid.load(Ordering::SeqCst)
        {
            return Err(failure(1));
        }
        self.dispatch_check()?;
        let path = self.executable.selected_path().map_err(|_| failure(1))?;
        self.dispatch_check()?;
        Ok(path)
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        args: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, args)?;
        Ok((&self.context.directory, &self.context.environment))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        match self.captures.lock() {
            Ok(mut captures) => {
                if !matches!(*captures, Received::Pending)
                    || !self.admitted.load(Ordering::SeqCst)
                    || self.invalid.load(Ordering::SeqCst)
                {
                    *captures = Received::Invalid;
                    self.invalid.store(true, Ordering::SeqCst);
                } else if let Some(value) = value.filter(|value| {
                    value.stdout.bytes
                        <= [4096, super::SMALL, super::SMALL, super::SMALL, super::ZIP][self.index]
                        && value.stderr.bytes <= super::SMALL
                        && value.stdout.bytes == value.stdout.metadata.len()
                        && value.stderr.bytes == value.stderr.metadata.len()
                }) {
                    *captures = Received::Complete(Box::new(value));
                } else {
                    *captures = Received::Invalid;
                    self.invalid.store(true, Ordering::SeqCst);
                }
            }
            Err(_) => self.invalid.store(true, Ordering::SeqCst),
        }
    }
    pub(super) fn captures(&self) -> Result<Captures> {
        if self.invalid.load(Ordering::SeqCst) || !self.admitted.load(Ordering::SeqCst) {
            return Err(failure(1));
        }
        let mut capture = self.captures.lock().map_err(|_| failure(1))?;
        match std::mem::replace(&mut *capture, Received::Invalid) {
            Received::Complete(value) => Ok(*value),
            _ => Err(failure(1)),
        }
    }
    pub(super) fn schedule(&self, context: &plan::Context) -> Result<()> {
        context.command(self.index)?;
        let (start, latest) = context.dispatch_window()?;
        let mut dispatch = self.dispatch.lock().map_err(|_| failure(1))?;
        if dispatch.is_some() {
            return Err(failure(1));
        }
        *dispatch = Some(Dispatch {
            clock: context.clock.clone(),
            start,
            latest,
            last: start,
        });
        Ok(())
    }
}

pub(super) struct Tools {
    executable: Arc<Executable>,
    context: Arc<Context>,
    _permits: Vec<Permit>,
}
fn strings(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}
fn api(endpoint: &str) -> Vec<OsString> {
    let mut argv = strings(&[
        "api",
        "--hostname",
        "github.com",
        "--method",
        "GET",
        "-H",
        "Accept: application/vnd.github+json",
        "-H",
        "Accept-Encoding: identity",
        "-H",
        "X-GitHub-Api-Version: 2022-11-28",
    ]);
    argv.push(endpoint.into());
    argv
}
fn arguments(request: &Request) -> Result<[Vec<OsString>; 5]> {
    let identity = serde_json::to_value(request.selection.identity()).map_err(|_| failure(1))?;
    let run = identity["buildRunId"].as_u64().ok_or_else(|| failure(1))?;
    let attempt = identity["runAttempt"].as_u64().ok_or_else(|| failure(1))?;
    let repo = "repos/swiftraccoon/simplestChat";
    Ok([
        strings(&["--version"]),
        api(repo),
        api(&format!("{repo}/actions/runs/{run}/attempts/{attempt}")),
        api(&format!("{repo}/actions/artifacts/{}", request.artifact_id)),
        api(&format!(
            "{repo}/actions/artifacts/{}/zip",
            request.artifact_id
        )),
    ])
}
pub(super) fn environment(request: &Request, captured: &Environment) -> Result<Environment> {
    let mut env: Environment = [
        ("PATH", "/usr/bin:/bin"),
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value.into()))
    .collect();
    let work = request.output.join("work");
    for (key, directory) in [
        ("HOME", "home"),
        ("GH_CONFIG_DIR", "gh-config"),
        ("XDG_CONFIG_HOME", "config"),
        ("XDG_CACHE_HOME", "cache"),
        ("TMPDIR", "tmp"),
    ] {
        env.insert(key.into(), work.join(directory).into_os_string());
    }
    for key in [
        "GH_PROMPT_DISABLED",
        "GH_NO_UPDATE_NOTIFIER",
        "GH_NO_EXTENSION_UPDATE_NOTIFIER",
    ] {
        env.insert(key.into(), "1".into());
    }
    env.insert("GH_TELEMETRY".into(), "disabled".into());
    env.insert("GH_TOKEN".into(), Request::token(captured)?);
    Ok(env)
}
impl Tools {
    pub fn open(
        caller: &Path,
        captured: &Environment,
        request: &Request,
        budget: &Budget,
    ) -> Result<Self> {
        let context = Context {
            directory: request.output.clone(),
            environment: environment(request, captured)?,
            arguments: arguments(request)?,
        };
        let permits = budget.reserve(2)?;
        let executable =
            crate::ci::release_checks::files::select(caller, captured, OsStr::new("gh"))
                .map_err(|_| failure(1))?;
        Ok(Self {
            executable: Arc::new(executable),
            context: Arc::new(context),
            _permits: permits,
        })
    }
    pub fn invocation(&self, index: usize) -> Result<Arc<Invocation>> {
        if index >= plan::CAPS.len() {
            return Err(failure(1));
        }
        self.executable.validate().map_err(|_| failure(1))?;
        Ok(Arc::new(Invocation {
            executable: self.executable.clone(),
            context: self.context.clone(),
            index,
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(Received::Pending),
            dispatch: Mutex::new(None),
        }))
    }
    pub fn arguments(&self, index: usize) -> Result<Vec<OsString>> {
        self.context
            .arguments
            .get(index)
            .cloned()
            .ok_or_else(|| failure(1))
    }
    pub fn finish(self) -> Result<()> {
        let checked = self
            .executable
            .validate()
            .map(|_| ())
            .map_err(|_| failure(1));
        let closed = Arc::try_unwrap(self.executable)
            .map_err(|_| failure(1))?
            .close()
            .map_err(|_| failure(1));
        checked.and(closed)
    }
}
