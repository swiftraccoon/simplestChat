//! Installed-tool quality gates shared by local development and CI.
//!
//! Groups run in web, Rust, Python, helpers order and stop at the first failure.
//! This command never installs tools or starts application services. Its child
//! environment is explicit so tests can run isolated checkouts without mutating
//! process-global state.

mod python;
#[cfg(test)]
#[path = "check/check_tests.rs"]
mod tests;

use crate::tooling::{self, Environment, Failure};
use anyhow::Result;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: cargo run --locked -q -p xtask -- check [--web | --rust | --python | --helpers]
Default: run all four groups. Requires the development prerequisites.
Web: typed lint, formatting, unit tests, type checking and production build.
Rust: formatting, workspace Clippy, the over-length line ratchet and checked documentation.
Python: pinned Ruff lint/format and strict basedpyright over maintained sources and stubs.
Helpers: shell syntax and helper/benchmark regression tests.
Database, native Rust tests and browser integration remain separate; see docs/testing.md.
";

pub(crate) fn run(root: &Path, arguments: &[String]) -> Result<ExitCode> {
    match run_with_env(root, arguments, tooling::inherited_env()) {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(failure) => {
            eprintln!("{}", failure.message);
            Ok(ExitCode::from(failure.code))
        }
    }
}

fn run_with_env(
    root: &Path,
    arguments: &[String],
    mut environment: Environment,
) -> tooling::Result<()> {
    let group = match arguments {
        [] => "all",
        [argument] => match argument.as_str() {
            "--help" | "-h" => {
                print!("{USAGE}");
                return Ok(());
            }
            "--web" => "web",
            "--rust" => "rust",
            "--python" => "python",
            "--helpers" => "helpers",
            _ => return Err(Failure::new(2, USAGE)),
        },
        _ => return Err(Failure::new(2, USAGE)),
    };
    if group == "all" || group == "web" {
        web(root, &environment)?;
    }
    if group == "all" || group == "rust" {
        rust(root, &mut environment)?;
    }
    if group == "all" || group == "python" {
        python::run(root, &environment)?;
    }
    if group == "all" || group == "helpers" {
        helpers(root, &environment)?;
    }
    Ok(())
}

fn web(root: &Path, environment: &Environment) -> tooling::Result<()> {
    if !root.join("web/node_modules").is_dir() {
        return Err(Failure::new(
            2,
            "Install web dependencies first: npm --prefix web ci --ignore-scripts",
        ));
    }
    for arguments in [
        vec!["--prefix", "web", "run", "lint"],
        vec!["--prefix", "web", "run", "format:check"],
        vec!["--prefix", "web", "test"],
        vec!["--prefix", "web", "run", "build"],
    ] {
        tooling::run(root, environment, "npm", arguments)?;
    }
    Ok(())
}

fn rust(root: &Path, environment: &mut Environment) -> tooling::Result<()> {
    let channel = tooling::pinned_toolchain(root)?;
    tooling::pin_compilers(root, environment, &channel)?;
    let inputs = tooling::native_env(root, environment, true);
    tooling::require_native_archives(&inputs)?;
    for arguments in [
        vec!["fmt", "--all", "--", "--check"],
        vec![
            "clippy",
            "--locked",
            "--workspace",
            "--all-targets",
            "--no-deps",
            "--",
            "-D",
            "warnings",
        ],
        vec!["run", "--locked", "-q", "-p", "xtask", "--", "line-length"],
        vec![
            "run",
            "--locked",
            "-q",
            "-p",
            "xtask",
            "--",
            "protocol-ts",
            "--check",
        ],
        vec![
            "run",
            "--locked",
            "-q",
            "-p",
            "xtask",
            "--",
            "http-policy",
            "--check",
        ],
    ] {
        tooling::run(
            root,
            environment,
            "rustup",
            ["run", &channel, "cargo"].into_iter().chain(arguments),
        )?;
    }
    let mut flags = environment
        .get(OsStr::new("RUSTDOCFLAGS"))
        .cloned()
        .unwrap_or_default();
    flags.push(" -D warnings");
    environment.insert("RUSTDOCFLAGS".into(), flags);
    tooling::run(
        root,
        environment,
        "rustup",
        [
            "run",
            &channel,
            "cargo",
            "doc",
            "--locked",
            "--workspace",
            "--no-deps",
            "--document-private-items",
        ],
    )
}

/// Expand the maintained flat globs as separate arguments, including filenames
/// containing whitespace. A missing match retains the shell's literal pattern.
fn matching(root: &Path, directory: &str, suffix: &str) -> tooling::Result<Vec<OsString>> {
    let mut files = std::fs::read_dir(root.join(directory))
        .map_err(|error| Failure::new(1, format!("Cannot read {directory}: {error}")))?
        .map(|entry| {
            entry
                .map(|entry| {
                    Path::new(directory)
                        .join(entry.file_name())
                        .into_os_string()
                })
                .map_err(|error| Failure::new(1, format!("Cannot read {directory}: {error}")))
        })
        .collect::<tooling::Result<Vec<_>>>()?;
    files.retain(|path| path.to_string_lossy().ends_with(suffix));
    files.sort();
    if files.is_empty() {
        files.push(format!("{directory}/*{suffix}").into());
    }
    Ok(files)
}

fn helpers(root: &Path, environment: &Environment) -> tooling::Result<()> {
    tooling::run(root, environment, "sh", ["-n", ".githooks/commit-msg"])?;
    let scripts = matching(root, "build", ".sh")?;
    for script in &scripts {
        tooling::run(root, environment, "bash", [OsStr::new("-n"), script])?;
    }
    tooling::run(
        root,
        environment,
        "shellcheck",
        scripts
            .iter()
            .map(OsString::as_os_str)
            .chain([OsStr::new(".githooks/commit-msg")]),
    )?;
    let mut arguments = vec![OsString::from("--test")];
    arguments.extend(matching(root, "build/tests", ".test.mjs")?);
    arguments.push("load_tests/benchmark-local.test.mjs".into());
    arguments.push("load_tests/report-command.test.mjs".into());
    tooling::run(root, environment, "node", arguments)
}
