//! Closed material-fetch, Debian-reader and compiler-resolver invocations.
//! Bootstrap executables are caller-trusted metadata/path observations; the
//! catalog authenticates component content, not the host's curl/dpkg runtime.

use super::{failure, plan};
use crate::{
    ci::{
        command::{Command, Program},
        ops_artifact::{
            namespace::{Budget, Permit},
            verify::tools::Captures,
        },
        release_checks::files::{self, Executable},
    },
    tooling::{Environment, Result},
};
use std::{
    ffi::{OsStr, OsString},
    io::Seek,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Curl,
    DpkgDeb,
    CcProbe,
    NativeShell,
    NativePerl,
    NativeMake,
}
impl Role {
    pub(super) fn program(self) -> Program {
        match self {
            Self::Curl => Program::OpsMaterialCurl,
            Self::DpkgDeb => Program::OpsDpkgDeb,
            Self::CcProbe => Program::OpsHostCc,
            Self::NativeShell => Program::OpsKitShell,
            Self::NativePerl => Program::OpsKitPerl,
            Self::NativeMake => Program::OpsKitMake,
        }
    }
}

pub(in crate::ci) struct Invocation {
    executable: Arc<Executable>,
    role: Role,
    directory: PathBuf,
    environment: Environment,
    arguments: Vec<OsString>,
    body: Duration,
    stdout_maximum: u64,
    input: Option<std::fs::Metadata>,
    clock: Arc<dyn plan::Clock>,
    start: Instant,
    latest: Instant,
    last: Mutex<Instant>,
    admitted: AtomicBool,
    invalid: AtomicBool,
    captures: Mutex<Option<Captures>>,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostedOpsMaterialInvocation")
    }
}
impl Invocation {
    pub(in crate::ci) fn begin(&self, request: &Command) -> Result<()> {
        if self.admitted.swap(true, Ordering::SeqCst)
            || request.capture.is_some()
            || request.stdout.is_some()
            || request.combined_log.is_some()
            || request.separate_logs.is_none()
            || request.cleanup
            || request.timeout != self.body
            || request.grace != Duration::from_secs(5)
            || request.separate_logs.as_ref().is_none_or(|logs| {
                logs.stdout.maximum() != self.stdout_maximum || logs.stderr.maximum() != 65536
            })
        {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        let input = match (&self.input, &request.stdin) {
            (None, None) => self.role != Role::DpkgDeb,
            (Some(expected), Some(file)) => {
                self.role == Role::DpkgDeb
                    && (&**file)
                        .stream_position()
                        .is_ok_and(|position| position == 0)
                    && file
                        .metadata()
                        .is_ok_and(|observed| super::super::namespace::same(expected, &observed))
            }
            _ => false,
        };
        if !input {
            self.invalid.store(true, Ordering::SeqCst);
            return Err(failure(1));
        }
        self.validate(request.program, &request.arguments)
            .map(|_| ())
    }
    fn ready(&self) -> Result<()> {
        let now = self.clock.now();
        let mut last = self.last.lock().map_err(|_| failure(1))?;
        if now < self.start || now < *last {
            return Err(failure(1));
        }
        *last = now;
        if now >= self.latest {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub(in crate::ci) fn validate(&self, program: Program, args: &[OsString]) -> Result<&Path> {
        let result = (|| {
            if program != self.role.program()
                || args != self.arguments
                || self.invalid.load(Ordering::SeqCst)
                || !self.admitted.load(Ordering::SeqCst)
            {
                return Err(failure(1));
            }
            self.ready()?;
            let path = self.executable.selected_path().map_err(|_| failure(1))?;
            self.ready()?;
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
        Ok((&self.directory, &self.environment))
    }
    pub(in crate::ci) fn observed_io(&self, value: Option<Captures>) {
        match self.captures.lock() {
            Ok(mut stored) => {
                if stored.is_some() || value.is_none() {
                    self.invalid.store(true, Ordering::SeqCst);
                } else {
                    *stored = value;
                }
            }
            Err(_) => self.invalid.store(true, Ordering::SeqCst),
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
}

pub(super) struct InvocationSpec {
    pub role: Role,
    pub args: Vec<OsString>,
    pub directory: PathBuf,
    pub environment: Environment,
    pub body: Duration,
    pub stdout_maximum: u64,
    pub input: Option<std::fs::Metadata>,
    pub clock: Arc<dyn plan::Clock>,
    pub latest: Instant,
}

pub(super) struct Tools {
    git: Option<Arc<Executable>>,
    curl: Option<Arc<Executable>>,
    dpkg: Option<Arc<Executable>>,
    cc: Option<Arc<Executable>>,
    native_shell: Option<Arc<Executable>>,
    native_perl: Option<Arc<Executable>>,
    native_make: Option<Arc<Executable>>,
    _permits: Vec<Permit>,
}

impl Tools {
    pub fn select(caller: &Path, environment: &Environment, budget: &Budget) -> Result<Self> {
        let mut value = Self {
            git: None,
            curl: None,
            dpkg: None,
            cc: None,
            native_shell: None,
            native_perl: None,
            native_make: None,
            _permits: budget.reserve(7)?,
        };
        let selected: Result<()> = (|| {
            value.git = Some(Arc::new(
                files::select(caller, environment, OsStr::new("git"))
                    .map_err(|e| failure(e.code))?,
            ));
            value.curl = Some(Arc::new(
                files::select(caller, environment, OsStr::new("curl"))
                    .map_err(|e| failure(e.code))?,
            ));
            value.dpkg = Some(Arc::new(
                files::select(caller, environment, OsStr::new("dpkg-deb"))
                    .map_err(|e| failure(e.code))?,
            ));
            Ok(())
        })();
        if let Err(error) = selected {
            let _ = value.finish();
            return Err(error);
        }
        Ok(value)
    }
    pub fn git(&self) -> Result<&Path> {
        self.git
            .as_ref()
            .ok_or_else(|| failure(1))?
            .validate()
            .map_err(|_| failure(1))
    }
    pub fn admit_cc(&mut self, kit: &Path) -> Result<()> {
        if self.cc.is_some() {
            return Err(failure(1));
        }
        self.cc = Some(Arc::new(
            Executable::open(&kit.join("host-tools/bin/cc")).map_err(|_| failure(1))?,
        ));
        Ok(())
    }
    pub fn admit_native(&mut self, kit: &Path) -> Result<()> {
        if self.native_shell.is_some() || self.native_perl.is_some() || self.native_make.is_some() {
            return Err(failure(1));
        }
        for (owner, name) in [
            (&mut self.native_shell, "dash"),
            (&mut self.native_perl, "perl"),
            (&mut self.native_make, "make"),
        ] {
            *owner = Some(Arc::new(
                Executable::open(&kit.join("host-tools/sysroot/usr/bin").join(name))
                    .map_err(|_| failure(1))?,
            ));
        }
        Ok(())
    }
    pub fn invocation(&self, spec: InvocationSpec) -> Result<Arc<Invocation>> {
        let InvocationSpec {
            role,
            args,
            directory,
            environment,
            body,
            stdout_maximum,
            input,
            clock,
            latest,
        } = spec;
        let executable = match role {
            Role::Curl => self.curl.as_ref(),
            Role::DpkgDeb => self.dpkg.as_ref(),
            Role::CcProbe => self.cc.as_ref(),
            Role::NativeShell => self.native_shell.as_ref(),
            Role::NativePerl => self.native_perl.as_ref(),
            Role::NativeMake => self.native_make.as_ref(),
        }
        .ok_or_else(|| failure(1))?
        .clone();
        executable.validate().map_err(|_| failure(1))?;
        let start = clock.now();
        if start >= latest
            || !super::super::namespace::path(&directory)
            || body.is_zero()
            || body
                > match role {
                    Role::NativeMake => Duration::from_secs(300),
                    Role::NativeShell | Role::NativePerl => Duration::from_secs(60),
                    _ => plan::DOWNLOAD,
                }
            || stdout_maximum == 0
            || stdout_maximum > super::files::MAX_FILE
        {
            return Err(failure(124));
        }
        Ok(Arc::new(Invocation {
            executable,
            role,
            directory,
            environment,
            arguments: args,
            body,
            stdout_maximum,
            input,
            clock,
            start,
            latest,
            last: Mutex::new(start),
            admitted: AtomicBool::new(false),
            invalid: AtomicBool::new(false),
            captures: Mutex::new(None),
        }))
    }
    pub fn finish(mut self) -> Result<()> {
        let mut result = Ok(());
        for owner in [
            &mut self.native_make,
            &mut self.native_perl,
            &mut self.native_shell,
            &mut self.cc,
            &mut self.dpkg,
            &mut self.curl,
            &mut self.git,
        ] {
            if let Some(owner) = owner.take() {
                let closed = Arc::try_unwrap(owner)
                    .map_err(|_| failure(1))
                    .and_then(|p| p.close().map_err(|_| failure(1)));
                result = result.and(closed);
            }
        }
        result
    }
}
