//! Keep the reference installer and exact advisory policy; bound the whole plan.

use crate::{
    ci::{
        command::{Command, Program},
        runner,
        selected_executable::Executable,
    },
    tooling::{Environment, Failure, Result},
};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

pub(super) const BUDGET: Duration = Duration::from_secs(30 * 60);
pub(super) struct Context {
    pub root: PathBuf,
    pub environment: Environment,
    pub temporary: PathBuf,
}
impl Context {
    pub(super) fn new(root: &Path, environment: Environment, system: &str) -> Result<Self> {
        if !runner::authorized(&environment, system) {
            return Err(Failure::new(
                2,
                "Rust audit requires explicit hosted or disposable Linux CI.",
            ));
        }
        let temporary = PathBuf::from(runner::text(&environment, "RUNNER_TEMP"));
        if !temporary.is_absolute() || temporary.is_symlink() || !temporary.is_dir() {
            return Err(Failure::new(
                2,
                "Rust audit requires an existing absolute runner directory.",
            ));
        }
        Ok(Self {
            root: root.into(),
            environment,
            temporary,
        })
    }
}
fn remaining(deadline: Instant) -> Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Failure::new(124, "Rust audit budget exhausted."))
}
pub(super) fn reference(path: &Path, install: bool, deadline: Instant) -> Result<Command> {
    let mut command = Command::capture(
        Program::Python,
        if install {
            &[
                "build/security_tools.py",
                "install",
                "--tools",
                "cargo-audit",
                "--directory",
            ]
        } else {
            &[
                "build/security_tools.py",
                "path",
                "cargo-audit",
                "--directory",
            ]
        },
    );
    command.arguments.push(path.as_os_str().to_owned());
    command.timeout = remaining(deadline)?;
    command.capture = Some(4096);
    Ok(command)
}
pub(super) fn audit(executable: Arc<Executable>, deadline: Instant) -> Result<Command> {
    // SQLx 0.9 removed the optional rsa lockfile entry; leave that advisory
    // unsuppressed. These atty/rand exclusions affect mediasoup-sys build tools.
    let mut command = Command::capture(
        Program::CargoAudit,
        &[
            "audit",
            "--deny",
            "unsound",
            "--ignore",
            "RUSTSEC-2021-0145",
            "--ignore",
            "RUSTSEC-2026-0097",
        ],
    );
    command.executable = Some(executable);
    command.capture = None;
    command.timeout = remaining(deadline)?;
    Ok(command)
}
