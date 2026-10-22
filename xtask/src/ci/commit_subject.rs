//! Bounded branch-tip subject policy; the POSIX Git hook remains the reference.
//!
//! Git output stays private. The first LF ends the subject; shell substitution
//! removes NULs, and the hook's C-locale whitespace test is byte-based. The
//! bounded CI domain is explicit and does not change local hook bootstrapping.

use super::{
    command::{Command, Program},
    process::{Executor, Native},
};
use crate::{
    owned_process::Signals,
    tooling::{self, Environment, Failure},
};
use std::{ffi::OsStr, path::Path, process::ExitCode, time::Duration};

#[cfg(test)]
#[path = "commit_subject_tests.rs"]
mod tests;

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci commit-subject";
const MAX_OUTPUT: usize = 1024 * 1024;
const MAX_SUBJECT: usize = 64 * 1024;
const SUBJECT: &str = concat!(
    r"\A(?:feat|fix|docs|style|refactor|perf|test|build|ci|chore|revert|ops)",
    r"(?:\([a-z0-9][a-z0-9._/-]*\))?!?: [^\x09-\x0d\x20].*\z",
);
const INVALID: &str = concat!(
    "Commit subject must use type(scope): description, or type: description.\n",
    "Example: fix(web): prevent header controls from overlapping",
);

struct Revision(String);
impl Revision {
    fn read(environment: &Environment) -> tooling::Result<Self> {
        let revision = environment
            .get(OsStr::new("COMMIT_REVISION"))
            .and_then(|value| value.to_str())
            .filter(|value| value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .ok_or_else(|| Failure::new(2, "COMMIT_REVISION must be one complete GitHub SHA."))?;
        Ok(Self(revision.into()))
    }
}

fn valid(output: &[u8]) -> tooling::Result<bool> {
    if output.len() > MAX_OUTPUT {
        return Err(Failure::new(
            1,
            "Commit message exceeded the CI output bound.",
        ));
    }
    let end = output
        .iter()
        .position(|byte| *byte == b'\n')
        .unwrap_or(output.len());
    if end > MAX_SUBJECT {
        return Err(Failure::new(
            1,
            "Commit subject exceeded the CI first-line bound.",
        ));
    }
    let subject: Vec<u8> = output[..end]
        .iter()
        .copied()
        .filter(|byte| *byte != 0)
        .collect();
    // Unicode \S/trim would reject NBSP and malformed UTF-8 accepted by LC_ALL=C.
    let expression = regex::bytes::RegexBuilder::new(SUBJECT)
        .unicode(false)
        .build()
        .map_err(|_| Failure::new(1, "Commit subject policy unavailable."))?;
    Ok(expression.is_match(&subject))
}

async fn verify(revision: &Revision, executor: &mut impl Executor) -> tooling::Result<()> {
    executor.ready().await?;
    let mut command = Command::capture(Program::Git, &["log", "-1", "--format=%B", &revision.0]);
    command.timeout = Duration::from_secs(30);
    command.capture = Some(MAX_OUTPUT as u64);
    let output = executor.run(command).await?;
    executor.ready().await?;
    if !executor.cleanup_passed() {
        return Err(Failure::new(
            1,
            "Commit-subject command cleanup is unproven.",
        ));
    }
    if !valid(&output)? {
        return Err(Failure::new(1, INVALID));
    }
    executor.ready().await
}

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    let environment = tooling::inherited_env();
    let result = (|| -> tooling::Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| Failure::new(1, "Commit-subject runtime unavailable."))?;
        runtime.block_on(async {
            let signals = Signals::new()?;
            let revision = Revision::read(&environment)?;
            let mut native = Native::new(root, &environment, signals);
            verify(&revision, &mut native).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(error) => {
            // Every diagnostic is a fixed local string; no message/revision or
            // arbitrary Git stderr is copied into the workflow log.
            eprintln!("{}", error.message);
            Ok(ExitCode::from(error.code))
        }
    }
}
