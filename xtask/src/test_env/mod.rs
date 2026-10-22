//! The owned test environment: a disposable PostgreSQL cluster and a loopback
//! server with a clean configuration, started for one command and stopped
//! afterwards with evidence of how they stopped.
//!
//! `test-postgres COMMAND…` and `test-server COMMAND…` each own one service;
//! `test-env COMMAND…` nests them. Refusals exit 2 before anything starts; a
//! command's own status is preserved; cleanup trouble turns success into 1 but
//! never hides a failure; INT and TERM clean up and exit 130 or 143.

mod helper;
mod postgres;
mod process;
mod server;
mod service;

use anyhow::Result;
use std::path::Path;
use std::process::ExitCode;

/// A refusal or failure with the exit status the shell helpers used.
#[derive(Debug)]
pub(crate) struct Stop {
    pub code: u8,
    pub message: String,
}

impl Stop {
    pub(crate) fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Stop {}

/// Run an async harness entry point on a current-thread runtime and turn its
/// outcome into the process exit code.
fn execute<F>(future: F) -> Result<ExitCode>
where
    F: std::future::Future<Output = Result<i32>>,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let status = match runtime.block_on(future) {
        Ok(status) => status,
        Err(error) => match error.downcast_ref::<Stop>() {
            Some(stop) => {
                eprintln!("{}", stop.message);
                i32::from(stop.code)
            }
            None => return Err(error),
        },
    };
    Ok(ExitCode::from(u8::try_from(status).unwrap_or(1)))
}

pub(crate) fn run_postgres(root: &Path, command: &[String]) -> Result<ExitCode> {
    execute(postgres::run(root, command))
}

pub(crate) fn run_server(root: &Path, command: &[String]) -> Result<ExitCode> {
    execute(server::run(root, command))
}

/// Both services for one command: the server harness runs as the cluster's
/// test command, through this same binary.
pub(crate) fn run_env(root: &Path, command: &[String]) -> Result<ExitCode> {
    if command.is_empty() {
        return execute(async {
            Err(Stop::usage("Usage: cargo run -q -p xtask -- test-env COMMAND [ARG ...]").into())
        });
    }
    let exe = std::env::current_exe()?;
    let mut nested = vec![exe.to_string_lossy().into_owned(), "test-server".to_owned()];
    nested.extend(command.iter().cloned());
    execute(postgres::run(root, &nested))
}
