//! Routine controller selection, one typed Ansible call and anonymous public checks.
//!
//! GitHub metadata selects candidates. The current playbook performs application
//! and Ops cryptography; only its actual successful completion permits public
//! verification. An interrupted controller cannot infer the remote unit's result.

mod execute;
mod files;
mod inventory;
mod lifecycle;
mod migrate;
mod options;
mod plan;
mod policy;
pub(crate) mod tools;
mod worker;

use simplestchat_toolkit::tooling::{Failure, Result};
use std::{path::Path, process::ExitCode};

use options::USAGE;

fn failure(code: u8) -> Failure {
    Failure::new(code, "Routine deployment controller did not complete.")
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
fn keep(primary: &mut Option<Failure>, error: Failure) {
    if primary.is_none() {
        *primary = Some(error);
    }
}

/// The `deploy` command, given the arguments after its name.
pub(super) fn run(root: &Path, arguments: &[String]) -> ExitCode {
    let options = match options::parse(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("Invalid routine deployment options.");
            return ExitCode::from(error.code);
        }
    };
    let advice = "Inspect private controller evidence and the remote release journal.";
    finished(lifecycle::run(root, options), advice)
}

/// The `migrate` command, given the arguments after its name.
pub(super) fn migrate(root: &Path, arguments: &[String]) -> ExitCode {
    let options = match migrate::parse(arguments) {
        Ok(Some(options)) => options,
        Ok(None) => {
            print!("{}", migrate::USAGE);
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("Invalid migration options.");
            return ExitCode::from(error.code);
        }
    };
    let advice = "Inspect private controller evidence and both hosts' migration records.";
    finished(lifecycle::migrate(root, options), advice)
}

/// The exit status of one owned operation: its own, or the envelope's when
/// it ended after its final fence.
fn finished(result: Result<lifecycle::Completed>, advice: &str) -> ExitCode {
    match result {
        Ok(completed) => {
            if completed.status != 0 {
                ExitCode::from(completed.status)
            } else if std::time::Instant::now() >= completed.end {
                ExitCode::from(124)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("{} {advice}", error.message);
            ExitCode::from(error.code)
        }
    }
}
