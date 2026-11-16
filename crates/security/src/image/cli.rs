//! `image scan IMAGE --artifact-dir DIR --output DIR --tools-directory DIR
//! [--engine docker|podman] [--platform linux/amd64|linux/arm64]
//! [--database-cache DIR]` and `image prepare-database --output DIR
//! --tools-directory DIR --database-cache DIR [--engine docker|podman]
//! [--platform linux/amd64|linux/arm64]`: the image gate and the
//! database's own preparation.

use crate::{
    Result, engine, files,
    image::{
        gate::{self, Checkout, Options, Preparation},
        host::{Local, Signals},
        sandbox::Platform,
    },
    policy::Date,
};
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{Arc, atomic::AtomicBool},
};

pub(super) const USAGE: &str = concat!(
    "usage: simplestchat-security image scan IMAGE --artifact-dir DIR --output DIR",
    " --tools-directory DIR [--engine docker|podman] [--platform linux/amd64|linux/arm64]",
    " [--database-cache DIR]\n",
    "       simplestchat-security image prepare-database --output DIR --tools-directory DIR",
    " --database-cache DIR [--engine docker|podman] [--platform linux/amd64|linux/arm64]",
);

/// What a command line of either command asks for.
#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct Asked {
    pub(super) image: Option<String>,
    pub(super) artifact: Option<PathBuf>,
    pub(super) output: Option<PathBuf>,
    pub(super) tools: Option<PathBuf>,
    pub(super) engine: Option<engine::Name>,
    pub(super) platform: Option<Platform>,
    pub(super) database_cache: Option<PathBuf>,
}

/// What `arguments` ask for, or nothing for a command line that is not one
/// of the command's: each option at most once and followed by a value that
/// is no option, and an image, the only argument of its own, only when
/// `image` says the command scans one.
pub(super) fn parsed(arguments: &[String], image: bool) -> Option<Asked> {
    let mut asked = Asked::default();
    let mut rest = arguments.iter();
    while let Some(argument) = rest.next() {
        if !argument.starts_with('-') {
            if !image || asked.image.is_some() {
                return None;
            }
            asked.image = Some(argument.clone());
            continue;
        }
        let value = rest.next().filter(|value| !value.starts_with('-'))?;
        match argument.as_str() {
            "--artifact-dir" if image && asked.artifact.is_none() => {
                asked.artifact = Some(value.into());
            }
            "--output" if asked.output.is_none() => asked.output = Some(value.into()),
            "--tools-directory" if asked.tools.is_none() => asked.tools = Some(value.into()),
            "--database-cache" if asked.database_cache.is_none() => {
                asked.database_cache = Some(value.into());
            }
            "--engine" if asked.engine.is_none() => {
                asked.engine = Some(engine::Name::parsed(value)?);
            }
            "--platform" if asked.platform.is_none() => {
                asked.platform = Some(Platform::parsed(value)?);
            }
            _ => return None,
        }
    }
    Some(asked)
}

/// The scan that `asked` asks for, with its engine, once everything it
/// needs is there: Docker and AMD64 unless it says otherwise.
pub(super) fn scanning(asked: Asked) -> Option<(Options, engine::Name)> {
    let options = Options {
        image_id: asked.image?,
        artifact: asked.artifact?,
        output: asked.output?,
        tools: asked.tools?,
        platform: asked.platform.unwrap_or(Platform::Amd64),
        database_cache: asked.database_cache,
    };
    Some((options, asked.engine.unwrap_or(engine::Name::Docker)))
}

/// The preparation that `asked` asks for, with its engine, once everything
/// it needs is there: Docker and AMD64 unless it says otherwise.
pub(super) fn preparing(asked: Asked) -> Option<(Preparation, engine::Name)> {
    let preparation = Preparation {
        output: asked.output?,
        tools: asked.tools?,
        platform: asked.platform.unwrap_or(Platform::Amd64),
        database_cache: Some(asked.database_cache?),
    };
    Some((preparation, asked.engine.unwrap_or(engine::Name::Docker)))
}

/// Let only this account read and write what the gate creates.
fn private() {
    let _previous = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o077));
}

/// The exit status of a command that `passed` once its status line was
/// written, as `said` says it was.
fn status(passed: bool, said: bool) -> ExitCode {
    if passed && said {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// The status line of a scan that `passed` or not, whose evidence is at
/// `output`.
fn reported(passed: bool, output: &Path) -> String {
    let verdict = if passed { "passed" } else { "failed" };
    format!("Image security {verdict}; evidence: {}", output.display())
}

/// The status line of a preparation of the database that `passed` or not.
fn readied(passed: bool) -> String {
    let verdict = if passed { "passed" } else { "failed" };
    format!("Image database preparation {verdict}")
}

/// Write the status line `line`: whether it was written.
fn said(line: &str) -> bool {
    writeln!(std::io::stdout(), "{line}").is_ok()
}

/// The gate as `options` ask for it on `engine`, cancelled by SIGINT and
/// SIGTERM while it runs.
fn judged(
    root: &Path,
    options: &Options,
    engine: engine::Name,
    cancelled: &Arc<AtomicBool>,
) -> Result<bool> {
    let resolved = options.artifact.canonicalize();
    let artifact = resolved.map_err(|error| files::unusable(&error))?;
    let options = Options {
        artifact,
        ..options.clone()
    };
    let _signals = Signals::cancelling(cancelled)?;
    let today = Date::today();
    let checkout = Checkout::read(root, today)?;
    let ambient = |name: &str| std::env::var_os(name);
    let host = Local {
        root,
        engine,
        ambient: &ambient,
        today,
        cancelled: Arc::clone(cancelled),
    };
    gate::execute(&options, &checkout, &host)
}

/// The database's preparation as `preparation` asks for it on `engine`,
/// cancelled by SIGINT and SIGTERM while it runs.
fn prepared(
    root: &Path,
    preparation: &Preparation,
    engine: engine::Name,
    cancelled: &Arc<AtomicBool>,
) -> Result<bool> {
    let _signals = Signals::cancelling(cancelled)?;
    let ambient = |name: &str| std::env::var_os(name);
    let host = Local {
        root,
        engine,
        ambient: &ambient,
        today: Date::today(),
        cancelled: Arc::clone(cancelled),
    };
    gate::execute_database(preparation, root, &host)
}

/// Run `image scan` for the checkout at `root`: one line saying whether the
/// image passed and where its evidence is, with status 0 only when it did;
/// a failure before any evidence also names its code on standard error.
pub(super) fn scan(root: &Path, arguments: &[String]) -> ExitCode {
    let Some((options, engine)) = parsed(arguments, true).and_then(scanning) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    private();
    let cancelled = Arc::new(AtomicBool::new(false));
    let passed = judged(root, &options, engine, &cancelled).unwrap_or_else(|fault| {
        eprintln!("Image security failed: {fault}");
        false
    });
    status(passed, said(&reported(passed, &options.output)))
}

/// Run `image prepare-database` for the checkout at `root`: one line saying
/// whether the database was prepared, with status 0 only when it was; a
/// failure before any evidence also names its code on standard error.
pub(super) fn prepare(root: &Path, arguments: &[String]) -> ExitCode {
    let Some((preparation, engine)) = parsed(arguments, false).and_then(preparing) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    private();
    let cancelled = Arc::new(AtomicBool::new(false));
    let passed = prepared(root, &preparation, engine, &cancelled).unwrap_or_else(|fault| {
        eprintln!("Image database preparation failed: {fault}");
        false
    });
    status(passed, said(&readied(passed)))
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
