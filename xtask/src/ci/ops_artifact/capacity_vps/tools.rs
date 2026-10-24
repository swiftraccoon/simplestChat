//! Closed commands bind retained tools, source, trust files, original input and
//! completed paired captures. A parsed report cannot construct this capability.

use super::super::namespace::{self, Budget, Permit};
pub(in crate::ci) use super::super::verify::tools::Captures;
use super::{Result, failure, inventory::Inventory, plan, sources::Sources, worker::Checkpoint};
use crate::{
    ci::{
        command::{Command, Program},
        release_checks::files::{self, Executable},
    },
    tooling::Environment,
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
    caller: PathBuf,
    environment: Environment,
    sources: Arc<Sources>,
    inventory: Arc<Inventory>,
    artifact: Option<Arc<Artifact>>,
    budget: Budget,
    #[cfg(test)]
    inert_fixture: bool,
}
struct Artifact {
    acquired: Arc<super::super::acquire::AuthenticatedAcquisition>,
    transfer: super::files::Original,
}
pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    context: Arc<Context>,
    program: Program,
    args: Vec<OsString>,
    body: Duration,
    grace: Duration,
    maximum: u64,
    input: Option<Metadata>,
    clock: Arc<dyn plan::Clock>,
    origin: Instant,
    start: Instant,
    latest: Instant,
    end: Instant,
    last: Mutex<Instant>,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Option<Captures>>,
    exit: Mutex<Option<ExitObservation>>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub(in crate::ci) struct ExitObservation {
    pub status: Option<i32>,
    pub settled: bool,
}
pub(super) struct Spec {
    pub program: Program,
    pub args: Vec<OsString>,
    pub body: Duration,
    pub grace: Duration,
    pub maximum: u64,
    pub input: Option<Metadata>,
    pub end: Instant,
    pub latest: Instant,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateVpsInvocation")
    }
}
impl Invocation {
    pub(in crate::ci) fn observed_exit(&self, status: Option<i32>, settled: bool) {
        match self.exit.lock() {
            Ok(mut stored) if stored.is_none() => {
                *stored = Some(ExitObservation { status, settled })
            }
            _ => self.invalid.store(true, Ordering::SeqCst),
        }
    }
    pub(super) fn exit(&self) -> Result<ExitObservation> {
        self.exit
            .lock()
            .map_err(|_| failure(1))?
            .clone()
            .ok_or_else(|| failure(1))
    }
    fn time(&self, dispatch: bool) -> Result<()> {
        let now = self.clock.now();
        let mut last = self.last.lock().map_err(|_| failure(1))?;
        if now < self.start || now < *last {
            return Err(failure(1));
        }
        *last = now;
        if now >= self.end || (dispatch && now > self.latest) {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    fn originals(&self) -> Result<()> {
        let checkpoint = Checkpoint {
            clock: self.clock.clone(),
            start: self.origin,
            end: self.end,
            abort: Arc::new(AtomicBool::new(false)),
        };
        self.context.sources.check(&checkpoint)?;
        self.context.inventory.check(&checkpoint)?;
        if let Some(artifact) = &self.context.artifact {
            artifact
                .acquired
                .check(&self.context.budget, &checkpoint.acquired())?;
            artifact.transfer.check(&self.context.budget, &checkpoint)?;
        } else if self.program != Program::OpsVpsGit {
            #[cfg(test)]
            if self.context.inert_fixture {
                return Ok(());
            }
            return Err(failure(1));
        }
        Ok(())
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        let valid = !self.admitted.swap(true, Ordering::SeqCst)
            && command.program == self.program
            && command.arguments == self.args
            && command.timeout == self.body
            && command.grace == self.grace
            && !command.cleanup
            && command.capture.is_none()
            && command.stdout.is_none()
            && command.combined_log.is_none()
            && command.separate_logs.as_ref().is_some_and(|logs| {
                logs.stdout.maximum() == self.maximum && logs.stderr.maximum() == super::MAX_STDERR
            })
            && command
                .ops_vps
                .as_ref()
                .is_some_and(|p| std::ptr::eq(&**p, self));
        let input = match (&self.input, &command.stdin) {
            (None, None) => self.program != Program::OpsVpsSsh,
            (Some(expected), Some(file)) => {
                self.program == Program::OpsVpsSsh
                    && file.metadata().is_ok_and(|m| namespace::same(expected, &m))
                    && (&**file).stream_position().is_ok_and(|offset| offset == 0)
                    && rustix::fs::fcntl_getfl(&**file).is_ok_and(|f| {
                        f.intersection(rustix::fs::OFlags::ACCMODE) == rustix::fs::OFlags::RDONLY
                    })
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
        let checked = (|| {
            if program != self.program
                || args != self.args
                || self.invalid.load(Ordering::SeqCst)
                || !self.admitted.load(Ordering::SeqCst)
            {
                return Err(failure(1));
            }
            self.time(true)?;
            self.originals()?;
            let selected = self.executable.selected_path().map_err(|_| failure(1))?;
            self.time(true)?;
            Ok(selected)
        })();
        if checked.is_err() {
            self.invalid.store(true, Ordering::SeqCst);
        }
        checked
    }
    pub(in crate::ci) fn context(
        &self,
        program: Program,
        args: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(program, args)?;
        Ok((
            if program == Program::OpsVpsAnsible {
                self.context.sources.directory()
            } else {
                &self.context.caller
            },
            &self.context.environment,
        ))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        match self.captures.lock() {
            Ok(mut current)
                if current.is_none()
                    && self.admitted.load(Ordering::SeqCst)
                    && !self.invalid.load(Ordering::SeqCst) =>
            {
                if let Some(value) = value.filter(|v| {
                    v.stdout.bytes <= self.maximum
                        && v.stderr.bytes <= super::MAX_STDERR
                        && v.stdout.bytes == v.stdout.metadata.len()
                        && v.stderr.bytes == v.stderr.metadata.len()
                }) {
                    *current = Some(value);
                } else {
                    self.invalid.store(true, Ordering::SeqCst);
                }
            }
            _ => self.invalid.store(true, Ordering::SeqCst),
        }
    }
    pub(super) fn captures(&self) -> Result<Captures> {
        if self.invalid.load(Ordering::SeqCst) || !self.admitted.load(Ordering::SeqCst) {
            return Err(failure(1));
        }
        self.captures
            .lock()
            .map_err(|_| failure(1))?
            .clone()
            .ok_or_else(|| failure(1))
    }
    pub(in crate::ci) fn settled(&self) -> Result<()> {
        self.time(false)?;
        self.originals()?;
        self.executable.selected_path().map_err(|_| failure(1))?;
        self.time(false)
    }
}

pub(super) struct Tools {
    git: Option<Arc<Executable>>,
    ssh: Option<Arc<Executable>>,
    ansible: Option<Arc<Executable>>,
    caller: PathBuf,
    home: PathBuf,
    source: Arc<Sources>,
    inventory: Arc<Inventory>,
    artifact: Option<Arc<Artifact>>,
    budget: Budget,
    #[cfg(test)]
    inert_fixture: bool,
    _permits: Vec<Permit>,
}
impl Tools {
    #[cfg(test)]
    pub(super) fn fixture(
        caller: &Path,
        home: &Path,
        sources: Arc<Sources>,
        inventory: Arc<Inventory>,
        budget: &Budget,
        paths: [&Path; 3],
    ) -> Result<Self> {
        let permits = budget.reserve(6)?;
        let mut opened = Vec::new();
        for path in paths {
            match Executable::open(path) {
                Ok(file) => opened.push(Arc::new(file)),
                Err(error) => {
                    for file in opened {
                        if let Ok(file) = Arc::try_unwrap(file) {
                            let _ = file.close();
                        }
                    }
                    return Err(error);
                }
            }
        }
        let ansible = opened.pop();
        let ssh = opened.pop();
        let git = opened.pop();
        Ok(Self {
            git,
            ssh,
            ansible,
            caller: caller.into(),
            home: home.into(),
            source: sources,
            inventory,
            artifact: None,
            budget: budget.clone(),
            inert_fixture: true,
            _permits: permits,
        })
    }
    pub fn open(
        caller: &Path,
        home: &Path,
        environment: &Environment,
        sources: Arc<Sources>,
        inventory: Arc<Inventory>,
        prepare: bool,
        budget: &Budget,
    ) -> Result<Self> {
        let permits = budget.reserve(6)?;
        let git = files::select(caller, environment, OsStr::new("git"))?;
        let ssh = match Executable::open(Path::new("/usr/bin/ssh")) {
            Ok(v) => v,
            Err(e) => {
                let _ = git.close();
                return Err(e);
            }
        };
        let ansible = if prepare {
            match Executable::open(&caller.join("ops/ansible/.venv/bin/ansible-playbook")) {
                Ok(v) => Some(Arc::new(v)),
                Err(e) => {
                    let _ = git.close();
                    let _ = ssh.close();
                    return Err(e);
                }
            }
        } else {
            None
        };
        Ok(Self {
            git: Some(Arc::new(git)),
            ssh: Some(Arc::new(ssh)),
            ansible,
            caller: caller.into(),
            home: home.into(),
            source: sources,
            inventory,
            artifact: None,
            budget: budget.clone(),
            #[cfg(test)]
            inert_fixture: false,
            _permits: permits,
        })
    }
    pub fn bind_artifact(
        &mut self,
        acquired: Arc<super::super::acquire::AuthenticatedAcquisition>,
        transfer: super::files::Original,
    ) -> Result<()> {
        if self.artifact.is_some() {
            return Err(failure(1));
        }
        self.artifact = Some(Arc::new(Artifact { acquired, transfer }));
        Ok(())
    }
    fn environment(&self, program: Program) -> Environment {
        let mut env: Environment = [
            ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
            ("LC_ALL", "C"),
            ("LANG", "C"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect();
        env.insert("HOME".into(), self.home.as_os_str().to_owned());
        if program == Program::OpsVpsGit {
            for (k, v) in [
                ("GIT_CONFIG_SYSTEM", "/dev/null"),
                ("GIT_CONFIG_GLOBAL", "/dev/null"),
                ("GIT_CONFIG_NOSYSTEM", "1"),
                ("GIT_CONFIG_COUNT", "0"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("GIT_NO_REPLACE_OBJECTS", "1"),
                ("GIT_ATTR_NOSYSTEM", "1"),
                ("GIT_OPTIONAL_LOCKS", "0"),
            ] {
                env.insert(k.into(), v.into());
            }
        }
        if program == Program::OpsVpsAnsible {
            env.insert("LC_ALL".into(), "C.UTF-8".into());
            env.insert("LANG".into(), "C.UTF-8".into());
            env.insert(
                "ANSIBLE_CONFIG".into(),
                self.source
                    .directory()
                    .join("ops/ansible/ansible.cfg")
                    .into_os_string(),
            );
            env.insert("ANSIBLE_HOST_KEY_CHECKING".into(), "True".into());
            env.insert("ANSIBLE_RETRY_FILES_ENABLED".into(), "False".into());
            env.insert(
                "ANSIBLE_SSH_ARGS".into(),
                self.inventory.target.common().into(),
            );
            env.insert("PYTHONDONTWRITEBYTECODE".into(), "1".into());
        }
        env
    }
    pub fn invocation(&self, spec: Spec, plan: &plan::Plan) -> Result<Arc<Invocation>> {
        let Spec {
            program,
            args,
            body,
            grace,
            maximum,
            input,
            end,
            latest,
        } = spec;
        let selected = match program {
            Program::OpsVpsGit => &self.git,
            Program::OpsVpsSsh => &self.ssh,
            Program::OpsVpsAnsible => &self.ansible,
            _ => return Err(failure(1)),
        };
        let executable = selected.as_ref().ok_or_else(|| failure(1))?.clone();
        let start = plan.now()?;
        if latest < start || end <= latest || maximum > super::MAX_ARCHIVE {
            return Err(failure(124));
        }
        Ok(Arc::new(Invocation {
            executable,
            context: Arc::new(Context {
                caller: self.caller.clone(),
                environment: self.environment(program),
                sources: self.source.clone(),
                inventory: self.inventory.clone(),
                artifact: self.artifact.clone(),
                budget: self.budget.clone(),
                #[cfg(test)]
                inert_fixture: self.inert_fixture,
            }),
            program,
            args,
            body,
            grace,
            maximum,
            input,
            clock: plan.clock.clone(),
            origin: plan.start,
            start,
            latest,
            end,
            last: Mutex::new(start),
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(None),
            exit: Mutex::new(None),
        }))
    }
    pub fn finish(mut self) -> Result<()> {
        let mut primary = None;
        for tool in [&mut self.ansible, &mut self.ssh, &mut self.git] {
            if let Some(tool) = tool.take() {
                match Arc::try_unwrap(tool) {
                    Ok(tool) => {
                        if let Err(e) = tool.close() {
                            super::keep(&mut primary, e);
                        }
                    }
                    Err(_) => super::keep(&mut primary, failure(1)),
                }
            }
        }
        primary.map_or(Ok(()), Err)
    }
}
