//! Fixed bounded case context, never authentication or runtime authority.
//!
//! This separate prebuilt-controller operation starts no process or runtime. It
//! reads one keeper report and writes one fresh sanitized JSON leaf. Missing or
//! malformed reports remain unavailable; original runtime status is unchanged.

use super::super::namespace::{self, Budget, Chain, FLAGS, Held};
use super::{failure, io, receipt::CASES};
use crate::tooling::Result;
use rustix::fs::{Mode, OFlags};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use simplestchat_ops::distribution::manifest::Selection;
use std::{
    ffi::OsStr,
    fs::{File, Metadata},
    io::{Read, Write},
    os::{
        fd::{AsFd, BorrowedFd},
        unix::fs::FileExt,
    },
    path::{Path, PathBuf},
    process::ExitCode,
};

const MAX_DIAGNOSTIC: usize = 8192;
const SAFE: u64 = 9_007_199_254_740_991;
const LEAF: &str = "diagnostic.json";

fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(failure(1)) }
}
fn closed<T>(result: Result<T>, finish: Result<()>) -> Result<T> {
    result.and_then(|value| finish.map(|()| value))
}
struct Expected {
    selection: Value,
    artifact: u64,
}
impl Expected {
    fn new(revision: &str, run: &str, attempt: &str, artifact: &str) -> Result<Self> {
        let number = |value: &str| -> Result<u64> {
            require(
                !value.is_empty()
                    && value.len() <= 16
                    && !value.starts_with('0')
                    && value.bytes().all(|byte| byte.is_ascii_digit()),
            )?;
            let value = value.parse().map_err(|_| failure(1))?;
            require(value > 0 && value <= SAFE)?;
            Ok(value)
        };
        let selection = serde_json::json!({
            "repository":"swiftraccoon/simplestChat", "revision":revision,
            "packageVersion":env!("CARGO_PKG_VERSION"), "protocolVersion":1,
            "target":"x86_64-unknown-linux-musl", "buildRunId":number(run)?,
            "runAttempt":number(attempt)?,
        });
        let bytes = serde_json::to_vec(&selection).map_err(|_| failure(1))?;
        let selected = Selection::decode(&bytes).map_err(|_| failure(1))?;
        Ok(Self {
            selection: serde_json::to_value(selected.identity()).map_err(|_| failure(1))?,
            artifact: number(artifact)?,
        })
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Guard,
    Boot,
    Stage,
    Cases,
    Cleanup,
    Deadline,
    Cancelled,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Row {
    name: String,
    exit_status: u8,
    passed: bool,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Wire {
    schema_version: u8,
    scope: String,
    selection: Box<RawValue>,
    artifact_id: u64,
    passed: bool,
    failure: Option<Phase>,
    cases: Vec<Row>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    name: &'static str,
    exit_status: u8,
    exit_matched: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Diagnostic {
    schema_version: u8,
    scope: &'static str,
    diagnostic_only: bool,
    selection: Value,
    artifact_id: u64,
    runtime_step_outcome: &'static str,
    receipt_available: bool,
    phase: Value,
    expected_case_count: usize,
    observed_case_count: usize,
    last_observed_case: Option<&'static str>,
    last_exit_status: Option<u8>,
    last_exit_matched: Option<bool>,
    cases: Vec<Case>,
}
impl Diagnostic {
    fn unavailable(expected: &Expected, outcome: &str) -> Self {
        let runtime_step_outcome = match outcome {
            "success" => "success",
            "failure" => "failure",
            "cancelled" => "cancelled",
            "skipped" => "skipped",
            _ => "unknown",
        };
        Self {
            schema_version: 1,
            scope: "OpsRuntimeDiagnostics",
            diagnostic_only: true,
            selection: expected.selection.clone(),
            artifact_id: expected.artifact,
            runtime_step_outcome,
            receipt_available: false,
            phase: Value::from("unavailable"),
            expected_case_count: CASES.len(),
            observed_case_count: 0,
            last_observed_case: None,
            last_exit_status: None,
            last_exit_matched: None,
            cases: Vec::new(),
        }
    }
    fn from_bytes(bytes: &[u8], expected: &Expected, outcome: &str) -> Result<Self> {
        require(!bytes.is_empty() && bytes.len() as u64 <= super::SMALL)?;
        let wire: Wire = serde_json::from_slice(bytes).map_err(|_| failure(1))?;
        let selected =
            Selection::decode(wire.selection.get().as_bytes()).map_err(|_| failure(1))?;
        let selected = serde_json::to_value(selected.identity()).map_err(|_| failure(1))?;
        require(
            wire.schema_version == 1
                && wire.scope == "OpsDebianKeeperReceipt"
                && wire.artifact_id == expected.artifact
                && selected == expected.selection
                && wire.cases.len() <= CASES.len()
                && (wire.passed == wire.failure.is_none()),
        )?;
        let mut value = Self::unavailable(expected, outcome);
        for (row, &(name, status)) in wire.cases.iter().zip(CASES.iter()) {
            // The guest clamps a raw signal status to zero but preserves its original
            // status-match fact. A false fact need not disagree with the clamped byte.
            require(row.name == name && (!row.passed || row.exit_status == status))?;
            value.cases.push(Case {
                name,
                exit_status: row.exit_status,
                exit_matched: row.passed,
            });
        }
        require(
            !wire.passed
                || (value.cases.len() == CASES.len()
                    && value.cases.iter().all(|row| row.exit_matched)),
        )?;
        value.receipt_available = true;
        value.phase = wire.failure.map_or_else(
            || Ok(Value::from("complete")),
            |phase| serde_json::to_value(phase).map_err(|_| failure(1)),
        )?;
        value.observed_case_count = value.cases.len();
        if let Some(last) = value.cases.last() {
            value.last_observed_case = Some(last.name);
            value.last_exit_status = Some(last.exit_status);
            value.last_exit_matched = Some(last.exit_matched);
        }
        Ok(value)
    }
    fn console(&self) -> Value {
        serde_json::json!({
            "kind":"ops-runtime-diagnostics", "receiptAvailable":self.receipt_available,
            "phase":self.phase, "observedCaseCount":self.observed_case_count,
            "expectedCaseCount":self.expected_case_count,
            "lastObservedCase":self.last_observed_case,
            "lastExitStatus":self.last_exit_status, "lastExitMatched":self.last_exit_matched,
        })
    }
}

fn visible(directory: &File, name: &str, stamp: &Metadata, budget: &Budget) -> Result<()> {
    let leaf = Held::open(directory, OsStr::new(name), FLAGS, budget)?;
    let result = io(leaf.file().metadata()).and_then(|now| require(namespace::same(stamp, &now)));
    result.and(leaf.finish())
}
fn read_receipt(root: &Path, budget: &Budget) -> Result<Vec<u8>> {
    let directory = Chain::open(&root.join("ops-runtime/keeper"), budget)?;
    let result = (|| {
        let file = Held::open(directory.file(), OsStr::new("summary.json"), FLAGS, budget)?;
        let result = (|| {
            let before = namespace::private_file(&file, false)?;
            require(before.len() > 0 && before.len() <= super::SMALL)?;
            let mut bytes = Vec::new();
            let mut reader = file.file().take(super::SMALL + 1);
            io(reader.read_to_end(&mut bytes))?;
            require(
                bytes.len() as u64 == before.len()
                    && namespace::same(&before, &io(file.file().metadata())?),
            )?;
            visible(directory.file(), "summary.json", &before, budget)?;
            directory.verify()?;
            Ok(bytes)
        })();
        closed(result, file.finish())
    })();
    closed(result, directory.finish())
}
fn collect(root: &Path, expected: &Expected, outcome: &str, budget: &Budget) -> Diagnostic {
    read_receipt(root, budget)
        .and_then(|bytes| Diagnostic::from_bytes(&bytes, expected, outcome))
        .unwrap_or_else(|_| Diagnostic::unavailable(expected, outcome))
}
fn retain(root: &Path, diagnostic: &Diagnostic, budget: &Budget) -> Result<()> {
    let mut bytes = serde_json::to_vec(diagnostic).map_err(|_| failure(1))?;
    bytes.push(b'\n');
    require(bytes.len() <= MAX_DIAGNOSTIC)?;
    let parent = Chain::open(root, budget)?;
    let result = (|| {
        rustix::fs::mkdirat(parent.file(), "ops-runtime-diagnostics", Mode::RWXU)
            .map_err(|_| failure(1))?;
        let directory = Held::open(
            parent.file(),
            OsStr::new("ops-runtime-diagnostics"),
            FLAGS | OFlags::DIRECTORY,
            budget,
        )?;
        let result = (|| {
            namespace::private_directory(&directory)?;
            let file = Held::create(directory.file(), LEAF, false, budget)?;
            let result = (|| {
                io(file.file().write_all(&bytes))?;
                io(file.file().flush())?;
                io(file.file().sync_all())?;
                let after = namespace::private_file(&file, false)?;
                require(after.len() == bytes.len() as u64)?;
                let mut replay = vec![0; bytes.len()];
                io(file.file().read_exact_at(&mut replay, 0))?;
                require(replay == bytes && namespace::same(&after, &io(file.file().metadata())?))?;
                visible(directory.file(), LEAF, &after, budget)?;
                visible(
                    parent.file(),
                    "ops-runtime-diagnostics",
                    &namespace::private_directory(&directory)?,
                    budget,
                )?;
                parent.verify()
            })();
            result.and(file.finish())
        })();
        result.and(directory.finish())
    })();
    result.and(parent.finish())
}
fn marker(path: &Path) -> Result<()> {
    let mut file = rustix::fs::open(
        path,
        OFlags::WRONLY | OFlags::APPEND | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map(File::from)
    .map_err(|_| failure(1))?;
    let written = (|| {
        require(io(file.metadata())?.is_file())?;
        io(file.write_all(b"retained=true\n"))?;
        io(file.flush())
    })();
    let finish = nix::unistd::close(file).map_err(|_| failure(1));
    written.and(finish)
}

// StdoutLock swallows EBADF. Keep its lock and flush its existing buffer,
// then borrow the descriptor for checked kernel writes without owning it.
struct Descriptor<'a>(BorrowedFd<'a>);
impl Write for Descriptor<'_> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        rustix::io::write(self.0, bytes).map_err(std::io::Error::from)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn print_console(mut output: impl Write, bytes: &[u8]) -> std::io::Result<()> {
    output.write_all(bytes)?;
    output.flush()
}

pub(super) fn native() -> ExitCode {
    let mut console =
        serde_json::json!({"kind":"ops-runtime-diagnostics", "receiptAvailable":false});
    let _ = (|| -> Result<()> {
        let variable = |name| std::env::var(name).map_err(|_| failure(1));
        let selected = Expected::new(
            &variable("GITHUB_SHA")?,
            &variable("OPS_RUN_ID")?,
            &variable("OPS_RUN_ATTEMPT")?,
            &variable("OPS_ARTIFACT_ID")?,
        )?;
        let root = PathBuf::from(variable("RUNNER_TEMP")?);
        require(root.is_absolute())?;
        let root = io(root.canonicalize())?;
        let budget = Budget::default();
        let diagnostic = collect(
            &root,
            &selected,
            &std::env::var("OPS_RUNTIME_OUTCOME").unwrap_or_default(),
            &budget,
        );
        retain(&root, &diagnostic, &budget)?;
        marker(&PathBuf::from(variable("GITHUB_OUTPUT")?))?;
        console = diagnostic.console();
        Ok(())
    })();
    let mut bytes = serde_json::to_vec(&console).unwrap_or_else(|_| {
        b"{\"kind\":\"ops-runtime-diagnostics\",\"receiptAvailable\":false}".to_vec()
    });
    bytes.push(b'\n');
    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    if output
        .flush()
        .and_then(|()| print_console(Descriptor(output.as_fd()), &bytes))
        .is_ok()
    {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
#[path = "diagnostics_tests.rs"]
mod tests;
