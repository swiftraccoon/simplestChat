//! Publish the fixed npm installer's directory to the runner's PATH file.
//! The preceding Node installer authenticates contents; this owns file policy.

use crate::{ci::runner, tooling};
use rustix::fs::{Mode, OFlags};
use std::{fs::File, io::Write, os::unix::fs::MetadataExt, path::Path, process::ExitCode};

const PREFIX: &str = "simplestchat-npm-12.2.0";
const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ci npm-path";
const DIRECTORY: OFlags = OFlags::RDONLY
    .union(OFlags::DIRECTORY)
    .union(OFlags::NOFOLLOW)
    .union(OFlags::CLOEXEC);

pub(super) fn run(arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        eprintln!("{USAGE}");
        return Ok(ExitCode::from(2));
    }
    match publish(&tooling::inherited_env()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(()) => {
            eprintln!("Pinned npm PATH publication failed.");
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

fn record(environment: &tooling::Environment) -> Result<(String, &Path), ()> {
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
    let bin = Path::new(temporary).join(PREFIX).join("bin");
    Ok((format!("{}\n", bin.to_str().ok_or(())?), target))
}

fn publish(environment: &tooling::Environment) -> Result<(), ()> {
    let (record, target) = record(environment)?;
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
        for component in [PREFIX, "bin"] {
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
