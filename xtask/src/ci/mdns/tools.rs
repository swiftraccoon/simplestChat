//! Caller-trusted fixed tool metadata, not executable-byte or SDK authentication.
use super::failure;
use crate::{
    ci::{command::Program, release_checks::files::Executable},
    tooling::Result,
};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::ci) enum Role {
    Interfaces,
    Plist,
    Launch,
    Logs,
    Compiler,
    Probe,
}
impl Role {
    pub fn program(self) -> Program {
        match self {
            Self::Interfaces => Program::MdnsInterfaces,
            Self::Plist => Program::MdnsPlist,
            Self::Launch => Program::MdnsLaunch,
            Self::Logs => Program::MdnsLogs,
            Self::Compiler => Program::MdnsCompiler,
            Self::Probe => Program::MdnsProbe,
        }
    }
}
pub(super) struct Paths(pub BTreeMap<Role, PathBuf>);
impl Paths {
    pub fn native() -> Self {
        Self(BTreeMap::from([
            (Role::Interfaces, "/sbin/ifconfig".into()),
            (Role::Plist, "/usr/bin/plutil".into()),
            (Role::Launch, "/bin/launchctl".into()),
            (Role::Logs, "/usr/bin/log".into()),
            (Role::Compiler, "/usr/bin/cc".into()),
        ]))
    }
}
#[derive(Clone, Copy, Default)]
pub(in crate::ci) struct Evidence {
    pub settled: bool,
    pub exit: Option<i32>,
    pub timed_out: bool,
    pub interrupted: Option<u8>,
    pub group_closed: bool,
    pub io_closed: bool,
}
pub(in crate::ci) struct Invocation {
    role: Role,
    executable: Arc<Executable>,
    evidence: Mutex<Evidence>,
    admitted: std::sync::atomic::AtomicBool,
    invalid: std::sync::atomic::AtomicBool,
}
impl std::fmt::Debug for Invocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SelectedMdnsTool")
    }
}
impl Invocation {
    pub(in crate::ci) fn begin(&self) -> Result<()> {
        if self
            .admitted
            .swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            self.invalid
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(failure(1));
        }
        Ok(())
    }
    pub(in crate::ci) fn outcome(
        &self,
        exit: Option<i32>,
        timed_out: bool,
        interrupted: Option<u8>,
        group_closed: bool,
    ) {
        if let Ok(mut v) = self.evidence.lock() {
            v.settled = true;
            v.exit = exit;
            v.timed_out = timed_out;
            v.interrupted = interrupted;
            v.group_closed = group_closed;
        }
    }
    pub(in crate::ci) fn closed(&self, passed: bool) {
        if let Ok(mut v) = self.evidence.lock() {
            v.io_closed = passed;
        }
    }
    pub(in crate::ci) fn observed(&self) -> Result<Evidence> {
        if self.invalid.load(std::sync::atomic::Ordering::SeqCst) {
            return Err(failure(1));
        }
        self.evidence.lock().map(|v| *v).map_err(|_| failure(1))
    }
    pub(in crate::ci) fn validate(&self, program: Program) -> Result<&Path> {
        if program != self.role.program() {
            return Err(failure(1));
        }
        // Compiler invocation-path semantics are retained through the selected
        // absolute path; validation still binds its canonical target.
        self.executable.selected_path().map_err(|_| failure(1))
    }
}

#[cfg(test)]
pub(in crate::ci) struct FixtureTool {
    pub invocation: Arc<Invocation>,
    executable: Arc<Executable>,
}
#[cfg(test)]
impl FixtureTool {
    pub fn new(role: Role, path: &Path) -> Result<Self> {
        let executable = Arc::new(Executable::open(path)?);
        let invocation = Arc::new(Invocation {
            role,
            executable: executable.clone(),
            evidence: Mutex::default(),
            admitted: std::sync::atomic::AtomicBool::new(false),
            invalid: std::sync::atomic::AtomicBool::new(false),
        });
        Ok(Self {
            invocation,
            executable,
        })
    }
    pub fn finish(self) -> Result<()> {
        drop(self.invocation);
        Arc::try_unwrap(self.executable)
            .map_err(|_| failure(1))?
            .close()
    }
}
#[derive(Default)]
pub(super) struct Tools(BTreeMap<Role, Arc<Executable>>);
impl Tools {
    pub fn admit(&mut self, role: Role, path: &Path) -> Result<Arc<Invocation>> {
        if self.0.contains_key(&role) {
            return Err(failure(1));
        }
        let executable = Arc::new(Executable::open(path).map_err(|_| failure(1))?);
        self.0.insert(role, executable.clone());
        Ok(Arc::new(Invocation {
            role,
            executable,
            evidence: Mutex::default(),
            admitted: std::sync::atomic::AtomicBool::new(false),
            invalid: std::sync::atomic::AtomicBool::new(false),
        }))
    }
    pub fn invocation(&mut self, role: Role, paths: &Paths) -> Result<Arc<Invocation>> {
        if let Some(executable) = self.0.get(&role) {
            executable.validate().map_err(|_| failure(1))?;
            return Ok(Arc::new(Invocation {
                role,
                executable: executable.clone(),
                evidence: Mutex::default(),
                admitted: std::sync::atomic::AtomicBool::new(false),
                invalid: std::sync::atomic::AtomicBool::new(false),
            }));
        }
        self.admit(role, paths.0.get(&role).ok_or_else(|| failure(1))?)
    }
    pub fn finish(self) -> Result<()> {
        let mut result = Ok(());
        for value in self.0.into_values() {
            let checked = value.validate().map(|_| ()).map_err(|_| failure(1));
            let closed = match Arc::try_unwrap(value) {
                Ok(value) => value.close().map_err(|_| failure(1)),
                Err(_) => Err(failure(1)),
            };
            result = result.and(checked).and(closed);
        }
        result
    }
}
