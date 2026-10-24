//! Whole fresh/cached release CLI policy against the unchanged Python main.
//!
//! Original HTTP/ZIP/file/crypto policy runs on real bounded fixture bytes. The
//! native side owns inert gh/curl processes; this proves no network/signatures.

use super::super::{control::LateClock, files};
use super::*;
use crate::{owned_process, test_support::Fixture};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::{
        fs::MetadataExt,
        process::{CommandExt, ExitStatusExt},
    },
    process::{Command, Stdio},
};
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
};

const WHOLE: Duration = Duration::from_secs(60);
const RESERVE: Duration = Duration::from_secs(10);
const PYTHON_BODY: Duration = Duration::from_secs(20);
const RETIRE: Duration = Duration::from_secs(25);
const DRAIN: Duration = Duration::from_secs(30);
const CAPTURE: usize = 131072;
const ERROR_CAPTURE: usize = 65536;
const FIXTURE_BYTES: u64 = 2 * 1024 * 1024;
const SCENARIOS: &[&str] = &[
    "fresh-success",
    "cached-success",
    "fresh-repeat",
    "cached-repeat",
    "run-attempt-large",
    "identifiers-max",
    "api-artifact-id",
    "api-artifact-expired",
    "api-artifact-expired-integer",
    "api-artifact-name",
    "api-size-zero",
    "api-size-boolean",
    "api-digest-missing",
    "api-digest-uppercase",
    "api-association-run",
    "api-association-revision",
    "api-association-fork",
    "api-association-boolean-id",
    "api-run-id",
    "api-run-branch",
    "api-run-attempt-zero",
    "api-run-attempt-boolean",
    "api-run-attempt-max-plus-one",
    "api-run-status",
    "api-run-conclusion",
    "api-run-event",
    "api-run-name",
    "api-run-path",
    "api-run-repository",
    "api-run-head-repository",
    "api-run-revision",
    "api-repository-case",
    "api-command-failure",
    "api-json-malformed",
    "token-command-failure",
    "token-empty",
    "token-spaced-positive",
    "token-control",
    "token-invalid-utf8",
    "token-too-large",
    "token-non-ascii",
    "redirect-status301",
    "redirect-status200",
    "redirect-location-missing",
    "redirect-location-duplicate",
    "redirect-location-leading-space",
    "redirect-location-trailing-space",
    "redirect-location-folded",
    "redirect-hostcase",
    "redirect-userinfo",
    "redirect-port",
    "redirect-fragment",
    "redirect-http",
    "redirect-query-missing",
    "redirect-path-percent",
    "redirect-empty-fragment",
    "redirect-multiple-fragment",
    "redirect-100",
    "redirect-103",
    "redirect-body-ignored",
    "blob-status302",
    "blob-idle-gap",
    "blob-deadline",
    "blob-content-encoding-gzip",
    "blob-content-encoding-case",
    "blob-content-encoding-duplicate",
    "blob-length-missing",
    "blob-length-wrong",
    "blob-length-duplicate",
    "blob-length-leading-space",
    "blob-length-trailing-space",
    "blob-length-folded",
    "blob-100",
    "blob-103",
    "blob-truncated",
    "blob-extra-with-length",
    "blob-extra-no-length",
    "blob-digest",
    "blob-read-fault",
    "blob-partial-truncated",
    "blob-partial-extra",
    "zip-crc",
    "zip-unsafe-name",
    "cached-member-changed",
    "cached-zip-changed",
    "gh-verifier-failure-first",
    "gh-verifier-failure-last",
    "late-source",
    "late-outcome",
    "late-manifest",
    "required",
    "optional-utc-error",
    "late-utc-error",
    "late-expiry",
    "disk-insufficient",
    "output-parent-relative",
    "output-parent-symlink",
    "cached-directory-symlink",
    "selector-artifact-plus",
    "selector-ci-underscores",
    "cached-output-parent-relative",
    "cached-directory-relative",
    "artifact-dir-empty",
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
struct HttpPin {
    role: String,
    path: String,
    sha256: String,
    bytes: u64,
    body_offset: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SelectorText {
    artifact_id: String,
    ci_run_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentFile {
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    branch: String,
    selectors: Value,
    repeat: usize,
    today: String,
    clock: String,
    rust_profile: Option<String>,
    native_expected_passed: Option<bool>,
    socket_idle_seconds: Option<u64>,
    selector_text: SelectorText,
    environment: EnvironmentFile,
    disk_free: Option<u64>,
    process_responses: Vec<ResponsePin>,
    http_responses: Vec<HttpPin>,
}
#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OutputTree {
    output_parent_exists: bool,
    output_parent_symlink: bool,
    transaction_directories: usize,
    candidate_directory_exists: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Observation {
    passed: bool,
    exit_codes: Vec<i32>,
    results: Vec<Option<Value>>,
    uncaught_classes: Vec<Option<String>>,
    api_calls: Vec<Value>,
    verifier_calls: Vec<Value>,
    http_calls: Vec<Value>,
    files: BTreeMap<String, Pin>,
    file_snapshots: Vec<BTreeMap<String, Pin>>,
    output_tree: OutputTree,
    cached_originals_unchanged: bool,
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
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    role: String,
    status: i32,
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
                .join("ops/ansible/tests/release_fresh_verify_oracle.py")
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
fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error("oracle string field"))
}
fn environment(
    root: &Path,
    input: &Input,
    end: Instant,
) -> io::Result<BTreeMap<OsString, OsString>> {
    if input.environment.path != "environment.json" {
        return Err(error("closed environment fixture"));
    }
    let data = bytes(&root.join(&input.environment.path), 16384, end)?;
    let values: BTreeMap<String, String> =
        serde_json::from_slice(&data).map_err(io::Error::other)?;
    Ok(values
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect())
}
fn validate_inputs(root: &Path, input: &Input, end: Instant) -> io::Result<()> {
    if input.process_responses.len() != 7
        || input.http_responses.len() != 2
        || !matches!(input.repeat, 1 | 2)
        || input.today != "2050-01-01"
        || !matches!(input.branch.as_str(), "fresh" | "cached")
    {
        return Err(error("closed fresh fixture cardinality"));
    }
    let selection: Value =
        serde_json::from_slice(&bytes(&root.join("selection.json"), 16384, end)?)
            .map_err(io::Error::other)?;
    for key in ["repository", "revision", "artifactId", "ciRunId"] {
        if selection.get(key) != input.selectors.get(key) {
            return Err(error("original selector fixture"));
        }
    }
    for (index, pin) in input.process_responses.iter().enumerate() {
        if pin.path != format!("responses/process-{index:02}.bin") || !matches!(pin.status, 0 | 7) {
            return Err(error("closed gh response fixture"));
        }
        let _ = pinned(root, &pin.path, pin.bytes, &pin.sha256, end)?;
    }
    for (pin, role) in input.http_responses.iter().zip(["api", "blob"]) {
        if pin.role != role
            || pin.path != format!("responses/{role}.http")
            || pin.body_offset > pin.bytes
        {
            return Err(error("closed raw HTTP response fixture"));
        }
        let _ = pinned(root, &pin.path, pin.bytes, &pin.sha256, end)?;
    }
    Ok(())
}
fn arguments(root: &Path, input: &Input, scenario: &str) -> io::Result<Vec<OsString>> {
    let parent: OsString = if matches!(
        scenario,
        "output-parent-relative" | "cached-output-parent-relative"
    ) {
        "relative-output".into()
    } else {
        root.join("output").into_os_string()
    };
    let mut args = vec![
        "--repository".into(),
        field(&input.selectors, "repository")?.into(),
        "--revision".into(),
        field(&input.selectors, "revision")?.into(),
        "--artifact-id".into(),
        input.selector_text.artifact_id.clone().into(),
        "--ci-run".into(),
        input.selector_text.ci_run_id.clone().into(),
        "--output-parent".into(),
        parent,
        "--timeout-seconds".into(),
        WHOLE.as_secs().to_string().into(),
        "--finalization-seconds".into(),
        RESERVE.as_secs().to_string().into(),
    ];
    if input.branch == "cached" {
        let candidate: OsString = match scenario {
            "cached-directory-relative" => "candidate".into(),
            "cached-directory-symlink" => root.join("cached-link").into_os_string(),
            _ => root.join("candidate").into_os_string(),
        };
        args.extend(["--artifact-dir".into(), candidate]);
    } else if scenario == "artifact-dir-empty" {
        args.extend(["--artifact-dir".into(), "".into()]);
    }
    Ok(args)
}
fn attempts(root: &Path) -> io::Result<Vec<PathBuf>> {
    let path = root.join("output");
    if !path.exists() || path.is_symlink() {
        return Ok(Vec::new());
    }
    fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect()
}
fn existing_snapshot(path: Option<&Path>, end: Instant) -> io::Result<BTreeMap<String, Pin>> {
    match path {
        Some(path) if path.is_dir() => snapshot(path, end),
        _ => Ok(BTreeMap::new()),
    }
}
fn events(root: &Path, end: Instant) -> io::Result<Vec<Event>> {
    let data = bytes(&root.join("native-events.jsonl"), 65536, end)?;
    if data.last().is_some_and(|byte| *byte != b'\n') {
        return Err(error("complete inert invocation journal"));
    }
    data.split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(io::Error::other))
        .collect()
}
struct Native {
    results: Vec<Result<Vec<u8>>>,
    parse_failure: Option<Cause>,
    snapshots: Vec<BTreeMap<String, Pin>>,
    files: BTreeMap<String, Pin>,
    tree: OutputTree,
    originals_before: BTreeMap<String, Original>,
    originals_after: BTreeMap<String, Original>,
    events: Vec<Event>,
    closes: (usize, usize),
    download: Pin,
    download_after: Pin,
}
async fn native(
    root: &Path,
    reference: &Reference,
    owner: &mut Owner,
    end: Instant,
) -> io::Result<Native> {
    let input = &reference.input;
    validate_inputs(root, input, end)?;
    let before = originals(&root.join("candidate"), end)?;
    let download = file_pin(&root.join("download.zip"), end)?.0;
    let arguments = arguments(root, input, &reference.scenario)?;
    let parsed = Options::parse(&arguments);
    let (mut options, parse_failure) = match parsed {
        Ok(Some(mut options)) => {
            options.environment = Some(environment(root, input, end)?);
            options.free_bytes = input.disk_free;
            options.expire_after = (reference.scenario == "blob-deadline").then_some(65536);
            (Some(options), None)
        }
        Ok(None) => return Err(error("oracle execution cannot select help")),
        Err(cause) => {
            owner.record(cause);
            (None, Some(cause))
        }
    };
    owner.today = chrono::NaiveDate::from_ymd_opt(2050, 1, 1);
    match input.clock.as_str() {
        "fixed" | "download-deadline" => {}
        "error" => owner.utc_error = true,
        "late-error" => owner.late_clock = Some(LateClock::Failure),
        "late-expiry" => {
            owner.late_clock = chrono::NaiveDate::from_ymd_opt(2050, 1, 2).map(LateClock::Date)
        }
        _ => return Err(error("closed UTC fixture")),
    }
    let mut results = Vec::new();
    let mut snapshots = Vec::new();
    let mut candidate = (input.branch == "cached").then(|| root.join("candidate"));
    files::start_probe(None);
    if let Some(options) = options.as_mut() {
        for _ in 0..input.repeat {
            let previous = attempts(root)?;
            let result = execute(options, owner).await;
            let failed = result.is_err();
            if let Err(cause) = &result {
                owner.record(*cause);
            }
            results.push(result);
            if input.branch == "fresh" {
                let created: Vec<_> = attempts(root)?
                    .into_iter()
                    .filter(|path| !previous.contains(path))
                    .collect();
                if created.len() == 1 {
                    candidate = Some(created[0].join("artifact"));
                }
            }
            snapshots.push(existing_snapshot(candidate.as_deref(), end)?);
            if failed {
                break;
            }
        }
    }
    let closes = files::take_probe();
    let after = originals(&root.join("candidate"), end)?;
    let download_after = file_pin(&root.join("download.zip"), end)?.0;
    let files = existing_snapshot(candidate.as_deref(), end)?;
    let output = root.join("output");
    let tree = OutputTree {
        output_parent_exists: output.exists(),
        output_parent_symlink: output.is_symlink(),
        transaction_directories: attempts(root)?.len(),
        candidate_directory_exists: candidate.as_ref().is_some_and(|path| path.is_dir()),
    };
    let events = events(root, end)?;
    let _ = owner.check(Window::Final);
    Ok(Native {
        results,
        parse_failure,
        snapshots,
        files,
        tree,
        originals_before: before,
        originals_after: after,
        events,
        closes,
        download,
        download_after,
    })
}
fn expected_roles(calls: &[Value], key: &str, prefix: &str) -> Vec<(String, i64)> {
    calls
        .iter()
        .map(|row| {
            (
                format!("{prefix}{}", row[key].as_str().unwrap()),
                row["status"].as_i64().unwrap_or(0),
            )
        })
        .collect()
}
fn observed_roles(events: &[Event], selected: impl Fn(&str) -> bool) -> Vec<(String, i64)> {
    events
        .iter()
        .filter(|event| selected(&event.role))
        .map(|event| (event.role.clone(), i64::from(event.status)))
        .collect()
}
fn compare_roles(native: &Native, reference: &Reference, same: bool) {
    let expected_api = expected_roles(&reference.observation.api_calls, "role", "");
    let expected_crypto =
        expected_roles(&reference.observation.verifier_calls, "subject", "verify-");
    let expected_http: Vec<_> = reference
        .observation
        .http_calls
        .iter()
        .map(|row| (row["role"].as_str().unwrap().to_owned(), 0))
        .collect();
    let actual_api = observed_roles(&native.events, |role| {
        (role.starts_with("api-") && role != "api-redirect") || role == "auth-token"
    });
    let actual_crypto = observed_roles(&native.events, |role| role.starts_with("verify-"));
    let actual_http = observed_roles(&native.events, |role| {
        matches!(role, "api-redirect" | "blob")
    });
    assert!(
        !native
            .events
            .iter()
            .any(|event| event.role == "refused-adapter-contract"),
        "{}: actual fixed argv/config/environment rejected",
        reference.scenario
    );
    for (actual, expected) in [
        (&actual_api, &expected_api),
        (&actual_crypto, &expected_crypto),
    ] {
        if same {
            assert_eq!(actual, expected, "{}: original roles", reference.scenario);
        } else {
            assert!(
                expected.starts_with(actual),
                "{}: negative native prefix",
                reference.scenario
            );
        }
    }
    // The named read-fault adapter exits7 after its real first64KiB prefix.
    // Original HTTP rows describe response status instead of child status.
    let actual_http: Vec<_> = actual_http.into_iter().map(|(role, _)| (role, 0)).collect();
    if same {
        assert_eq!(
            actual_http, expected_http,
            "{}: HTTP roles",
            reference.scenario
        );
    } else {
        assert!(
            expected_http.starts_with(&actual_http),
            "{}: HTTP prefix",
            reference.scenario
        );
    }
    if reference.input.branch == "cached" {
        assert!(!native.events.iter().any(|event| matches!(
            event.role.as_str(),
            "auth-token" | "api-redirect" | "blob" | "curl-version"
        )));
    }
}
fn compare(native: Native, reference: Reference, primary: Option<Cause>) {
    let scenario = reference.scenario.as_str();
    assert_eq!(reference.schema_version, 1);
    assert_eq!(reference.input.today, "2050-01-01");
    assert_eq!(
        native.originals_before, native.originals_after,
        "{scenario}: checked original source inventory"
    );
    assert!(reference.observation.cached_originals_unchanged);
    assert_eq!(
        native.download, native.download_after,
        "{scenario}: original exported download source"
    );
    assert_eq!(
        native.closes.1, 0,
        "{scenario}: checked finalizers, no fallback"
    );
    let passed =
        native.results.len() == reference.input.repeat && native.results.iter().all(Result::is_ok);
    if let Some(profile) = reference.input.rust_profile.as_deref() {
        let expected = reference.input.native_expected_passed.unwrap();
        assert_eq!(passed, expected, "{scenario}: explicit {profile}");
        assert_ne!(
            passed, reference.observation.passed,
            "{scenario}: accepted-domain difference"
        );
        if scenario == "blob-idle-gap" {
            assert_eq!(reference.input.socket_idle_seconds, Some(16));
            assert_eq!(native.files.len(), 11);
            assert_eq!(native.files["artifact.zip"], native.download);
            let crypto = observed_roles(&native.events, |role| role.starts_with("verify-"));
            assert_eq!(
                crypto,
                crate::release::trust::SUBJECTS.map(|subject| (format!("verify-{subject}"), 0))
            );
            let result = native.results[0].as_ref().unwrap();
            let value: Value = serde_json::from_slice(result).unwrap();
            assert_eq!(value.as_object().unwrap().len(), 4);
            assert_eq!(value["revision"], reference.input.selectors["revision"]);
            assert!(Path::new(value["directory"].as_str().unwrap()).is_absolute());
        } else if reference.input.branch == "cached" {
            assert_eq!(
                native.originals_before.len(),
                11,
                "{scenario}: real exported cached originals"
            );
            assert_eq!(
                native.files, reference.observation.files,
                "{scenario}: original cached evidence remains intact"
            );
            compare_roles(&native, &reference, false);
        } else {
            assert_eq!(profile, "ascii-positive-decimal-selectors");
            assert!(matches!(
                scenario,
                "selector-artifact-plus" | "selector-ci-underscores"
            ));
            assert_eq!(
                native.parse_failure,
                Some(Cause::Input),
                "{scenario}: actual parser refusal before native dispatch"
            );
            assert_eq!(primary, Some(Cause::Input));
            assert!(
                native.results.is_empty(),
                "{scenario}: no execute call or emitted operation result"
            );
            assert!(
                native.originals_before.is_empty(),
                "{scenario}: fresh export has no cached candidate files"
            );
            assert!(
                native.events.is_empty(),
                "{scenario}: no native command admitted"
            );
            assert!(
                native.files.is_empty() && native.snapshots.is_empty(),
                "{scenario}: no native artifact output or extraction"
            );
            assert_eq!(
                native.tree,
                OutputTree {
                    output_parent_exists: false,
                    output_parent_symlink: false,
                    transaction_directories: 0,
                    candidate_directory_exists: false,
                },
                "{scenario}: no native output parent, transaction or artifact directory"
            );
            assert_eq!(
                reference.observation.files.len(),
                11,
                "{scenario}: reference-only accepted syntax extracts all signed members"
            );
        }
        assert_eq!(primary.is_none(), passed, "{scenario}: original owner");
        return;
    }
    assert_eq!(
        passed, reference.observation.passed,
        "{scenario}: {:?}",
        native.results
    );
    assert_eq!(
        primary.is_none(),
        passed,
        "{scenario}: final original owner"
    );
    assert_eq!(
        native.files, reference.observation.files,
        "{scenario}: exact retained effects"
    );
    assert_eq!(
        native.snapshots, reference.observation.file_snapshots,
        "{scenario}: every-use effects"
    );
    assert_eq!(
        native.tree, reference.observation.output_tree,
        "{scenario}: creation ordering"
    );
    assert_eq!(
        native
            .results
            .iter()
            .map(|result| i32::from(result.is_err()))
            .collect::<Vec<_>>(),
        reference.observation.exit_codes,
        "{scenario}: every-use outcome"
    );
    assert_eq!(
        reference.observation.uncaught_classes.len(),
        reference.observation.exit_codes.len()
    );
    compare_roles(&native, &reference, true);
    if !native.events.is_empty() {
        assert!(native.closes.0 > 0, "{scenario}: original handles closed");
    }
    for (actual, expected) in native.results.iter().zip(&reference.observation.results) {
        match (actual, expected) {
            (Ok(data), Some(expected)) => {
                let mut value: Value = serde_json::from_slice(data).unwrap();
                let object = value.as_object_mut().unwrap();
                assert_eq!(object.len(), 4, "{scenario}: complete result");
                assert!(Path::new(object["directory"].as_str().unwrap()).is_absolute());
                object.insert("directory".into(), Value::String("$CANDIDATE".into()));
                assert_eq!(
                    &value, expected,
                    "{scenario}: exact four passive result keys"
                );
            }
            (Err(_), _) => {}
            _ => panic!("{scenario}: complete result shape"),
        }
    }
    if scenario == "optional-utc-error" {
        assert_eq!(reference.observation.utc_samples, 0, "optional UTC is lazy");
    }
    if scenario == "required" {
        assert_eq!(
            reference.observation.utc_samples, 2,
            "both original fresh UTC checks"
        );
    }
}
#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_fresh_release_matches_unchanged_python() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for scenario in SCENARIOS {
            // Fixture export and its Python process spend this SAME case budget.
            let start = Instant::now();
            let mut owner = Owner::new(start, WHOLE, RESERVE).unwrap();
            let fixture = Fixture::new();
            let root = fixture.0.join("input");
            let python = python(&mut owner, start, &root, scenario).await;
            let ready = python.retired
                && python.reaped
                && python.closed == (true, true)
                && python.captures.eof()
                && python.status == Some(0)
                && python.captures.errors.is_empty()
                && owner.error().is_none()
                && python.captures.output.last() == Some(&b'\n')
                && python
                    .captures
                    .output
                    .iter()
                    .filter(|byte| **byte == b'\n')
                    .count()
                    == 1;
            let observation = if ready {
                match serde_json::from_slice::<Reference>(&python.captures.output) {
                    Ok(reference) if reference.scenario == *scenario => {
                        native(&root, &reference, &mut owner, start + WHOLE)
                            .await
                            .map(|native| (native, reference))
                    }
                    _ => Err(error("whole oracle schema/scenario")),
                }
            } else {
                Err(error("bounded Python oracle process settlement"))
            };
            let primary = owner.error();
            drop(owner);
            // Every File helper closes originals on error as well as success;
            // State, Signals, process groups/pipes and fixture retire first.
            drop(fixture);
            assert!(
                ready,
                "{scenario}: Python owner before any reference verdict"
            );
            let (native, reference) =
                observation.unwrap_or_else(|cause| panic!("{scenario}: {cause}"));
            compare(native, reference, primary);
        }
    });
    drop(runtime);
}
