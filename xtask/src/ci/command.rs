//! Closed command vocabulary for bounded CI subprocesses. No shell execution.

use std::ffi::OsString;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Program {
    Git,
    Docker,
    Curl,
    Sudo,
    Python,
    CargoAudit,
    Node,
}
impl Program {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Git => "git",
            Self::Docker => "docker",
            Self::Curl => "curl",
            Self::Sudo => "sudo",
            Self::Python => "python3",
            Self::CargoAudit => "cargo-audit",
            Self::Node => "node",
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct Command {
    pub program: Program,
    pub arguments: Vec<OsString>,
    pub timeout: Duration,
    pub grace: Duration,
    pub capture: Option<u64>,
    pub cleanup: bool,
    pub stdout: Option<std::sync::Arc<std::fs::File>>,
    pub executable: Option<std::sync::Arc<super::selected_executable::Executable>>,
    pub node: Option<std::sync::Arc<super::release_checks::files::Executable>>,
}
impl Command {
    pub(super) fn capture(program: Program, args: &[&str]) -> Self {
        Self {
            program,
            arguments: args.iter().map(OsString::from).collect(),
            timeout: Duration::from_secs(30),
            grace: Duration::from_secs(5),
            capture: Some(4 * 1024 * 1024),
            cleanup: false,
            stdout: None,
            executable: None,
            node: None,
        }
    }
}
