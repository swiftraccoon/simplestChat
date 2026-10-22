//! The owned test environment: a disposable PostgreSQL cluster and a loopback
//! server with a clean configuration, started for one command and stopped
//! afterwards with evidence of how they stopped.
//!
//! `test-postgres COMMAND…` and `test-server COMMAND…` each own one service;
//! `test-env COMMAND…` nests them. Refusals exit 2 before anything starts; a
//! command's own status is preserved; cleanup trouble turns success into 1 but
//! never hides a failure; INT and TERM clean up and exit 130 or 143.

mod companion;
mod helper;
mod postgres;
mod process;
mod server;
mod service;

use crate::{
    owned_process::Signals,
    tooling::{self, Environment},
};
use anyhow::Result;
pub(crate) use companion::{Companion, NoCompanion};
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

pub(crate) fn run_postgres(_root: &Path, command: &[String]) -> Result<ExitCode> {
    let environment = tooling::inherited_env();
    let directory = std::env::current_dir()?;
    execute(async {
        let mut signals = Signals::new()?;
        postgres::run(&directory, command, &environment, &mut signals).await
    })
}

/// CI preparation transfers its captured environment and registered signals to
/// this same cluster owner; no nested Cargo process or second harness is used.
pub(crate) async fn postgres_with_environment(
    root: &Path,
    command: &[String],
    environment: &Environment,
    signals: &mut Signals,
) -> Result<i32> {
    postgres::run(root, command, environment, signals).await
}

pub(crate) fn run_server(root: &Path, command: &[String]) -> Result<ExitCode> {
    let environment = tooling::inherited_env();
    execute(server::run(root, command, &environment))
}

/// Compose a fixed CI workload directly, with one captured environment and one
/// signal owner. The companion is finalized before the owned server retires.
pub(crate) async fn server_with_environment(
    root: &Path,
    command: &[String],
    environment: &Environment,
    signals: &mut Signals,
    companion: &mut impl Companion,
) -> Result<i32> {
    server::run_with_companion(root, command, environment, signals, companion).await
}

/// Reuse the harness's bounded fresh CLI-group owner, without another runtime
/// or signal registration. Cleanup calls ignore cancellation until settlement.
pub(crate) async fn helper_with_signals(
    command: std::process::Command,
    timeout: std::time::Duration,
    capture: Option<u64>,
    signals: Option<&mut Signals>,
) -> Result<(i32, Vec<u8>)> {
    let output = helper::run(command, timeout, capture, signals).await?;
    Ok((output.status, output.stdout))
}

/// Both services for one command: the server harness runs as the cluster's
/// test command, through this same binary.
pub(crate) fn run_env(_root: &Path, command: &[String]) -> Result<ExitCode> {
    if command.is_empty() {
        return execute(async {
            Err(Stop::usage("Usage: cargo run -q -p xtask -- test-env COMMAND [ARG ...]").into())
        });
    }
    let exe = std::env::current_exe()?;
    let mut nested = vec![exe.to_string_lossy().into_owned(), "test-server".to_owned()];
    nested.extend(command.iter().cloned());
    let environment = tooling::inherited_env();
    let directory = std::env::current_dir()?;
    execute(async {
        let mut signals = Signals::new()?;
        postgres::run(&directory, &nested, &environment, &mut signals).await
    })
}
