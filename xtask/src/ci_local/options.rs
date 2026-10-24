//! Local workflow selection, with full-gate coverage kept indivisible.

use crate::tooling::{Failure, Result};
use std::path::PathBuf;

pub(super) const USAGE: &str = "\
Usage: cargo run --locked -q -p xtask -- ci-local [all|JOB] [options]

  all                         Run the required gate and all its needs (default).
  JOB                         Run one job; this is not a complete CI result.
  --base REF                  Changed-source base (default: merge-base with origin/main).
  --matrix KEY:VALUE          Select a matrix entry for a single job only.
  --output NEW_DIRECTORY      Evidence directory (default: new directory under results/).
  --disposable-engine         Use explicit DOCKER_HOST for an isolated Linux engine.
  --help                      Show this help.

Full CI requires a clean committed checkout, but no push or pull request. Hosted
signing/publication stays on GitHub. macOS uses only the owned simplestchat-ci
Podman VM. Linux requires --disposable-engine and an explicit local Unix
DOCKER_HOST; never select a shared or production engine.
";

pub(super) struct Options {
    pub job: String,
    pub base: Option<String>,
    pub output: Option<PathBuf>,
    pub disposable: bool,
    pub matrix: Vec<String>,
}

pub(super) fn parse(arguments: &[String]) -> Result<Option<Options>> {
    let mut options = Options {
        job: "all".into(),
        base: None,
        output: None,
        disposable: false,
        matrix: Vec::new(),
    };
    let mut arguments = arguments.iter().peekable();
    if arguments
        .peek()
        .is_some_and(|argument| !argument.starts_with('-'))
    {
        options.job = arguments.next().expect("peeked job").clone();
    }
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--help" | "-h" => return Ok(None),
            "--disposable-engine" => options.disposable = true,
            "--base" | "--matrix" | "--output" => {
                let value = arguments
                    .next()
                    .ok_or_else(|| Failure::new(2, format!("Missing value for {argument}")))?;
                match argument.as_str() {
                    "--base" => options.base = Some(value.clone()),
                    "--matrix" => options.matrix.push(value.clone()),
                    "--output" => options.output = Some(value.into()),
                    _ => unreachable!("matched value option"),
                }
            }
            _ => {
                return Err(Failure::new(
                    2,
                    format!("Unknown option: {argument} (see --help)"),
                ));
            }
        }
    }
    let mut characters = options.job.bytes();
    if !characters
        .next()
        .is_some_and(|byte| byte.is_ascii_lowercase())
        || !characters
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(Failure::new(2, "Invalid job name."));
    }
    if options.job == "all" && !options.matrix.is_empty() {
        return Err(Failure::new(
            2,
            "A full CI run cannot filter out matrix entries.",
        ));
    }
    if matches!(
        options.job.as_str(),
        "required" | "release-security" | "release-prepare"
    ) {
        return Err(Failure::new(
            2,
            "Use all for the aggregate gate; release signing runs only on GitHub.",
        ));
    }
    Ok(Some(options))
}
