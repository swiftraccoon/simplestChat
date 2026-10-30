//! Closed routine-deployment roles over the existing process and log owners.
//! Paths and environments stay private. Snapshots are not descriptor execution
//! or exclusion of a same-user writer; the outer owner supplies source custody.
pub(in crate::ci) use crate::ci::ops_artifact::verify::tools::Captures;
use crate::{
    ci::{
        command::{Command, Program},
        release_checks::files::{self, Executable},
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::{OsStr, OsString},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

const STDERR: u64 = 65536;
fn failure(code: u8) -> Failure {
    Failure::new(code, "Routine deployment command is unproven.")
}
fn index(program: Program) -> Result<usize> {
    match program {
        Program::FrontendGit => Ok(0),
        Program::FrontendGh => Ok(1),
        Program::FrontendInventory => Ok(2),
        Program::FrontendAnsible => Ok(3),
        Program::FrontendCurl => Ok(4),
        _ => Err(failure(1)),
    }
}
/// The outer operation supplies its original source, inventory and evidence
/// witnesses. This is deliberately separate from executable validation and
/// must not recursively inspect Tools while its caller holds the state lock.
pub(in crate::ci) trait Custody: Send + Sync {
    fn check(&self) -> Result<()>;
}
pub(super) struct Spec {
    pub program: Program,
    pub arguments: Vec<OsString>,
    pub timeout: Duration,
    pub grace: Duration,
    pub maximum: u64,
    pub end: Instant,
    pub latest: Instant,
    pub custody: Arc<dyn Custody>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(super) struct ExitObservation {
    pub status: Option<i32>,
    pub settled: bool,
    pub body_completed: bool,
}
struct Context {
    root: PathBuf,
    environments: [Environment; 5],
    origin: Instant,
}
enum Received {
    Pending,
    Complete(Box<Captures>),
    Invalid,
}
pub(in crate::ci) struct Invocation {
    context: Arc<Context>,
    tools: Vec<Arc<Executable>>,
    spec: Spec,
    last: Mutex<Instant>,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Received>,
    exit: Mutex<Option<ExitObservation>>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrivateRoutineDeploymentInvocation")
    }
}
impl Invocation {
    fn custody(&self) -> Result<()> {
        let result = self.spec.custody.check();
        if result.is_err() {
            self.invalid.store(true, Ordering::SeqCst);
        }
        result
    }
    fn time(&self, dispatch: bool) -> Result<()> {
        let now = Instant::now();
        let mut last = self.last.lock().map_err(|_| failure(1))?;
        if now < self.context.origin || now < *last {
            return Err(failure(1));
        }
        *last = now;
        if now >= self.spec.end || dispatch && now > self.spec.latest {
            return Err(failure(124));
        }
        Ok(())
    }
    fn originals(&self) -> Result<&Path> {
        for tool in &self.tools {
            tool.validate()?;
        }
        self.tools
            .first()
            .ok_or_else(|| failure(1))?
            .selected_path()
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        if self.admitted.swap(true, Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        let valid = command.program == self.spec.program
            && command.arguments == self.spec.arguments
            && command.timeout == self.spec.timeout
            && command.grace == self.spec.grace
            && !command.cleanup
            && command.capture.is_none()
            && command.stdout.is_none()
            && command.stdin.is_none()
            && command.combined_log.is_none()
            && command.separate_logs.as_ref().is_some_and(|logs| {
                logs.stdout.maximum() == self.spec.maximum && logs.stderr.maximum() == STDERR
            })
            && command
                .frontend
                .as_ref()
                .is_some_and(|proof| std::ptr::eq(&**proof, self));
        if !valid {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        self.validate(command.program, &command.arguments)
            .map(|_| ())
    }
    pub(in crate::ci) fn validate(
        &self,
        program: Program,
        arguments: &[OsString],
    ) -> Result<&Path> {
        if program != self.spec.program
            || arguments != self.spec.arguments
            || self.invalid.load(Ordering::SeqCst)
        {
            return Err(failure(1));
        }
        self.time(true)?;
        self.custody()?;
        self.time(true)?;
        let selected = self.originals()?;
        self.custody()?;
        self.time(true)?;
        Ok(selected)
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        arguments: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, arguments)?;
        Ok((
            &self.context.root,
            &self.context.environments[index(program)?],
        ))
    }
    pub(in crate::ci) fn observed_exit(
        &self,
        status: Option<i32>,
        settled: bool,
        body_completed: bool,
    ) {
        match self.exit.lock() {
            Ok(mut value) if value.is_none() => {
                *value = Some(ExitObservation {
                    status,
                    settled,
                    body_completed,
                });
            }
            _ => {
                self.invalid.store(true, Ordering::SeqCst);
            }
        }
    }
    /// Passive terminal data remains readable after a failed deadline. It does
    /// not authenticate an application, prove remote settlement or grant retry.
    pub(super) fn exit(&self) -> Result<ExitObservation> {
        self.exit
            .lock()
            .map_err(|_| failure(1))?
            .clone()
            .ok_or_else(|| failure(1))
    }
    pub(in crate::ci) fn observed_io(&self, captures: Option<Captures>) {
        let regular = |seal: &crate::ci::ops_artifact::verify::tools::Seal| {
            seal.bytes == seal.metadata.len()
                && seal.metadata.is_file()
                && seal.metadata.nlink() == 1
                && seal.metadata.mode() & 0o7777 == 0o600
                && seal.metadata.uid() == rustix::process::getuid().as_raw()
        };
        let valid = captures.filter(|value| {
            regular(&value.stdout)
                && regular(&value.stderr)
                && value.stdout.bytes <= self.spec.maximum
                && value.stderr.bytes <= STDERR
                && (value.stdout.metadata.dev(), value.stdout.metadata.ino())
                    != (value.stderr.metadata.dev(), value.stderr.metadata.ino())
        });
        match self.captures.lock() {
            Ok(mut stored)
                if matches!(*stored, Received::Pending)
                    && self.admitted.load(Ordering::SeqCst)
                    && !self.invalid.load(Ordering::SeqCst) =>
            {
                *stored = match valid {
                    Some(value) => Received::Complete(Box::new(value)),
                    None => {
                        self.invalid.store(true, Ordering::SeqCst);
                        Received::Invalid
                    }
                };
            }
            _ => {
                self.invalid.store(true, Ordering::SeqCst);
            }
        }
    }
    pub(super) fn captures(&self) -> Result<Captures> {
        self.completed()?;
        let mut received = self.captures.lock().map_err(|_| failure(1))?;
        match std::mem::replace(&mut *received, Received::Invalid) {
            Received::Complete(value) => Ok(*value),
            _ => Err(failure(1)),
        }
    }
    pub(in crate::ci) fn completed(&self) -> Result<()> {
        if !self.admitted.load(Ordering::SeqCst) || self.invalid.load(Ordering::SeqCst) {
            return Err(failure(1));
        }
        self.time(false)?;
        self.custody()?;
        self.time(false)?;
        self.originals()?;
        self.custody()?;
        self.time(false)
    }
}

pub(super) struct Tools {
    tools: Vec<Arc<Executable>>,
    context: Arc<Context>,
}
fn environments(
    root: &Path,
    captured: &Environment,
    gh: &Path,
    ansible: &Path,
) -> Result<[Environment; 5]> {
    let gh_dir = gh.parent().ok_or_else(|| failure(1))?;
    let ansible_dir = ansible.parent().ok_or_else(|| failure(1))?;
    let path = std::env::join_paths([
        gh_dir,
        ansible_dir,
        Path::new("/usr/bin"),
        Path::new("/bin"),
        Path::new("/usr/sbin"),
        Path::new("/sbin"),
    ])
    .map_err(|_| failure(1))?;
    let mut base: Environment = [
        ("LC_ALL", "C"),
        ("LANG", "C"),
        ("TERM", "dumb"),
        ("NO_COLOR", "1"),
        ("PYTHONDONTWRITEBYTECODE", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v.into()))
    .collect();
    base.insert("PATH".into(), path);
    for key in ["HOME", "USER", "LOGNAME", "TMPDIR"] {
        if let Some(value) = captured.get(OsStr::new(key)) {
            base.insert(key.into(), value.clone());
        }
    }
    let mut all: [Environment; 5] = std::array::from_fn(|_| base.clone());
    all[0].insert("GIT_OPTIONAL_LOCKS".into(), "0".into());
    for role in [1, 3] {
        for key in [
            "GH_TOKEN",
            "GITHUB_TOKEN",
            "GH_CONFIG_DIR",
            "XDG_CONFIG_HOME",
        ] {
            if let Some(value) = captured.get(OsStr::new(key)) {
                all[role].insert(key.into(), value.clone());
            }
        }
        for key in [
            "GH_PROMPT_DISABLED",
            "GH_NO_UPDATE_NOTIFIER",
            "GH_NO_EXTENSION_UPDATE_NOTIFIER",
        ] {
            all[role].insert(key.into(), "1".into());
        }
        all[role].insert("GH_TELEMETRY".into(), "disabled".into());
    }
    all[3].insert(
        "ANSIBLE_CONFIG".into(),
        root.join("ops/ansible/ansible.cfg").into_os_string(),
    );
    all[3].insert("ANSIBLE_HOST_KEY_CHECKING".into(), "True".into());
    all[3].insert("ANSIBLE_RETRY_FILES_ENABLED".into(), "False".into());
    all[3].insert("ANSIBLE_NOCOLOR".into(), "True".into());
    // Both inventory resolution and the actual play use the same reviewed
    // configuration; credentials are retained only by the latter.
    for key in [
        "ANSIBLE_CONFIG",
        "ANSIBLE_HOST_KEY_CHECKING",
        "ANSIBLE_RETRY_FILES_ENABLED",
        "ANSIBLE_NOCOLOR",
    ] {
        let value = all[3][OsStr::new(key)].clone();
        all[2].insert(key.into(), value);
    }
    if let Some(value) = captured.get(OsStr::new("SSH_AUTH_SOCK")) {
        all[0].insert("SSH_AUTH_SOCK".into(), value.clone());
        all[3].insert("SSH_AUTH_SOCK".into(), value.clone());
    }
    Ok(all)
}
fn close_all(tools: Vec<Arc<Executable>>) -> Result<()> {
    let mut result = Ok(());
    for tool in tools {
        let closed = Arc::try_unwrap(tool)
            .map_err(|_| failure(1))
            .and_then(Executable::close);
        if result.is_ok() {
            result = closed;
        }
    }
    result
}
impl Tools {
    pub fn open(
        root: &Path,
        environment: &Environment,
        ansible: &Path,
        gh: &Path,
        current_xtask: &Path,
    ) -> Result<Self> {
        if !root.is_absolute()
            || !ansible.is_absolute()
            || !gh.is_absolute()
            || !current_xtask.is_absolute()
        {
            return Err(failure(1));
        }
        let inventory = ansible.with_file_name("ansible-inventory");
        let mut opened = Vec::new();
        let result = (|| {
            for (name, explicit) in [
                ("git", None),
                ("gh", Some(gh)),
                ("ansible-inventory", Some(inventory.as_path())),
                ("ansible-playbook", Some(ansible)),
                ("curl", None),
                ("xtask", Some(current_xtask)),
            ] {
                let executable = match explicit {
                    Some(path) => Executable::open(path)?,
                    None => files::select(root, environment, OsStr::new(name))?,
                };
                opened.push(Arc::new(executable));
            }
            let gh = opened[1].selected_path()?;
            if gh.file_name() != Some(OsStr::new("gh"))
                || opened[1].validate()?.file_name() != Some(OsStr::new("gh"))
            {
                return Err(failure(1));
            }
            environments(root, environment, gh, opened[3].selected_path()?)
        })();
        match result {
            Ok(environments) => Ok(Self {
                tools: opened,
                context: Arc::new(Context {
                    root: root.into(),
                    environments,
                    origin: Instant::now(),
                }),
            }),
            Err(error) => {
                let _ = close_all(opened);
                Err(error)
            }
        }
    }
    pub fn invocation(&self, spec: Spec) -> Result<Arc<Invocation>> {
        let role = index(spec.program)?;
        let maximum = if role == 3 {
            64 * 1024 * 1024
        } else {
            4 * 1024 * 1024
        };
        let now = Instant::now();
        if spec.timeout.is_zero()
            || spec.maximum == 0
            || spec.maximum > maximum
            || now >= spec.end
            || now > spec.latest
            || spec
                .latest
                .checked_add(spec.timeout)
                .and_then(|v| v.checked_add(spec.grace))
                .is_none_or(|v| v > spec.end)
        {
            return Err(failure(124));
        }
        for tool in &self.tools {
            tool.validate()?;
        }
        let mut tools = vec![self.tools[role].clone()];
        if role == 3 {
            tools.extend([self.tools[1].clone(), self.tools[5].clone()]);
        }
        let invocation = Arc::new(Invocation {
            context: self.context.clone(),
            tools,
            spec,
            last: Mutex::new(now),
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(Received::Pending),
            exit: Mutex::new(None),
        });
        invocation.time(true)?;
        Ok(invocation)
    }
    pub fn gh_path(&self) -> Result<&Path> {
        self.tools[1].selected_path()
    }
    pub fn ansible_path(&self) -> Result<&Path> {
        self.tools[3].selected_path()
    }
    pub fn inventory_path(&self) -> Result<&Path> {
        self.tools[2].selected_path()
    }
    pub fn xtask_path(&self) -> Result<&Path> {
        self.tools[5].selected_path()
    }
    pub fn finish(self) -> Result<()> {
        let mut checked = Ok(());
        for tool in &self.tools {
            let validation = tool.validate();
            if checked.is_ok() {
                checked = validation.map(|_| ());
            }
        }
        let closed = close_all(self.tools);
        checked.and(closed)
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
