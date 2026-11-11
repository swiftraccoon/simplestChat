//! The hosted runtime proof's request, written once from the workflow's inputs.
//!
//! The dispatch inputs select an artifact identity and the event selects the
//! source revision; no host, executable or path is an input. This prebuilt
//! controller operation validates both, writes the closed request to one fresh
//! private leaf under `RUNNER_TEMP` and publishes only the SHA-256 of those
//! exact bytes as the `sha256` step output, which the runtime operation then
//! requires. It starts no process and reads no credential. A refusal before
//! the write leaves neither a request nor an output line.
//!
//! Affected API guidance: C-VALIDATE closes the identifier and revision
//! domains before any write; C-GOOD-ERR keeps one fixed diagnostic line per
//! refusal class and repeats no input; C-SERDE fixes the field order of the
//! written bytes. These decisions cover this change, not a retrospective audit.

use rustix::fs::{Mode, OFlags};
use serde::Serialize;
use std::{
    fs::File,
    io::Write,
    path::{Path, PathBuf},
    process::ExitCode,
};

const SAFE: u64 = 9_007_199_254_740_991;
const LEAF: &str = "ops-runtime-request.json";

/// Why no request was published. Its display text is the whole diagnostic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The event revision is not forty lowercase hexadecimal digits.
    Revision,
    /// A run, attempt or artifact identifier is not a canonical positive
    /// integer within the safe range.
    Identifier,
    /// The runner directory, the request leaf or the step output was unusable.
    Unwritten,
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Revision => "Invalid Ops source revision.",
            Self::Identifier => "Invalid Ops artifact identifier.",
            Self::Unwritten => "Ops runtime request is unwritten.",
        })
    }
}

/// The workflow step's complete input.
pub(super) struct Inputs<'a> {
    pub revision: &'a str,
    pub run: &'a str,
    pub attempt: &'a str,
    pub artifact: &'a str,
    pub runner_temp: &'a Path,
    pub step_output: &'a Path,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Selected<'a> {
    repository: &'static str,
    revision: &'a str,
    package_version: &'static str,
    target: &'static str,
    protocol_version: u8,
    build_run_id: u64,
    run_attempt: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Wire<'a> {
    schema_version: u8,
    selection: Selected<'a>,
    artifact_id: u64,
    output: &'a str,
    authentication: &'static str,
}

/// One to sixteen ASCII digits without a leading zero, at most the safe bound.
fn identifier(raw: &str) -> Result<u64, Refusal> {
    let canonical = (1..=16).contains(&raw.len())
        && !raw.starts_with('0')
        && raw.bytes().all(|byte| byte.is_ascii_digit());
    let value = if canonical { raw.parse().ok() } else { None };
    value
        .filter(|value| *value <= SAFE)
        .ok_or(Refusal::Identifier)
}

/// Validate the revision and then each identifier, write the request to a
/// leaf that must not exist, and append its digest to the step output. An
/// existing file, symbolic link or FIFO at the leaf is never adopted.
pub(super) fn write(inputs: &Inputs<'_>) -> Result<(), Refusal> {
    let revision = inputs.revision;
    let hexadecimal = |byte: u8| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte);
    if revision.len() != 40 || !revision.bytes().all(hexadecimal) {
        return Err(Refusal::Revision);
    }
    let selection = Selected {
        repository: "swiftraccoon/simplestChat",
        revision,
        package_version: env!("CARGO_PKG_VERSION"),
        target: "x86_64-unknown-linux-musl",
        protocol_version: 1,
        build_run_id: identifier(inputs.run)?,
        run_attempt: identifier(inputs.attempt)?,
    };
    let artifact_id = identifier(inputs.artifact)?;
    // Repeated separators and `.` components collapse, so an ordinary runner
    // directory yields the normalized output path the runtime operation accepts.
    let runner: PathBuf = inputs.runner_temp.components().collect();
    let output = runner.join("ops-runtime");
    let wire = Wire {
        schema_version: 1,
        selection,
        artifact_id,
        output: output
            .to_str()
            .filter(|_| runner.is_absolute())
            .ok_or(Refusal::Unwritten)?,
        authentication: "gh-token",
    };
    let mut bytes = serde_json::to_vec(&wire).map_err(|_| Refusal::Unwritten)?;
    bytes.push(b'\n');
    let flags = OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let mut request = rustix::fs::open(runner.join(LEAF), flags, Mode::RUSR | Mode::WUSR)
        .map(File::from)
        .map_err(|_| Refusal::Unwritten)?;
    let written = request.write_all(&bytes);
    let closed = nix::unistd::close(request);
    if written.is_err() || closed.is_err() {
        return Err(Refusal::Unwritten);
    }
    let line = format!("sha256={}\n", super::super::sha(&bytes));
    super::step_output(inputs.step_output, line.as_bytes()).map_err(|_| Refusal::Unwritten)
}

/// `ops-artifact runtime-request`: read the step's environment, write the
/// request and report a refusal as one fixed line on standard error.
pub(super) fn native() -> ExitCode {
    let text = |name: &str| std::env::var(name).unwrap_or_default();
    let path = |name: &str| {
        std::env::var_os(name)
            .map(PathBuf::from)
            .unwrap_or_default()
    };
    let (revision, run) = (text("GITHUB_SHA"), text("OPS_RUN_ID"));
    let (attempt, artifact) = (text("OPS_RUN_ATTEMPT"), text("OPS_ARTIFACT_ID"));
    let (runner_temp, step_output) = (path("RUNNER_TEMP"), path("GITHUB_OUTPUT"));
    let inputs = Inputs {
        revision: &revision,
        run: &run,
        attempt: &attempt,
        artifact: &artifact,
        runner_temp: &runner_temp,
        step_output: &step_output,
    };
    match write(&inputs) {
        Ok(()) => ExitCode::SUCCESS,
        Err(refusal) => {
            eprintln!("{refusal}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
#[path = "request_writer_tests.rs"]
mod tests;
