//! Whole cached verification against the unchanged pinned-Python file oracle.
//!
//! Real ZIP/archive/trust/receiver policy runs on bounded private inputs. Both
//! sides use inert GH observations: this proves no signatures, network or hosts.

use super::super::{
    control::{Cause, Owner, Result, Window},
    files,
};
use super::*;
use crate::{owned_process, test_support::Fixture};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    fs::{self, File, Metadata, OpenOptions},
    io::{self, Read, Write},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt},
        process::CommandExt,
        process::ExitStatusExt,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
};

const WHOLE: Duration = Duration::from_secs(45);
const RESERVE: Duration = Duration::from_secs(10);
const PYTHON_BODY: Duration = Duration::from_secs(20);
const RETIRE: Duration = Duration::from_secs(25);
const DRAIN: Duration = Duration::from_secs(30);
const CAPTURE: usize = 131072;
const ERROR_CAPTURE: usize = 65536;
const FIXTURE_BYTES: u64 = 2 * 1024 * 1024;
const SCENARIOS: &[&str] = &[
    "success",
    "repeat-fresh",
    "required",
    "attempt-large",
    "identifiers-max",
    "optional-utc-error",
    "required-utc-error",
    "late-utc-error",
    "late-expiry",
    "expiry-today",
    "expiry-yesterday",
    "expiry-invalid",
    "expiry-unicode",
    "runtime-required-integer",
    "runtime-schema-boolean",
    "runtime-passed-integer",
    "runtime-image",
    "runtime-required-mismatch",
    "runtime-hash",
    "vex-hash",
    "required-scope",
    "required-evidence",
    "required-vex-empty",
    "optional-vex-nonempty",
    "security-passed-integer",
    "security-image",
    "security-sbom",
    "build-passed",
    "build-image",
    "manifest-archive",
    "predicate-schema-boolean",
    "predicate-extra",
    "predicate-digest",
    "predicate-attempt",
    "encoding-predicate-utf16",
    "encoding-security-bom",
    "ignored-large-integer",
    "ignored-bool-null",
    "late-source",
    "late-outcome-error",
    "late-manifest-date",
    "late-archive",
    "gh-failure-first",
    "gh-failure-second",
    "gh-invalid-json",
    "gh-duplicate-json",
    "gh-empty",
    "gh-multiple",
    "gh-no-timestamp",
    "gh-statement-type",
    "gh-predicate-type",
    "gh-claim",
    "gh-subject-missing",
    "gh-subject-duplicate",
    "gh-subject-extra",
    "gh-subject-reordered",
    "gh-claim-schema-bool",
    "gh-claim-run-float",
    "gh-large-integer",
    "gh-utf16",
    "gh-surrogate",
    "gh-certificate-subjectAlternativeName",
    "gh-certificate-issuer",
    "gh-certificate-buildSignerDigest",
    "gh-certificate-sourceRepositoryURI",
    "gh-certificate-sourceRepositoryDigest",
    "gh-certificate-sourceRepositoryRef",
    "gh-certificate-runInvocationURI",
    "gh-certificate-runnerEnvironment",
    "gh-certificate-buildTrigger",
    "mutate-bundle",
    "mutate-source",
    "mutate-outcome",
    "mutate-manifest",
    "mutate-zip",
    "exchange-bundle",
    "zip-store",
    "zip-store-trailing",
    "zip-deflate-extra-output",
    "zip-comment",
    "zip-local-zip64",
    "zip-central-zip64",
    "zip-extra-opaque",
    "zip-extra-bad-crc",
    "zip-directory-data",
    "zip-prefix-files",
    "zip-large-central-record",
    "zip-local-metadata-diff",
    "zip-local-flags-diff",
    "zip-selected-patched-flag",
    "zip-selected-strong-flag",
    "zip-unselected-flags",
    "zip-shared-offset-extra-first",
    "zip-shared-offset-extra-last",
    "zip-shared-offset-wide-extent",
    "zip-eocd-count-diff",
    "zip-selected-crc",
    "zip-local-name",
    "zip-duplicate",
    "zip-traversal",
    "zip-backslash",
    "zip-missing",
    "zip-empty-selected",
    "zip-bad-comment",
    "zip-eocd-zip64",
    "zip-trailing",
    "zip-symlink",
    "zip-method",
    "zip-encrypted-extra",
    "zip-mode",
    "zip-unicode-override",
];

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Pin {
    sha256: String,
    bytes: u64,
    mode: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ResponsePin {
    path: String,
    sha256: String,
    bytes: u64,
    status: i32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Mutation {
    after_call: usize,
    target: String,
    kind: String,
    path: String,
    bytes: u64,
    sha256: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    selection: Value,
    today: String,
    repeat: usize,
    responses: Vec<ResponsePin>,
    mutation: Option<Mutation>,
    clock: String,
    rust_profile: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Call {
    subject: String,
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    timeout_seconds: f64,
    stdout_limit: u64,
    stderr_limit: u64,
    status: i32,
    response_sha256: String,
    response_bytes: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Observation {
    passed: bool,
    passes: Vec<bool>,
    results: Vec<Option<Value>>,
    calls: Vec<Call>,
    candidate_unchanged: bool,
    files: BTreeMap<String, Pin>,
    failure_class: Option<String>,
    utc_samples: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Reference {
    schema_version: u8,
    scenario: String,
    input: Input,
    observation: Observation,
}

struct Captures {
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
    out_buffer: Vec<u8>,
    err_buffer: Vec<u8>,
    output: Vec<u8>,
    errors: Vec<u8>,
    out_eof: bool,
    err_eof: bool,
}
impl Captures {
    fn new() -> Self {
        Self {
            stdout: None,
            stderr: None,
            out_buffer: vec![0; 8192],
            err_buffer: vec![0; 8192],
            output: Vec::new(),
            errors: Vec::new(),
            out_eof: false,
            err_eof: false,
        }
    }
    fn eof(&self) -> bool {
        self.out_eof && self.err_eof
    }
    async fn read_one(&mut self, owner: &mut Owner) {
        // Both buffers remain owned and nonempty across every losing poll. No
        // future cancellation can turn a resumed read into an empty-buffer EOF.
        let out_active = self.stdout.is_some() && !self.out_eof;
        let err_active = self.stderr.is_some() && !self.err_eof;
        let event = {
            let (output, errors) = (&mut self.stdout, &mut self.stderr);
            let (out_buffer, err_buffer) = (&mut self.out_buffer, &mut self.err_buffer);
            tokio::select! {
                result = async { output.as_mut().unwrap().read(out_buffer.as_mut_slice()).await },
                    if out_active => Some((true, result)),
                result = async { errors.as_mut().unwrap().read(err_buffer.as_mut_slice()).await },
                    if err_active => Some((false, result)),
                _ = tokio::time::sleep(Duration::from_millis(5)) => None,
            }
        };
        match event {
            Some((stdout, Ok(0))) => {
                if stdout {
                    self.out_eof = true;
                } else {
                    self.err_eof = true;
                }
            }
            Some((stdout, Ok(count))) => {
                let (bytes, buffer) = if stdout {
                    (&mut self.output, &self.out_buffer)
                } else {
                    (&mut self.errors, &self.err_buffer)
                };
                let maximum = if stdout { CAPTURE } else { ERROR_CAPTURE };
                if bytes.len().checked_add(count).is_some_and(|n| n <= maximum) {
                    bytes.extend_from_slice(&buffer[..count]);
                } else {
                    owner.record(Cause::Policy);
                }
            }
            Some((_, Err(_))) => owner.record(Cause::Io),
            None => {}
        }
    }
    fn close(&mut self, owner: &mut Owner) -> (bool, bool) {
        let out = self.stdout.take().is_some_and(|pipe| {
            pipe.into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(io::Error::from))
                .is_ok()
        });
        let err = self.stderr.take().is_some_and(|pipe| {
            pipe.into_owned_fd()
                .and_then(|fd| nix::unistd::close(fd).map_err(io::Error::from))
                .is_ok()
        });
        if !out || !err {
            owner.record(Cause::Close);
        }
        (out, err)
    }
}
struct Python {
    captures: Captures,
    retired: bool,
    reaped: bool,
    closed: (bool, bool),
    status: Option<i32>,
}
async fn python(owner: &mut Owner, start: Instant, fixture: &Path, scenario: &str) -> Python {
    let mut result = Python {
        captures: Captures::new(),
        retired: false,
        reaped: false,
        closed: (false, false),
        status: None,
    };
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    if owner.check(Window::Body).is_err() || Instant::now() >= start + PYTHON_BODY {
        owner.record(Cause::Deadline);
        return result;
    }
    let mut command = Command::new(prefix.join("bin/python3"));
    command
        .args([
            OsString::from("-B"),
            repository
                .join("ops/ansible/tests/release_verify_oracle.py")
                .into_os_string(),
            "--scenario".into(),
            scenario.into(),
            "--fixture-directory".into(),
            fixture.as_os_str().into(),
        ])
        .current_dir(repository)
        .env_clear()
        .env("PATH", prefix.join("bin"))
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let spawned = command.spawn();
    drop(command);
    let mut group = match spawned {
        Ok(child) => owned_process::Group::new(child),
        Err(_) => {
            owner.record(Cause::Io);
            return result;
        }
    };
    let out = group.child().stdout.take().expect("created piped stdout");
    let err = group.child().stderr.take().expect("created piped stderr");
    // Conversion refusal remains negative; attempt the independent pipe and
    // every group finalizer even if Tokio consumed one rejected argument.
    match ChildStdout::from_std(out) {
        Ok(pipe) => result.captures.stdout = Some(pipe),
        Err(_) => owner.record(Cause::Io),
    }
    match ChildStderr::from_std(err) {
        Ok(pipe) => result.captures.stderr = Some(pipe),
        Err(_) => owner.record(Cause::Io),
    }
    while owner.check(Window::Body).is_ok() {
        if Instant::now() >= start + PYTHON_BODY {
            owner.record(Cause::Deadline);
            break;
        }
        match owned_process::exited_now(group.pid()) {
            Ok(true) if result.captures.eof() => break,
            Ok(_) => {}
            Err(_) => {
                owner.record(Cause::Io);
                break;
            }
        }
        result.captures.read_one(owner).await;
    }
    // Keep the NOWAIT reservation until all exact-group signals finish. A
    // consuming reap/close error never discards another admitted finalizer.
    result.retired = owned_process::finish_before(group.pid(), start + RETIRE).await;
    if !result.retired {
        owner.record(Cause::Close);
    }
    while Instant::now() < start + RETIRE {
        let _ = owner.check(Window::Final);
        match owned_process::exited_now(group.pid()) {
            Ok(true) => break,
            Ok(false) => result.captures.read_one(owner).await,
            Err(_) => {
                owner.record(Cause::Io);
                break;
            }
        }
    }
    // As with the production owner, blocked kernel wait latency is outside a
    // hard wall guarantee. Still await the one admitted consuming wait.
    match group.reap().await {
        Ok(status) => {
            result.reaped = true;
            result.status = status.code().or_else(|| status.signal().map(|s| -s));
            if result.status != Some(0) {
                owner.record(Cause::Io);
            }
        }
        Err(_) => owner.record(Cause::Close),
    }
    if Instant::now() >= start + RETIRE {
        owner.record(Cause::Deadline);
    }
    while !result.captures.eof() && Instant::now() < start + DRAIN {
        let _ = owner.check(Window::Final);
        result.captures.read_one(owner).await;
    }
    if !result.captures.eof() {
        owner.record(Cause::Close);
    }
    result.closed = result.captures.close(owner);
    if Instant::now() >= start + DRAIN {
        owner.record(Cause::Deadline);
    }
    let _ = owner.check(Window::Body);
    result
}

fn error(message: &'static str) -> io::Error {
    io::Error::other(message)
}
fn file_pin(path: &Path, end: Instant) -> io::Result<(Pin, Metadata)> {
    let mut file = File::open(path)?;
    let observed = (|| {
        let before = file.metadata()?;
        if !before.is_file() || before.len() > FIXTURE_BYTES {
            return Err(error("oracle artifact fixture bound"));
        }
        let mut buffer = vec![0_u8; 65536];
        let mut digest = Sha256::new();
        let mut bytes = 0_u64;
        loop {
            if Instant::now() >= end {
                return Err(error("oracle artifact observation expired"));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            bytes = bytes
                .checked_add(count.try_into().map_err(|_| error("oracle count"))?)
                .filter(|n| *n <= FIXTURE_BYTES)
                .ok_or_else(|| error("oracle artifact fixture bound"))?;
            digest.update(&buffer[..count]);
        }
        let after = file.metadata()?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
            || bytes != after.len()
        {
            return Err(error("oracle artifact changed while observed"));
        }
        Ok((
            Pin {
                sha256: files::hex(&digest.finalize()),
                bytes,
                mode: after.mode() & 0o7777,
            },
            after,
        ))
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    observed.and_then(|value| closed.map(|()| value))
}
fn snapshot(directory: &Path, end: Instant) -> io::Result<BTreeMap<String, Pin>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| error("oracle artifact filename"))?
            .to_owned();
        result.insert(name, file_pin(&path, end)?.0);
    }
    Ok(result)
}
#[derive(Debug, PartialEq, Eq)]
struct Original {
    pin: Pin,
    device: u64,
    inode: u64,
    modified: i64,
    modified_nanos: i64,
    changed: i64,
    changed_nanos: i64,
}
fn originals(directory: &Path, end: Instant) -> io::Result<BTreeMap<String, Original>> {
    let mut result = BTreeMap::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| error("original fixture name"))?
            .to_owned();
        let (pin, metadata) = file_pin(&path, end)?;
        result.insert(
            name,
            Original {
                pin,
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: metadata.mtime(),
                modified_nanos: metadata.mtime_nsec(),
                changed: metadata.ctime(),
                changed_nanos: metadata.ctime_nsec(),
            },
        );
    }
    Ok(result)
}
fn bytes(path: &Path, maximum: u64, end: Instant) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let result = (|| {
        if file.metadata()?.len() > maximum {
            return Err(error("oracle input bound"));
        }
        let mut output = Vec::new();
        let mut buffer = vec![0; 65536];
        loop {
            if Instant::now() >= end {
                return Err(error("oracle input expired"));
            }
            let count = file.read(&mut buffer)?;
            if count == 0 {
                return Ok(output);
            }
            if (output.len() as u64)
                .checked_add(count as u64)
                .is_none_or(|n| n > maximum)
            {
                return Err(error("oracle input bound"));
            }
            output.extend_from_slice(&buffer[..count]);
        }
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    result.and_then(|data| closed.map(|()| data))
}
fn pinned(
    root: &Path,
    path: &str,
    expected: u64,
    sha256: &str,
    end: Instant,
) -> io::Result<Vec<u8>> {
    if !Path::new(path)
        .components()
        .all(|part| matches!(part, std::path::Component::Normal(_)))
    {
        return Err(error("oracle fixture relative name"));
    }
    let data = bytes(&root.join(path), FIXTURE_BYTES, end)?;
    if data.len() as u64 != expected || files::hex(&Sha256::digest(&data)) != sha256 {
        return Err(error("oracle fixture pin"));
    }
    Ok(data)
}
fn environment(root: &Path) -> BTreeMap<OsString, OsString> {
    let mut result: BTreeMap<OsString, OsString> = [
        ("PATH", root.join("bin").into_os_string()),
        ("HOME", root.join("home").into_os_string()),
        ("USER", "fixture".into()),
        ("LOGNAME", "fixture".into()),
        ("GH_TOKEN", "INERT_GH_TOKEN".into()),
        ("GITHUB_TOKEN", "INERT_GITHUB_TOKEN".into()),
        ("XDG_CONFIG_HOME", root.join("config").into_os_string()),
        ("LC_ALL", "must-be-overridden".into()),
        ("UNRELATED", "must-not-inherit".into()),
    ]
    .into_iter()
    .map(|(key, value)| (key.into(), value))
    .collect();
    // An inherited locale variable must not leak through the exact allowlist.
    result.insert("LANG".into(), "must-not-inherit".into());
    result
}
struct Observed {
    arguments: Vec<String>,
    environment: BTreeMap<String, String>,
    program: bool,
    bounded: bool,
    response_sha256: String,
    response_bytes: u64,
    status: i32,
}
struct Inert<'a> {
    root: &'a Path,
    input: &'a Input,
    responses: Vec<Vec<u8>>,
    changed: Option<Vec<u8>>,
    calls: Vec<Observed>,
}
impl<'a> Inert<'a> {
    fn new(root: &'a Path, input: &'a Input, end: Instant) -> io::Result<Self> {
        if input.responses.len() != 4 || !matches!(input.repeat, 1 | 2) {
            return Err(error("closed verifier fixture cardinality"));
        }
        let responses = input
            .responses
            .iter()
            .map(|p| pinned(root, &p.path, p.bytes, &p.sha256, end))
            .collect::<io::Result<_>>()?;
        let changed = input
            .mutation
            .as_ref()
            .map(|m| pinned(root, &m.path, m.bytes, &m.sha256, end))
            .transpose()?;
        Ok(Self {
            root,
            input,
            responses,
            changed,
            calls: Vec::new(),
        })
    }
    fn mutate(&self) -> Result<()> {
        let Some(mutation) = &self.input.mutation else {
            return Ok(());
        };
        if self.calls.len() != mutation.after_call {
            return Ok(());
        }
        if mutation.after_call != 4
            || ![
                "artifact.zip",
                "release-attestation.jsonl",
                "source.json",
                "outcome.json",
                "release.json",
            ]
            .contains(&mutation.target.as_str())
        {
            return Err(Cause::Input);
        }
        let data = self.changed.as_deref().ok_or(Cause::Input)?;
        let target = self.root.join("candidate").join(&mutation.target);
        let exchange = match mutation.kind.as_str() {
            "exchange" => true,
            "replace" => false,
            _ => return Err(Cause::Input),
        };
        let path = if exchange {
            self.root.join("candidate/.replacement")
        } else {
            target.clone()
        };
        let mut options = OpenOptions::new();
        options.write(true).mode(0o600);
        if exchange {
            options.create_new(true);
        } else {
            options.truncate(true);
        }
        let mut file = options.open(&path).map_err(|_| Cause::Io)?;
        let written = file.write_all(data).map_err(|_| Cause::Io);
        let closed = nix::unistd::close(file).map_err(|_| Cause::Close);
        written.and(closed)?;
        if exchange {
            fs::rename(path, target).map_err(|_| Cause::Io)?;
        }
        Ok(())
    }
}
impl Executor for Inert<'_> {
    async fn run(&mut self, owner: &mut Owner, invocation: process::Invocation) -> Result<Vec<u8>> {
        let index = self.calls.len();
        if index >= self.input.repeat * 4 {
            owner.record(Cause::Policy);
            return Err(Cause::Policy);
        }
        let candidate = self.root.join("candidate");
        let prefix = candidate.to_str().ok_or(Cause::Input)?;
        let mut arguments = vec!["gh".to_owned()];
        for argument in &invocation.arguments {
            arguments.push(
                argument
                    .to_str()
                    .ok_or(Cause::Input)?
                    .replace(prefix, "$CANDIDATE"),
            );
        }
        let mut environment = BTreeMap::new();
        for (key, value) in &invocation.environment {
            environment.insert(
                key.to_str().ok_or(Cause::Input)?.to_owned(),
                value
                    .to_str()
                    .ok_or(Cause::Input)?
                    .replace(self.root.to_str().ok_or(Cause::Input)?, "$FIXTURE"),
            );
        }
        let now = owner.now();
        let response = self.responses[index % 4].clone();
        let status = self.input.responses[index % 4].status;
        self.calls.push(Observed {
            arguments,
            environment,
            program: invocation.program == self.root.join("bin/gh"),
            bounded: now < invocation.body_end
                && Some(invocation.body_end)
                    == owner.body_end().checked_sub(Duration::from_secs(10))
                && invocation.body_end <= now + Duration::from_secs(180)
                && invocation.body_end < invocation.settlement_end
                && invocation.settlement_end == owner.body_end()
                && invocation.settlement_end < owner.end(),
            response_sha256: files::hex(&Sha256::digest(&response)),
            response_bytes: response.len() as u64,
            status,
        });
        if self.calls.len() == 4 {
            match self.input.clock.as_str() {
                "late-error" => owner.utc_error = true,
                "late-expiry" => owner.today = chrono::NaiveDate::from_ymd_opt(2050, 1, 2),
                "fixed" | "error" => {}
                _ => {
                    owner.record(Cause::Input);
                    return Err(Cause::Input);
                }
            }
        }
        if let Err(cause) = self.mutate() {
            owner.record(cause);
            return Err(cause);
        }
        if status != 0 {
            owner.record(Cause::Policy);
            Err(Cause::Policy)
        } else {
            Ok(response)
        }
    }
}
fn profile(scenario: &str) -> Option<&'static str> {
    match scenario {
        "mutate-zip" => Some("retained-originals"),
        "exchange-bundle" => Some("original-inode"),
        "gh-surrogate" => Some("unicode-scalar"),
        _ => None,
    }
}
fn options(root: &Path, selected: &Input, end: Instant) -> io::Result<Options> {
    let data = bytes(&root.join("metadata.json"), 16384, end)?;
    if serde_json::from_slice::<Value>(&data).map_err(io::Error::other)? != selected.selection {
        return Err(error("original nine-field selection"));
    }
    let encoded = String::from_utf8(data).map_err(io::Error::other)?;
    let mut result = Options::parse(&[
        "--directory".into(),
        root.join("candidate").into_os_string(),
        "--selection-json".into(),
        encoded.into(),
        "--timeout-seconds".into(),
        WHOLE.as_secs().to_string().into(),
        "--finalization-seconds".into(),
        RESERVE.as_secs().to_string().into(),
    ])
    .map_err(|_| error("oracle selection option"))?
    .ok_or_else(|| error("oracle help is not execution"))?;
    result.environment = Some(environment(root));
    Ok(result)
}
fn compare_calls(actual: &[Observed], expected: &[Call], same_count: bool, scenario: &str) {
    if same_count {
        assert_eq!(actual.len(), expected.len(), "{scenario}: fresh calls");
    } else {
        assert!(
            actual.len() <= expected.len(),
            "{scenario}: bounded dispatched prefix"
        );
    }
    for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
        assert!(
            actual.program && actual.bounded,
            "{scenario}: retained GH/budget {index}"
        );
        assert_eq!(
            actual.arguments, expected.arguments,
            "{scenario}: argv {index}"
        );
        assert_eq!(
            actual.environment, expected.environment,
            "{scenario}: allowlist {index}"
        );
        assert_eq!(
            actual.arguments.get(3).map(String::as_str),
            Some(format!("$CANDIDATE/{}", expected.subject).as_str())
        );
        assert_eq!(expected.timeout_seconds, 180.0);
        assert_eq!(
            (expected.stdout_limit, expected.stderr_limit),
            (8 * 1024 * 1024, 65536)
        );
        assert_eq!(
            actual.response_sha256, expected.response_sha256,
            "{scenario}: raw stdout"
        );
        assert_eq!(actual.response_bytes, expected.response_bytes);
        assert_eq!(actual.status, expected.status);
    }
}

#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_verify_scenarios_match_unchanged_python() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for scenario in SCENARIOS {
            let start = Instant::now();
            let mut owner = Owner::new(start, WHOLE, RESERVE).unwrap();
            let fixture = Fixture::new();
            let root = fixture.0.join("input");
            let python = python(&mut owner, start, &root, scenario).await;
            let python_error = owner.error();
            // The Python owner has retired/reaped its one group and closed both
            // actual pipes before parsing any oracle-supplied data or verdict.
            assert!(python.retired && python.reaped, "{scenario}");
            assert_eq!(python.closed, (true, true), "{scenario}");
            assert!(python.captures.eof(), "{scenario}");
            assert_eq!(python.status, Some(0), "{scenario}");
            assert!(python.captures.errors.is_empty(), "{scenario}");
            assert_eq!(python_error, None, "{scenario}");
            assert_eq!(python.captures.output.last(), Some(&b'\n'));
            assert_eq!(
                python
                    .captures
                    .output
                    .iter()
                    .filter(|b| **b == b'\n')
                    .count(),
                1
            );
            let reference: Reference = serde_json::from_slice(&python.captures.output).unwrap();
            assert_eq!(reference.schema_version, 1);
            assert_eq!(reference.scenario, *scenario);
            assert_eq!(reference.input.today, "2050-01-01");
            assert_eq!(reference.input.rust_profile.as_deref(), profile(scenario));
            let candidate = root.join("candidate");
            let before = originals(&candidate, start + WHOLE);
            let options = options(&root, &reference.input, start + WHOLE);
            let inert = Inert::new(&root, &reference.input, start + WHOLE);
            // Fixture readback File owners have already attempted checked
            // closes on all outcomes; admission failures do not abandon State.
            if before.is_err() || options.is_err() || inert.is_err() {
                owner.record(Cause::Io);
            }
            owner.today = chrono::NaiveDate::from_ymd_opt(2050, 1, 1);
            owner.utc_error = reference.input.clock == "error";
            let mut results = Vec::new();
            let mut executor = inert.ok();
            files::start_probe(None);
            if let (Ok(options), Some(executor)) = (options, executor.as_mut()) {
                for _ in 0..reference.input.repeat {
                    let result = execute(&options, &mut owner, executor).await;
                    let failed = result.is_err();
                    if let Err(cause) = &result {
                        owner.record(*cause);
                    }
                    results.push(result);
                    if failed {
                        break;
                    }
                }
            }
            let probes = files::take_probe();
            let after = originals(&candidate, start + WHOLE);
            let observed = snapshot(&candidate, start + WHOLE);
            let _ = owner.check(Window::Final);
            let primary = owner.error();
            drop(owner);
            // Production State.finish, every test File read, and original signal
            // registration are gone before assertions about native verdicts.
            assert_eq!(probes.1, 0, "{scenario}: no descriptor fallback");
            assert!(probes.0 > 0, "{scenario}: actual retained owners closed");
            let before = before.unwrap();
            let after = after.unwrap();
            assert_eq!(
                before == after,
                reference.observation.candidate_unchanged,
                "{scenario}"
            );
            assert_eq!(
                observed.unwrap(),
                reference.observation.files,
                "{scenario}: effects"
            );
            let executor = executor.unwrap();
            let passed =
                results.len() == reference.input.repeat && results.iter().all(Result::is_ok);
            if reference.input.rust_profile.is_some() {
                assert!(
                    reference.observation.passed,
                    "{scenario}: explicit reference domain"
                );
                assert!(
                    !passed && primary.is_some(),
                    "{scenario}: retained native profile"
                );
                compare_calls(
                    &executor.calls,
                    &reference.observation.calls,
                    false,
                    scenario,
                );
                continue;
            }
            assert_eq!(
                passed, reference.observation.passed,
                "{scenario}: {results:?}"
            );
            assert_eq!(
                primary.is_none(),
                passed,
                "{scenario}: final original owner"
            );
            assert_eq!(
                results.iter().map(Result::is_ok).collect::<Vec<_>>(),
                reference.observation.passes,
                "{scenario}"
            );
            // Concurrent owned mutations may be caught earlier by the native
            // original witness; never require additional dispatch after loss.
            compare_calls(
                &executor.calls,
                &reference.observation.calls,
                reference.input.mutation.is_none(),
                scenario,
            );
            assert_eq!(
                reference.observation.failure_class.as_deref(),
                (!passed).then_some("release_attestation_failed")
            );
            if reference.input.clock == "error" && passed {
                assert_eq!(
                    reference.observation.utc_samples, 0,
                    "optional UTC stays lazy"
                );
            }
            for (actual, expected) in results.iter().zip(&reference.observation.results) {
                match (actual, expected) {
                    (Ok(bytes), Some(expected)) => {
                        let actual: Value = serde_json::from_slice(bytes).unwrap();
                        assert_eq!(
                            &actual, expected,
                            "{scenario}: complete fresh passive proof"
                        );
                        assert_eq!(actual.as_object().unwrap().len(), 10);
                    }
                    (Err(_), None) => {}
                    _ => panic!("{scenario}: complete result shape"),
                }
            }
        }
    });
    drop(runtime);
}
