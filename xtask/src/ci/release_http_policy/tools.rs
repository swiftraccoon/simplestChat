//! Exact four Docker inspections, with one-use pre-spawn scheduling capabilities.

use super::{Result, fail, plan::Time};
use crate::{
    ci::{
        command::{Command, Program},
        ops_artifact::verify::tools::Captures,
        release_checks::files::Executable,
    },
    tooling::Environment,
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(super) const FORMAT: &str = concat!(
    "{\"id\":{{json .Id}},\"running\":{{json .State.Running}},",
    "\"labels\":{{json .Config.Labels}},\"ports\":{{json .NetworkSettings.Ports}}}"
);
pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    root: PathBuf,
    environment: Environment,
    arguments: Vec<OsString>,
    program: Program,
    time: Time,
    latest: Instant,
    used: AtomicBool,
    invalid: AtomicBool,
    observed: AtomicBool,
    seals: Mutex<Option<Captures>>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReleaseHttpInspection")
    }
}
impl Invocation {
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        if self.used.swap(true, Ordering::SeqCst)
            || command.program != self.program
            || command.arguments != self.arguments
            || command.timeout != Duration::from_secs(5)
            || command.grace != Duration::from_secs(1)
            || command.cleanup
            || command.capture.is_some()
            || command.stdout.is_some()
            || command.combined_log.is_some()
            || (command.stdin.is_some() != (self.program == Program::Python))
            || command
                .separate_logs
                .as_ref()
                .is_none_or(|logs| logs.stdout.maximum() != 65536 || logs.stderr.maximum() != 4096)
        {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(fail(1, "fixture_container_inspection"));
        }
        self.validate(command.program, &command.arguments)
            .map(|_| ())
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        if !self.used.load(Ordering::SeqCst)
            || self.invalid.load(Ordering::SeqCst)
            || program != self.program
            || args != self.arguments
        {
            return Err(fail(1, "fixture_container_inspection"));
        }
        if self.time.now()? > self.latest {
            return Err(fail(124, "http_policy_deadline"));
        }
        let path = self
            .executable
            .selected_path()
            .map_err(|_| fail(1, "fixture_container_inspection"))?;
        if self.time.now()? > self.latest {
            return Err(fail(124, "http_policy_deadline"));
        }
        Ok(path)
    }
    pub(in crate::ci) fn context(
        &self,
        p: Program,
        a: &[OsString],
    ) -> Result<(&Path, &Environment)> {
        self.validate(p, a)?;
        Ok((&self.root, &self.environment))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        if self.observed.swap(true, Ordering::SeqCst) || !self.used.load(Ordering::SeqCst) {
            self.invalid.store(true, Ordering::SeqCst);
        }
        if let Ok(mut seals) = self.seals.lock() {
            *seals = value;
        } else {
            self.invalid.store(true, Ordering::SeqCst);
        }
    }
    pub(super) fn seals(&self) -> Result<Captures> {
        if self.invalid.load(Ordering::SeqCst) || !self.observed.load(Ordering::SeqCst) {
            return Err(fail(1, "fixture_container_inspection"));
        }
        self.seals
            .lock()
            .map_err(|_| fail(1, "fixture_container_inspection"))?
            .take()
            .ok_or_else(|| fail(1, "fixture_container_inspection"))
    }
}
pub(super) struct Tools {
    executable: Arc<Executable>,
}
impl Tools {
    #[cfg(test)]
    pub fn selected(executable: Executable) -> Self {
        Self {
            executable: Arc::new(executable),
        }
    }
    pub fn open(path: &Path) -> Result<Self> {
        Ok(Self {
            executable: Arc::new(
                Executable::open(path).map_err(|_| fail(1, "fixture_container_inspection"))?,
            ),
        })
    }
    pub fn command(
        &self,
        id: &str,
        time: &Time,
        index: usize,
    ) -> Result<(Command, Arc<Invocation>)> {
        let latest = time.docker(index)?;
        let mut command = Command::capture(
            Program::Docker,
            &[
                "--host",
                "unix:///var/run/docker.sock",
                "inspect",
                "--format",
                FORMAT,
                id,
            ],
        );
        command.timeout = Duration::from_secs(5);
        command.grace = Duration::from_secs(1);
        let proof = Arc::new(Invocation {
            executable: self.executable.clone(),
            root: PathBuf::from("/"),
            environment: [
                ("PATH".into(), "/usr/sbin:/usr/bin:/sbin:/bin".into()),
                ("LC_ALL".into(), "C".into()),
            ]
            .into_iter()
            .collect(),
            arguments: command.arguments.clone(),
            program: Program::Docker,
            time: time.clone(),
            latest,
            used: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            observed: AtomicBool::new(false),
            seals: Mutex::new(None),
        });
        command.release_http = Some(proof.clone());
        Ok((command, proof))
    }
    #[cfg(test)]
    pub fn oracle(&self, time: &Time) -> Result<(Command, Arc<Invocation>)> {
        let mut command = Command::capture(Program::Python, &["-I", "-B"]);
        command.arguments.push(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("src/ci/release_http_policy/oracle.py")
                .into_os_string(),
        );
        command.timeout = Duration::from_secs(5);
        command.grace = Duration::from_secs(1);
        let proof = Arc::new(Invocation {
            executable: self.executable.clone(),
            root: PathBuf::from("/"),
            environment: [
                ("PATH".into(), "/usr/bin:/bin".into()),
                ("LC_ALL".into(), "C".into()),
            ]
            .into_iter()
            .collect(),
            arguments: command.arguments.clone(),
            program: Program::Python,
            time: time.clone(),
            latest: time.docker(0)?,
            used: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            observed: AtomicBool::new(false),
            seals: Mutex::new(None),
        });
        command.release_http = Some(proof.clone());
        Ok((command, proof))
    }
    pub fn finish(self) -> Result<()> {
        let executable =
            Arc::try_unwrap(self.executable).map_err(|_| fail(1, "http_policy_close"))?;
        let checked = executable
            .validate()
            .map(|_| ())
            .map_err(|_| fail(1, "http_policy_close"));
        let closed = executable.close().map_err(|_| fail(1, "http_policy_close"));
        checked.and(closed)
    }
}
