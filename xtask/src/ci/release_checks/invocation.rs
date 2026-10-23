//! The fixed coordinator retains its original phase-tail dispatch fence.

use super::{files::Executable, plan::Context};
use crate::{
    ci::command::{Command, Program},
    tooling::{Environment, Failure, Result},
};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    root: PathBuf,
    environment: Environment,
    program: Program,
    arguments: Vec<OsString>,
    capture: Option<u64>,
    timeout: Duration,
    grace: Duration,
    latest: Instant,
    used: AtomicBool,
    invalid: AtomicBool,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReleaseCheckPhase")
    }
}
fn exhausted() -> Failure {
    Failure::new(124, "Release-check budget exhausted.")
}
fn invalid() -> Failure {
    Failure::new(1, "Release-check invocation invalid.")
}
impl Invocation {
    pub(super) fn new(
        context: &Context,
        executable: Arc<Executable>,
        command: &Command,
        deadline: Instant,
        reserve: Duration,
    ) -> Result<Arc<Self>> {
        if command.program != Program::Python
            || command.timeout != Duration::from_secs(850)
            || command.grace != Duration::from_secs(200)
            || command.capture.is_some()
        {
            return Err(invalid());
        }
        let latest = deadline.checked_sub(reserve).ok_or_else(exhausted)?;
        if Instant::now() > latest {
            return Err(exhausted());
        }
        Ok(Arc::new(Self {
            executable,
            root: context.root.clone(),
            environment: context.environment.clone(),
            program: command.program,
            arguments: command.arguments.clone(),
            capture: command.capture,
            timeout: command.timeout,
            grace: command.grace,
            latest,
            used: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
        }))
    }
    pub(in crate::ci) fn begin(&self, command: &Command) -> Result<()> {
        if self.used.swap(true, Ordering::SeqCst)
            || command.program != self.program
            || command.arguments != self.arguments
            || command.capture != self.capture
            || command.timeout != self.timeout
            || command.grace != self.grace
            || command.cleanup
            || command.stdout.is_some()
            || command.stdin.is_some()
            || command.separate_logs.is_some()
            || command.combined_log.is_some()
        {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(invalid());
        }
        self.validate(command.program, &command.arguments)
            .map(|_| ())
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        if self.invalid.load(Ordering::SeqCst)
            || !self.used.load(Ordering::SeqCst)
            || program != self.program
            || args != self.arguments
        {
            return Err(invalid());
        }
        if Instant::now() > self.latest {
            return Err(exhausted());
        }
        let path = self.executable.validate()?;
        if Instant::now() > self.latest {
            return Err(exhausted());
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
}
