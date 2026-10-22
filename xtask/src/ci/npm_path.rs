//! Publish a closed installer role's directory to the runner's PATH file.
//!
//! npm's preceding Node installer authenticates its contents. Controller setup
//! retains the ordinary Python venv/pip installer. This module owns directory
//! admission and the one-line runner-file append, not installed package trust.

use crate::{ci::runner, tooling};
use rustix::fs::{Mode, OFlags};
use std::{fs::File, io::Write, os::unix::fs::MetadataExt, path::Path, process::ExitCode};

const PREFIX: &str = "simplestchat-npm-12.2.0";
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Npm,
    Controller,
}
impl Role {
    fn prefix(self) -> &'static str {
        match self {
            Self::Npm => PREFIX,
            Self::Controller => "ansible-controller",
        }
    }
    fn usage(self) -> &'static str {
        match self {
            Self::Npm => "usage: cargo run --locked -q -p xtask -- ci npm-path",
            Self::Controller => "usage: cargo run --locked -q -p xtask -- ci controller-path",
        }
    }
    fn failure(self) -> &'static str {
        match self {
            Self::Npm => "Pinned npm PATH publication failed.",
            Self::Controller => "Controller PATH publication failed.",
        }
    }
}
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    run_role(Role::Npm, arguments)
}
pub(super) fn run_controller(arguments: &[String]) -> anyhow::Result<ExitCode> {
    run_role(Role::Controller, arguments)
}

/// No path/prefix argument is accepted. Both role names select literals;
/// the environment supplies only the runner-owned root and existing PATH file.
fn run_role(role: Role, arguments: &[String]) -> anyhow::Result<ExitCode> {
    let usage = role.usage();
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{usage}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{usage}");
        return Ok(ExitCode::from(2));
    }
    match publish(role, &tooling::inherited_env()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(()) => {
            eprintln!("{}", role.failure());
            Ok(ExitCode::FAILURE)
        }
    }
}

fn owned_directory(file: &File) -> Result<(), ()> {
    let metadata = file.metadata().map_err(|_| ())?;
    if metadata.is_dir()
        && metadata.uid() == rustix::process::getuid().as_raw()
        && metadata.mode() & 0o022 == 0
    {
        Ok(())
    } else {
        Err(())
    }
}

fn record(role: Role, environment: &tooling::Environment) -> Result<(String, &Path), ()> {
    let temporary = runner::text(environment, "RUNNER_TEMP");
    // GitHub's PATH protocol is line based. No shell quoting can make a control
    // character safe here; spaces and Unicode paths remain literal and valid.
    if !Path::new(temporary).is_absolute()
        || temporary.chars().any(char::is_control)
        || temporary.len() > 4096
    {
        return Err(());
    }
    let target = Path::new(runner::text(environment, "GITHUB_PATH"));
    if !target.is_absolute() {
        return Err(());
    }
    let bin = Path::new(temporary).join(role.prefix()).join("bin");
    Ok((format!("{}\n", bin.to_str().ok_or(())?), target))
}

fn publish(role: Role, environment: &tooling::Environment) -> Result<(), ()> {
    let (record, target) = record(role, environment)?;
    let temporary = Path::new(runner::text(environment, "RUNNER_TEMP"));
    let mut files = Vec::new();
    let result = (|| {
        let file = rustix::fs::open(
            target,
            OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map(File::from)
        .map_err(|_| ())?;
        files.push(file);
        if !files[0].metadata().is_ok_and(|m| m.is_file()) {
            return Err(());
        }
        let parent = rustix::fs::open(temporary, DIRECTORY, Mode::empty())
            .map(File::from)
            .map_err(|_| ())?;
        files.push(parent);
        owned_directory(&files[1])?;
        for component in [role.prefix(), "bin"] {
            let directory =
                rustix::fs::openat(files.last().unwrap(), component, DIRECTORY, Mode::empty())
                    .map(File::from)
                    .map_err(|_| ())?;
            files.push(directory);
            owned_directory(files.last().unwrap())?;
        }
        files[0].write_all(record.as_bytes()).map_err(|_| ())
    })();
    finish(result, files, |file| {
        nix::unistd::close(file).map_err(|_| ())
    })
}

fn finish(
    result: Result<(), ()>,
    files: Vec<File>,
    mut close: impl FnMut(File) -> Result<(), ()>,
) -> Result<(), ()> {
    let mut closed = true;
    for file in files.into_iter().rev() {
        closed &= close(file).is_ok();
    }
    result.and(if closed { Ok(()) } else { Err(()) })
}

#[cfg(test)]
#[path = "npm_path_tests.rs"]
mod tests;
