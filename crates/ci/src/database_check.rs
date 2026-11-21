//! Fixed disposable PostgreSQL checks with an explicit captured environment.
//!
//! The monitoring statements, the release backup verification and the
//! migration snapshot are tested by the Ops crate beside the code that issues
//! them. With `RESTORE_CONTAINER_CHECK=1` the restore check also verifies
//! inside real containers of the pinned production PostgreSQL image, which
//! preparation pulls first. Preparation owns only its CLI groups; the same
//! PostgreSQL harness owns the new cluster.

use super::{
    command::{Command, Program},
    process::{Executor, Native},
};
use simplestchat_toolkit::test_env;
use simplestchat_toolkit::{
    owned_process::Signals,
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

const USAGE: &str = "usage: cargo run --locked -q -p simplestchat-ci -- \
                     database-check monitoring|restore|migration";
/// The production PostgreSQL image, `scpub_postgres_image` of the deployment.
const RESTORE_IMAGE: &str = concat!(
    "docker.io/library/postgres:18.6-bookworm@sha256:",
    "3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650",
);
/// Selects the restore check's real-container mode, for which preparation
/// pulls `RESTORE_IMAGE`.
const CONTAINER_SELECTOR: &str = "RESTORE_CONTAINER_CHECK";

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
        // These tests live in the Ops crate beside the code that issues the
        // statements. Each is named exactly: a filter that matched nothing
        // would pass having run nothing.
        let owned = |module: &str, names: &[&str]| {
            let mut command = [
                "cargo",
                "test",
                "--locked",
                "-p",
                "simplestchat-ops",
                "--lib",
                "--",
                "--ignored",
                "--exact",
                "--test-threads=1",
            ]
            .map(str::to_owned)
            .to_vec();
            command.extend(names.iter().map(|name| format!("{module}::{name}")));
            command
        };
        match self {
            Self::Monitoring => owned(
                "monitor::database_tests",
                &[
                    "an_ambiguous_committed_response_retains_the_outage_after_recovery",
                    "external_history_keeps_gaps_privacy_and_retention_through_partial_recovery",
                    "incident_lifecycle_replay_privileges_and_retention",
                ],
            ),
            Self::Restore => owned(
                "release::attestation::restore::database_tests",
                &[
                    "a_pre_upgrade_backup_verifies_against_its_own_recorded_ledger",
                    "a_current_backup_keeps_incidents_and_refuses_privacy_and_schema_drift",
                    "a_truncated_backup_never_verifies",
                    "a_real_container_of_the_pinned_image_verifies_and_refuses",
                ],
            ),
            Self::Migration => owned(
                "migrate::snapshot::database_tests",
                &[
                    "only_the_lossless_enum_cast_relocation_is_made_canonical",
                    "a_real_restore_keeps_rows_partitions_grants_and_the_schema_digest",
                ],
            ),
        }
    }
    /// Select the tests that run only inside this check's own cluster.
    fn select(self, environment: &mut Environment) {
        let selector = match self {
            Self::Monitoring => "MONITORING_DATABASE_CHECK",
            Self::Restore => "RESTORE_DATABASE_CHECK",
            Self::Migration => "MIGRATION_DATABASE_CHECK",
        };
        environment.insert(selector.into(), "1".into());
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

/// Prepare `check`: pull the pinned image when the restore check runs in
/// real containers, then find the PostgreSQL tools. Each command has its own
/// outer bound; a failure keeps its status and nothing after it runs.
async fn prepare(
    check: Check,
    environment: &Environment,
    executor: &mut impl Executor,
) -> tooling::Result<Environment> {
    executor.ready().await?;
    let containers = environment
        .get(OsStr::new(CONTAINER_SELECTOR))
        .is_some_and(|value| value == "1");
    if check == Check::Restore && containers {
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
        let mut environment = prepare(check, &environment, &mut native).await?;
        check.select(&mut environment);
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
