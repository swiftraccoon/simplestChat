//! Genuine complete controller and original installed receiver reference comparison.
//! External tools are fixed inert executables; no SSH, TLS or crypto claim follows.

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
    path::PathBuf,
    process::{Command, Stdio},
};
use tokio::{
    io::AsyncReadExt,
    process::{ChildStderr, ChildStdout},
};

const WHOLE: Duration = Duration::from_secs(90);
const RESERVE: Duration = Duration::from_secs(10);
const PYTHON_BODY: Duration = Duration::from_secs(20);
const RETIRE: Duration = Duration::from_secs(25);
const DRAIN: Duration = Duration::from_secs(30);
const CAPTURE: usize = 262144;
const ERROR_CAPTURE: usize = 65536;
const FIXTURE_BYTES: u64 = 4 * 1024 * 1024;
const SCENARIOS: &[&str] = &[
    "success-fresh",
    "success-cached",
    "repeat-fresh",
    "repeat-cached",
    "existing-identical-pair",
    "existing-identical-manifest",
    "existing-identical-image",
    "metadata-refusal",
    "signature-first",
    "signature-fourth",
    "cached-member-changed",
    "root-required",
    "root-mode",
    "config-mode",
    "release-dir-mode",
    "results-mode",
    "work-mode",
    "lock-busy",
    "work-journal-unfinished",
    "release-journal-unfinished",
    "disk-insufficient",
    "grant-wire-invalid-host",
    "grant-wire-fragment",
    "grant-wire-empty-query",
    "grant-wire-oversize",
    "grant-api-failure",
    "url-wait-alarm",
    "http-status",
    "http-ce-gzip",
    "http-ce-duplicate-first-valid",
    "http-ce-duplicate-first-invalid",
    "http-cl-missing",
    "http-cl-duplicate-first-valid",
    "http-cl-duplicate-first-invalid",
    "http-truncated",
    "http-overflow",
    "http-checksum",
    "http-read-fault",
    "http-write-fault",
    "http-flush-fault",
    "http-fsync-fault",
    "zip-directory-cap",
    "zip-duplicate",
    "zip-unsafe",
    "zip-missing-selected",
    "zip-crc",
    "zip-truncated",
    "zip-method",
    "source-mutated",
    "outcome-failed",
    "manifest-revision",
    "bundle-mutated",
    "runtime-expired",
    "runtime-utc-error",
    "existing-manifest-diff",
    "existing-image-diff",
    "existing-symlink",
    "first-link-error",
    "second-link-error",
    "race-eexist-equal",
    "race-eexist-diff",
    "receipt-write-error",
    "receipt-fsync-error",
    "final-missing",
    "final-truncated",
    "final-nonzero",
    "final-extra-output",
    "final-ascii-whitespace",
    "final-non-ascii-whitespace",
    "final-identity-mismatch",
    "receiver-interrupt",
    "receiver-alarm",
    "cached-empty-directory",
];

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct Pin {
    sha256: String,
    bytes: u64,
    mode: u32,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Reference {
    schema_version: u8,
    scenario: String,
    input: Value,
    observation: Value,
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
                .join("ops/ansible/tests/release_fetch_controller_oracle.py")
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

fn json(path: &Path, end: Instant) -> io::Result<Value> {
    serde_json::from_slice(&bytes(path, FIXTURE_BYTES, end)?).map_err(io::Error::other)
}
fn field<'a>(value: &'a Value, key: &str) -> io::Result<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| error("closed oracle string field"))
}
fn array<'a>(value: &'a Value, key: &str) -> io::Result<&'a Vec<Value>> {
    value
        .get(key)
        .and_then(Value::as_array)
        .ok_or_else(|| error("closed oracle array field"))
}
fn normalize(value: &mut Value, root: &Path) {
    match value {
        Value::Object(object) => {
            object.remove("elapsedSeconds");
            object.remove("startedAt");
            for value in object.values_mut() {
                normalize(value, root);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, root);
            }
        }
        Value::String(text) => {
            *text = text.replace(root.to_str().unwrap(), "$FIXTURE");
            // Both original implementations generate private attempt names. All
            // path prefixes remain checked; only that random component differs.
            for prefix in ["download.", "release-fetch."] {
                while let Some(start) = text.find(prefix) {
                    let end = text[start..]
                        .find('/')
                        .map_or(text.len(), |offset| start + offset);
                    text.replace_range(
                        start..end,
                        if prefix == "download." {
                            "$ATTEMPT"
                        } else {
                            "$TRANSACTION"
                        },
                    );
                }
            }
        }
        _ => {}
    }
}
fn directory_files(directory: &Path, end: Instant) -> io::Result<Value> {
    if !directory.exists() {
        return Ok(serde_json::json!({}));
    }
    let mut result = serde_json::Map::new();
    for entry in fs::read_dir(directory)? {
        let path = entry?.path();
        let name = path
            .file_name()
            .and_then(OsStr::to_str)
            .ok_or_else(|| error("closed fixture filename"))?;
        if path.is_symlink() {
            result.insert(name.into(), serde_json::json!({"kind":"symlink"}));
        } else if path.is_file() {
            let (pin, _) = file_pin(&path, end)?;
            result.insert(
                name.into(),
                serde_json::json!({"bytes":pin.bytes,"sha256":pin.sha256,"mode":pin.mode}),
            );
        }
    }
    Ok(Value::Object(result))
}
fn controller_members(
    root: &Path,
    directory: &Path,
    files: &mut serde_json::Map<String, Value>,
    directories: &mut serde_json::Map<String, Value>,
    end: Instant,
) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        if Instant::now() >= end {
            return Err(error("local controller observation expired"));
        }
        let path = entry?.path();
        let name = path
            .strip_prefix(root)
            .ok()
            .and_then(Path::to_str)
            .ok_or_else(|| error("local controller filename"))?
            .to_owned();
        let metadata = path.symlink_metadata()?;
        if metadata.is_symlink() {
            files.insert(name, serde_json::json!({"kind":"symlink"}));
        } else if metadata.is_dir() {
            directories.insert(name, Value::from(metadata.mode() & 0o7777));
            controller_members(root, &path, files, directories, end)?;
        } else if metadata.is_file() {
            let (pin, _) = file_pin(&path, end)?;
            files.insert(
                name,
                serde_json::json!({"bytes":pin.bytes,"sha256":pin.sha256,"mode":pin.mode}),
            );
        } else {
            return Err(error("unexpected local controller member"));
        }
    }
    Ok(())
}
fn controller_evidence(root: &Path, end: Instant) -> io::Result<Value> {
    let parent = root.join("controller");
    let metadata = match parent.symlink_metadata() {
        Ok(value) if value.is_dir() => value,
        Ok(_) => return Err(error("unexpected local controller parent")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(serde_json::json!({"exists":false,"attempts":[]}));
        }
        Err(error) => return Err(error),
    };
    let mut attempts = Vec::new();
    for entry in fs::read_dir(&parent)? {
        let path = entry?.path();
        let metadata = path.symlink_metadata()?;
        if !metadata.is_dir()
            || !path
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("release-fetch."))
        {
            return Err(error("unexpected local controller attempt"));
        }
        let mut files = serde_json::Map::new();
        let mut directories = serde_json::Map::new();
        directories.insert("".into(), Value::from(metadata.mode() & 0o7777));
        controller_members(&path, &path, &mut files, &mut directories, end)?;
        let mut journals = serde_json::Map::new();
        for name in ["artifact.json", "outcome.json"] {
            let Some(pin) = files.remove(name) else {
                continue;
            };
            if pin.get("kind").is_some() {
                journals.insert(name.into(), pin);
                continue;
            }
            let data = bytes(&path.join(name), FIXTURE_BYTES, end)?;
            let maximum = usize::try_from(FIXTURE_BYTES)
                .map_err(|_| error("local controller fixture bound"))?;
            let decoded = crate::json::decode_limited(&data, maximum)
                .map_err(|_| ())
                .and_then(|_| serde_json::from_slice::<Value>(&data).map_err(|_| ()));
            let observed = match decoded {
                Ok(mut value) => {
                    normalize(&mut value, root);
                    serde_json::json!({"mode":pin["mode"],"value":value})
                }
                Err(()) => serde_json::json!({"partial":pin}),
            };
            journals.insert(name.into(), observed);
        }
        attempts.push(serde_json::json!({
            "files":files,"directories":directories,"journals":journals
        }));
    }
    attempts.sort_by_cached_key(Value::to_string);
    Ok(serde_json::json!({
        "exists":true,"mode":metadata.mode() & 0o7777,"attempts":attempts
    }))
}
fn reference_controller_report(mut value: Value) -> Value {
    // Only these observed original FetchError strings use the declared coarse
    // native controller class. Remote receipt classes are never normalized.
    if value["operation"] == "fetch_release_controller"
        && matches!(
            value["failureClass"].as_str(),
            Some(
                "artifact_identity_mismatch"
                    | "receiver_identity_mismatch"
                    | "receiver_closed_without_receipt"
                    | "receiver_extra_output"
            )
        )
    {
        value["failureClass"] = Value::from("controller_operation_failed");
    }
    value
}
fn reference_controller_evidence(value: &Value) -> Value {
    let mut value = value.clone();
    for attempt in value["attempts"].as_array_mut().unwrap() {
        if let Some(report) = attempt
            .get_mut("journals")
            .and_then(Value::as_object_mut)
            .and_then(|journals| journals.get_mut("outcome.json"))
            .and_then(|journal| journal.get_mut("value"))
        {
            *report = reference_controller_report(report.take());
        }
    }
    value["attempts"]
        .as_array_mut()
        .unwrap()
        .sort_by_cached_key(Value::to_string);
    value
}
fn attempts(root: &Path, end: Instant) -> io::Result<Vec<Value>> {
    let revision = root.join("remote/public/releases").join("a".repeat(40));
    let mut result = Vec::new();
    for entry in fs::read_dir(&revision)? {
        let attempt = entry?.path();
        if !attempt.is_dir()
            || !attempt
                .file_name()
                .and_then(OsStr::to_str)
                .is_some_and(|name| name.starts_with("download."))
        {
            continue;
        }
        let mut files = directory_files(&attempt, end)?;
        let mut journals = serde_json::Map::new();
        for name in ["envelope.json", "fetch-outcome.json"] {
            let path = attempt.join(name);
            if path.exists() {
                let mut value = match json(&path, end) {
                    Ok(value) => value,
                    Err(_) => serde_json::json!({"partial":true}),
                };
                normalize(&mut value, root);
                journals.insert(name.into(), value);
            }
            files.as_object_mut().unwrap().remove(name);
        }
        let mut links = serde_json::Map::new();
        for name in ["release.json", "image.tar"] {
            let target = revision.join(name);
            let retained = attempt.join(name);
            let linked = if target.exists() && !target.is_symlink() && retained.exists() {
                let target = target.metadata()?;
                let retained = retained.metadata()?;
                target.dev() == retained.dev() && target.ino() == retained.ino()
            } else {
                false
            };
            links.insert(name.into(), Value::Bool(linked));
        }
        result.push(serde_json::json!({
            "files":files,"journals":journals,"publicationLinks":links
        }));
    }
    result.sort_by_cached_key(Value::to_string);
    Ok(result)
}
fn lines(path: &Path, end: Instant) -> io::Result<Vec<Value>> {
    bytes(path, CAPTURE as u64, end)?
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice(line).map_err(io::Error::other))
        .collect()
}
fn reference_attempts(reference: &Reference) -> io::Result<Vec<Value>> {
    let mut attempts = array(&reference.observation, "attempts")?.clone();
    for attempt in &mut attempts {
        let files = attempt
            .get_mut("files")
            .and_then(Value::as_object_mut)
            .ok_or_else(|| error("reference attempt files"))?;
        files.remove("envelope.json");
        files.remove("fetch-outcome.json");
    }
    attempts.sort_by_cached_key(Value::to_string);
    Ok(attempts)
}
fn arguments(root: &Path, reference: &Reference) -> io::Result<Vec<OsString>> {
    let mut result = Vec::new();
    for (name, value) in [
        ("repository", "owner/repo".to_owned()),
        ("artifact-id", "123".to_owned()),
        ("revision", "a".repeat(40)),
        ("ci-run", "789".to_owned()),
        ("host", "inert.example".to_owned()),
        ("user", "root".to_owned()),
        (
            "identity",
            root.join("identity").to_str().unwrap().to_owned(),
        ),
        (
            "output-parent",
            root.join("controller").to_str().unwrap().to_owned(),
        ),
        ("timeout-seconds", WHOLE.as_secs().to_string()),
        ("finalization-seconds", RESERVE.as_secs().to_string()),
    ] {
        result.extend([format!("--{name}").into(), value.into()]);
    }
    if field(&reference.input, "branch")? == "cached" {
        result.extend([
            "--verified-directory".into(),
            if reference.scenario == "cached-empty-directory" {
                OsString::new()
            } else {
                root.join("candidate").into_os_string()
            },
        ]);
    }
    Ok(result)
}
struct Native {
    reports: Vec<Value>,
    parser_failure: Option<Cause>,
    primary: Option<Cause>,
    controller: Value,
    originals_before: BTreeMap<String, Original>,
    originals_after: BTreeMap<String, Original>,
    attempts: Vec<Value>,
    publication: Value,
    events: Vec<Value>,
    tools: Vec<Value>,
    closes: usize,
    fallback: usize,
    output_exists: bool,
}
async fn native(
    export: &Path,
    reference: &Reference,
    start: Instant,
    owner: &mut Owner,
) -> io::Result<Native> {
    if reference.schema_version != 1
        || field(&reference.input, "fixtureDirectory")? != "native"
        || field(&reference.input, "environment")? != "native/environment.json"
        || field(&reference.input, "today")? != "2050-01-01"
    {
        return Err(error("closed whole controller input"));
    }
    let end = start + WHOLE;
    let root = export.join("native");
    let originals_before = originals(&root.join("candidate"), end)?;
    let environment: BTreeMap<String, String> =
        serde_json::from_slice(&bytes(&root.join("environment.json"), 16384, end)?)
            .map_err(io::Error::other)?;
    let repeat = reference
        .input
        .get("repeat")
        .and_then(Value::as_u64)
        .filter(|n| matches!(*n, 1 | 2))
        .ok_or_else(|| error("closed repeat count"))?;
    let mut reports = Vec::new();
    let parsed = Options::parse(&arguments(&root, reference)?);
    let parser_failure = parsed.as_ref().err().map(|_| Cause::Input);
    files::start_probe(None);
    if let Ok(Some(mut options)) = parsed {
        options.environment = Some(
            environment
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect(),
        );
        owner.today = Some(chrono::NaiveDate::from_ymd_opt(2050, 1, 1).unwrap());
        for _ in 0..repeat {
            let bytes = execute(&options, start, owner).await;
            let mut report: Value = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            normalize(&mut report, &root);
            reports.push(report);
            if owner.error().is_some() {
                break;
            }
        }
    }
    let (closes, fallback) = files::take_probe();
    let originals_after = originals(&root.join("candidate"), end)?;
    Ok(Native {
        reports,
        parser_failure,
        primary: owner.error(),
        controller: controller_evidence(&root, end)?,
        originals_before,
        originals_after,
        attempts: attempts(&root, end)?,
        publication: directory_files(
            &root.join("remote/public/releases").join("a".repeat(40)),
            end,
        )?,
        events: lines(&root.join("events.jsonl"), end)?,
        tools: lines(&root.join("native-events.jsonl"), end)?,
        closes,
        fallback,
        output_exists: root.join("controller").exists(),
    })
}
fn count_roles(events: &[Value], role: &str) -> usize {
    events
        .iter()
        .filter(|row| row.get("role").and_then(Value::as_str) == Some(role))
        .count()
}
fn compare(native: Native, reference: Reference) {
    let name = reference.scenario.as_str();
    assert_eq!(
        native.originals_before, native.originals_after,
        "{name}: original11"
    );
    assert_eq!(native.fallback, 0, "{name}: explicit original closes");
    assert_eq!(reference.observation["sourceUnchanged"], true, "{name}");
    assert_eq!(
        reference.observation["originalPythonUnchanged"], true,
        "{name}"
    );
    assert_eq!(reference.observation["secretsAbsent"], true, "{name}");
    assert_eq!(reference.observation["stderrBytes"], 0, "{name}");
    let reference_events = reference.observation["events"].as_array().unwrap();
    let children = reference.observation["children"].as_array().unwrap();
    assert_eq!(
        children.len(),
        count_roles(reference_events, "ssh-start"),
        "{name}"
    );
    for child in children {
        assert_eq!(child["reaped"], true, "{name}: reference child consumed");
        assert_eq!(child["pipesClosed"], true, "{name}: reference child peers");
    }
    for connection in reference.observation["localConnections"]
        .as_array()
        .unwrap()
    {
        assert_eq!(connection["responseClosed"], true, "{name}: HTTP response");
        assert_eq!(connection["pipeClosed"], true, "{name}: HTTP pipe");
        assert!(
            connection["connectionCloses"]
                .as_u64()
                .is_some_and(|n| n > 0),
            "{name}"
        );
    }
    if name == "cached-empty-directory" {
        assert_eq!(native.parser_failure, Some(Cause::Input));
        assert!(native.reports.is_empty());
        assert!(native.attempts.is_empty());
        assert!(native.events.is_empty());
        assert!(native.tools.is_empty());
        assert!(!native.output_exists);
        assert_eq!(
            native.controller,
            serde_json::json!({"exists":false,"attempts":[]})
        );
        assert_eq!(reference.observation["passed"], true);
        return;
    }
    assert_eq!(native.parser_failure, None, "{name}: parser");
    assert!(native.closes > 0, "{name}: owned native inputs closed");
    assert_eq!(
        native.primary.is_none(),
        native.reports.iter().all(|report| report["passed"] == true),
        "{name}: original native owner"
    );
    let public = serde_json::to_vec(&native.reports).unwrap();
    for secret in [
        b"INERT_FETCH_CONTROLLER_TOKEN".as_slice(),
        b"INERT_LOCAL_GRANT".as_slice(),
        b"INERT_REMOTE_GRANT".as_slice(),
    ] {
        assert!(
            !public.windows(secret.len()).any(|window| window == secret),
            "{name}: native public reports omit credentials and grants"
        );
    }
    let expected = reference.observation["runs"].as_array().unwrap();
    assert_eq!(
        native.reports.len(),
        expected.len(),
        "{name}: whole repetitions"
    );
    for (actual, expected) in native.reports.iter().zip(expected) {
        let expected = reference_controller_report(expected["report"].clone());
        assert_eq!(*actual, expected, "{name}: complete controller report");
    }
    assert_eq!(
        native.controller,
        reference_controller_evidence(&reference.observation["controllerEvidence"]),
        "{name}: local controller journals, modes and complete retained effects"
    );
    assert_eq!(
        native.attempts,
        reference_attempts(&reference).unwrap(),
        "{name}: attempts"
    );
    let expected_publication: serde_json::Map<String, Value> = reference.observation["publication"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(key, _)| matches!(key.as_str(), "release.json" | "image.tar"))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    assert_eq!(
        native.publication,
        Value::Object(expected_publication),
        "{name}: publication"
    );
    let events = reference.observation["events"].as_array().unwrap();
    for (actual, expected) in [
        ("api-artifact", "metadata-artifact"),
        ("api-run", "metadata-run"),
        ("api-redirect", "grant-local"),
        ("blob", "local-http"),
    ] {
        let expected = count_roles(events, expected)
            + if actual == "api-redirect" {
                count_roles(events, "grant-remote")
            } else {
                0
            };
        assert_eq!(
            count_roles(&native.tools, actual),
            expected,
            "{name}: {actual}"
        );
    }
    for subject in crate::release::trust::SUBJECTS {
        let expected = events
            .iter()
            .filter(|row| row["role"] == "verify" && row["subject"].as_str() == Some(subject))
            .count();
        assert_eq!(
            count_roles(&native.tools, &format!("verify-{subject}")),
            expected,
            "{name}"
        );
    }
    for role in [
        "ssh-start",
        "receiver-ready",
        "receiver-final",
        "remote-http",
        "remote-http-closed",
        "ssh-finished",
        "remote-file-mutated",
    ] {
        assert_eq!(
            count_roles(&native.events, role),
            count_roles(events, role),
            "{name}: {role}"
        );
    }
    for row in &native.events {
        if row["role"] == "ssh-finished" {
            assert_eq!(
                row["lockAvailableAfter"], true,
                "{name}: workload lock released"
            );
            if let Some(values) = row["lockHeldAtReceipt"].as_array() {
                assert!(
                    values.iter().all(|value| value == true),
                    "{name}: receipt lock"
                );
            }
        }
    }
}
#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_fetch_controller_matches_unchanged_python() {
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
                    .filter(|b| **b == b'\n')
                    .count()
                    == 1;
            let observed = if ready {
                match serde_json::from_slice::<Reference>(&python.captures.output) {
                    Ok(reference) if reference.scenario == *scenario => {
                        native(&root, &reference, start, &mut owner)
                            .await
                            .map(|native| (native, reference))
                    }
                    _ => Err(error("closed whole controller response")),
                }
            } else {
                Err(error("bounded original controller/receiver owner"))
            };
            drop(owner);
            drop(fixture);
            // All native State/Signals/groups/pipes/File originals and the
            // Python owner are settled before any result assertion can panic.
            assert!(ready, "{scenario}: original reference owner");
            let (native, reference) =
                observed.unwrap_or_else(|error| panic!("{scenario}: {error}"));
            compare(native, reference);
        }
    });
    drop(runtime);
}
