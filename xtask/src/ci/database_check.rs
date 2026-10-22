//! Fixed disposable PostgreSQL checks with an explicit captured environment.
//!
//! The existing Python suites remain the database/container owners. Preparation
//! owns only its CLI groups; the same PostgreSQL harness owns the new cluster.

use super::{
    command::{Command, Program},
    process::{Executor, Native},
};
use crate::{
    owned_process::Signals,
    test_env,
    tooling::{self, Environment, Failure},
};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

#[cfg(test)]
#[path = "database_check_tests.rs"]
mod tests;

const USAGE: &str =
    "usage: cargo run --locked -q -p xtask -- ci database-check monitoring|restore|migration";
const RESTORE_IMAGE: &str = "docker.io/library/postgres:18.6-bookworm@sha256:3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Check {
    Monitoring,
    Restore,
    Migration,
}
impl Check {
    fn parse(arguments: &[String]) -> Option<Self> {
        match arguments {
            [name] => match name.as_str() {
                "monitoring" => Some(Self::Monitoring),
                "restore" => Some(Self::Restore),
                "migration" => Some(Self::Migration),
                _ => None,
            },
            _ => None,
        }
    }
    fn command(self) -> Vec<String> {
        let pattern = match self {
            Self::Monitoring => "test_monitoring_database.py",
            Self::Restore => "test_restore_database.py",
            Self::Migration => "test_migrate_public.py",
        };
        [
            "python",
            "-m",
            "unittest",
            "discover",
            "-s",
            "ops/ansible/tests",
            "-p",
            pattern,
            "-v",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }
}

fn failure(message: &str) -> Failure {
    Failure::new(1, message)
}

/// Match command substitution's trailing LF removal; embedded/control bytes
/// cannot become additional PATH entries. join_paths handles spaces literally.
fn postgres_path(output: &[u8], environment: &Environment) -> tooling::Result<Environment> {
    if output.len() > 4096 {
        return Err(failure("PostgreSQL tool directory exceeded its bound."));
    }
    let text = std::str::from_utf8(output)
        .map_err(|_| failure("PostgreSQL tool directory is invalid."))?
        .trim_end_matches('\n');
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(failure("PostgreSQL tool directory is invalid."));
    }
    let directory = PathBuf::from(text);
    if !directory.is_absolute() || !directory.is_dir() {
        return Err(failure("PostgreSQL tool directory is unavailable."));
    }
    let inherited = environment
        .get(OsStr::new("PATH"))
        .cloned()
        .unwrap_or_default();
    let mut paths = vec![directory];
    paths.extend(std::env::split_paths(&inherited));
    let path = std::env::join_paths(paths)
        .map_err(|_| failure("PostgreSQL tool directory cannot be added to PATH."))?;
    let mut result = environment.clone();
    result.insert(OsString::from("PATH"), path);
    Ok(result)
}

async fn prepare(
    check: Check,
    environment: &Environment,
    executor: &mut impl Executor,
) -> tooling::Result<Environment> {
    executor.ready().await?;
    if check == Check::Restore {
        let mut pull = Command::capture(Program::Docker, &["pull", RESTORE_IMAGE]);
        pull.capture = None;
        pull.timeout = Duration::from_secs(600);
        executor.run(pull).await?;
    }
    let mut bindir = Command::capture(Program::PgConfig, &["--bindir"]);
    bindir.capture = Some(4096);
    bindir.timeout = Duration::from_secs(30);
    let output = executor.run(bindir).await?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(failure("Database-check preparation cleanup is unproven."));
    }
    postgres_path(&output, environment)
}

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    let Some(check) = Check::parse(arguments) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    };
    let environment = tooling::inherited_env();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let status = runtime.block_on(async {
        let mut native = Native::new(root, &environment, Signals::new()?);
        let environment = prepare(check, &environment, &mut native).await?;
        let mut signals = native.into_signals().await?;
        test_env::postgres_with_environment(root, &check.command(), &environment, &mut signals)
            .await
    });
    let code = match status {
        Ok(code) => code,
        Err(error) => {
            if let Some(failure) = error.downcast_ref::<Failure>() {
                eprintln!("{}", failure.message);
                i32::from(failure.code)
            } else if let Some(stop) = error.downcast_ref::<test_env::Stop>() {
                eprintln!("{}", stop.message);
                i32::from(stop.code)
            } else {
                eprintln!("Disposable database check failed.");
                1
            }
        }
    };
    Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)))
}
