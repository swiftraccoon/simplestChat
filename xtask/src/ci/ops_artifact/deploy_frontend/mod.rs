//! Routine controller selection, one typed Ansible call and anonymous public checks.
//!
//! GitHub metadata selects candidates. The current playbook performs application
//! and Ops cryptography; only its actual successful completion permits public
//! verification. An interrupted controller cannot infer the remote unit's result.

mod execute;
mod files;
mod inventory;
mod lifecycle;
mod options;
mod plan;
mod policy;
pub(in crate::ci) mod tools;
mod worker;

use crate::tooling::{Failure, Result};
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

pub(super) fn run(root: &Path, arguments: &[String]) -> ExitCode {
    if arguments == ["--help"] || arguments == ["-h"] {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }
    let Some((operation, arguments)) = arguments.split_first() else {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    };
    if operation != "deploy" {
        eprint!("{USAGE}");
        return ExitCode::from(2);
    }
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
    match lifecycle::run(root, options) {
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
            eprintln!(
                "{} Inspect private controller evidence and the remote release journal.",
                error.message
            );
            ExitCode::from(error.code)
        }
    }
}
