//! Genuine complete unchanged host receiver comparison; no TLS/authentication claim.
use super::*;
use crate::{owned_process, test_support::Fixture};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::{
        fs::MetadataExt,
        process::{CommandExt, ExitStatusExt},
    },
    path::{Path, PathBuf},
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
    "success",
    "repeat",
    "identical-pair",
    "identical-manifest",
    "identical-image",
    "id-i64",
    "predicate-semantic-equal",
    "wire-trailing",
    "envelope-missing-key",
    "envelope-extra-key",
    "envelope-schema-bool",
    "envelope-id-bool",
    "envelope-id-zero",
    "envelope-id-overflow",
    "envelope-revision",
    "envelope-proof",
    "frame-empty",
    "frame-no-lf",
    "frame-oversize",
    "frame-duplicate",
    "frame-utf16-be",
    "url-invalid-host",
    "url-fragment",
    "url-oversize",
    "root-required",
    "root-mode",
    "config-mode",
    "releases-mode",
    "results-mode",
    "work-mode",
    "lock-mode",
    "lock-busy",
    "work-journal-unfinished",
    "release-journal-unfinished",
    "journal-schema-bool",
    "disk-insufficient",
    "http-status-404",
    "http-status-302",
    "http-status-201",
    "http-ce-gzip",
    "http-ce-first-valid",
    "http-ce-first-invalid",
    "http-cl-missing",
    "http-cl-first-valid",
    "http-cl-first-invalid",
    "http-cl-leading-zero",
    "http-truncated",
    "http-extra-ignored",
    "http-overflow",
    "http-checksum",
    "http-read-error",
    "http-write-error",
    "http-flush-error",
    "http-fsync-error",
    "zip-directory-cap",
    "zip-duplicate",
    "zip-unsafe",
    "zip-missing",
    "zip-crc",
    "zip-truncated",
    "zip-method",
    "zip-encrypted",
    "zip-empty-selected",
    "zip-extra-opaque",
    "zip-extra-bad-crc",
    "zip-directory-data",
    "zip-prefix-files",
    "zip-local-metadata-diff",
    "zip-local-flags-diff",
    "source-revision",
    "source-keys",
    "source-hash",
    "outcome-passed-int",
    "outcome-error-missing",
    "outcome-error-value",
    "manifest-revision",
    "manifest-timestamp",
    "bundle-mutated",
    "predicate-mismatch",
    "security-passed-int",
    "runtime-required",
    "runtime-expired",
    "runtime-clock-error",
    "vex-cardinality",
    "ignored-large-int",
    "existing-manifest-diff",
    "existing-image-diff",
    "existing-symlink",
    "first-link-error",
    "second-link-error",
    "race-eexist-equal",
    "race-eexist-diff",
    "publish-fsync-error",
    "extract-write-error",
    "receipt-create-error",
    "receipt-fsync-error",
    "final-write-error",
    "final-flush-error",
    "receiver-interrupt",
    "receiver-alarm",
    "frame-syntax",
    "race-eexist-hardlink",
    "frame-exact-limit",
    "url-host-domain",
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
                .join("ops/ansible/tests/release_receive_oracle.py")
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
            if Instant::now() >= end {
                return Err(error("oracle original read completed late"));
            }
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
            || Instant::now() >= end
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
            if Instant::now() >= end {
                return Err(error("oracle original read completed late"));
            }
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
            for value in object.values_mut() {
                normalize(value, root);
            }
        }
        Value::Array(values) => {
            for value in values {
                normalize(value, root);
            }
        }
        Value::String(text) if text.starts_with(root.to_str().unwrap()) => {
            let relative = Path::new(text).strip_prefix(root).unwrap();
            *text = "/fixture/".to_owned()
                + &relative
                    .components()
                    .map(|part| {
                        let part = part.as_os_str().to_str().unwrap();
                        if part.starts_with("download.") {
                            "download.ATTEMPT"
                        } else {
                            part
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("/");
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
        if !path.is_symlink() && path.is_file() {
            let (pin, _) = file_pin(&path, end)?;
            result.insert(
                name.into(),
                serde_json::json!({"bytes":pin.bytes,"sha256":pin.sha256,"mode":pin.mode}),
            );
        }
    }
    Ok(Value::Object(result))
}
fn attempts(root: &Path, end: Instant) -> io::Result<Vec<Value>> {
    let revision = root.join("public/releases").join("a".repeat(40));
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
        result
            .push(serde_json::json!({"files":files,"journals":journals,"publicationLinks":links}));
    }
    result.sort_by_cached_key(Value::to_string);
    Ok(result)
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
struct Wire {
    frames: VecDeque<Vec<u8>>,
    records: Vec<Value>,
    reads: usize,
    ready: bool,
    finished: bool,
    scenario: String,
}
impl Channel for Wire {
    async fn line(&mut self, owner: &mut Owner) -> Result<Vec<u8>> {
        owner.check(Window::Body)?;
        if self.reads >= 2 || self.reads == 1 && !self.ready {
            return control::failed(owner, Cause::Input);
        }
        self.reads += 1;
        let mut bytes = self.frames.pop_front().unwrap_or_default();
        if let Some(lf) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.truncate(lf + 1);
        }
        if bytes.is_empty() || bytes.len() > 16384 || bytes.last() != Some(&b'\n') {
            return control::failed(owner, Cause::Input);
        }
        tokio::task::yield_now().await;
        owner.check(Window::Body)?;
        Ok(bytes)
    }
    async fn write(&mut self, bytes: &[u8], final_output: bool, owner: &mut Owner) -> Result<()> {
        if final_output {
            owner.final_work()?;
        } else {
            owner.check(Window::Body)?;
        }
        if final_output && self.scenario == "final-write-error" {
            return control::failed(owner, Cause::Io);
        }
        let record: Value = serde_json::from_slice(bytes).map_err(|_| Cause::Policy)?;
        self.records.push(record);
        if final_output && self.scenario == "final-flush-error" {
            return control::failed(owner, Cause::Io);
        }
        if !final_output {
            self.ready = true;
        }
        tokio::task::yield_now().await;
        if final_output {
            owner.final_work()
        } else {
            owner.check(Window::Body)
        }
    }
    fn finish(&mut self, _owner: &mut Owner) {
        self.finished = true;
    }
}
fn fault(scenario: &str) -> Option<Fault> {
    Some(match scenario {
        "http-read-error" => Fault::HttpRead,
        "http-write-error" => Fault::Write,
        "http-flush-error" => Fault::Flush,
        "http-fsync-error" => Fault::Fsync,
        "extract-write-error" => Fault::ExtractWrite,
        "first-link-error" => Fault::FirstLink,
        "second-link-error" => Fault::SecondLink,
        "race-eexist-equal" => Fault::RaceEqual,
        "race-eexist-diff" => Fault::RaceDifferent,
        "race-eexist-hardlink" => Fault::RaceHardlink,
        "publish-fsync-error" => Fault::PublishSync,
        "receipt-create-error" => Fault::ReceiptCreate,
        "receipt-fsync-error" => Fault::ReceiptSync,
        "receiver-interrupt" => Fault::Interrupt,
        "receiver-alarm" => Fault::Alarm,
        _ => return None,
    })
}
struct Native {
    passed: bool,
    codes: Vec<u8>,
    records: Vec<Value>,
    reads: usize,
    channels_finished: bool,
    attempts: Vec<Value>,
    publication: Value,
    originals_before: BTreeMap<String, Original>,
    originals_after: BTreeMap<String, Original>,
    primary: Option<Cause>,
    closes: usize,
    fallback: usize,
    lock_released: bool,
}
async fn native(
    export: &Path,
    reference: &Reference,
    start: Instant,
    owner: &mut Owner,
) -> io::Result<Native> {
    use std::os::fd::AsFd;
    if reference.schema_version != 1 || field(&reference.input, "today")? != "2050-01-01" {
        return Err(error("closed host receiver input"));
    }
    let end = start + WHOLE;
    let root = export.join("native");
    let originals_before = originals(&root.join("inputs"), end)?;
    let first = bytes(&root.join("inputs/envelope.bin"), FIXTURE_BYTES, end)?;
    let second = bytes(&root.join("inputs/url.bin"), FIXTURE_BYTES, end)?;
    let current_uid = rustix::process::geteuid().as_raw();
    let access = Access {
        root: root.join("public"),
        config: root.join("config"),
        work: root.join("work"),
        ca: root.join("ca.pem"),
        uid: if reference.scenario == "root-required" {
            current_uid
                .checked_add(1)
                .ok_or_else(|| error("fixture uid range"))?
        } else {
            current_uid
        },
        environment: [
            (OsString::from("PATH"), root.join("bin").into_os_string()),
            ("HOME".into(), root.join("config").into_os_string()),
        ]
        .into_iter()
        .collect(),
        free_bytes: Some(if reference.scenario == "disk-insufficient" {
            0
        } else {
            1 << 50
        }),
        fault: fault(&reference.scenario),
    };
    let repeat = reference
        .input
        .get("repeat")
        .and_then(Value::as_u64)
        .filter(|n| matches!(*n, 1 | 2))
        .ok_or_else(|| error("closed host repeat count"))?;
    let mut held_lock = None;
    let lock_ready = if reference.scenario == "lock-busy" {
        match File::open(root.join("work/workload.lock")) {
            Ok(file) => {
                held_lock = Some(file);
                rustix::fs::flock(
                    held_lock.as_ref().unwrap().as_fd(),
                    rustix::fs::FlockOperation::NonBlockingLockExclusive,
                )
                .map_err(io::Error::from)
            }
            Err(error) => Err(error),
        }
    } else {
        Ok(())
    };
    owner.today = Some(chrono::NaiveDate::from_ymd_opt(2050, 1, 1).unwrap());
    if reference.scenario == "runtime-expired" {
        owner.today = Some(chrono::NaiveDate::from_ymd_opt(2050, 1, 2).unwrap());
    }
    owner.utc_error = reference.scenario == "runtime-clock-error";
    let mut records = Vec::new();
    let mut codes = Vec::new();
    let mut reads = 0;
    let mut channels_finished = true;
    files::start_probe(None);
    if lock_ready.is_ok() {
        for _ in 0..repeat {
            let mut wire = Wire {
                frames: [first.clone(), second.clone()].into(),
                records: Vec::new(),
                reads: 0,
                ready: false,
                finished: false,
                scenario: reference.scenario.clone(),
            };
            let passed = execute(&access, &mut wire, owner, start).await;
            codes.push(if passed { 0 } else { 1 });
            reads += wire.reads;
            channels_finished &= wire.finished;
            records.extend(wire.records);
            if !passed {
                break;
            }
        }
    }
    let (closes, fallback) = files::take_probe();
    let lock_closed = held_lock
        .take()
        .map(nix::unistd::close)
        .transpose()
        .map_err(io::Error::from);
    lock_ready?;
    lock_closed?;
    let lock_released = if root.join("work/workload.lock").exists() {
        let file = File::open(root.join("work/workload.lock"))?;
        let checked = rustix::fs::flock(
            file.as_fd(),
            rustix::fs::FlockOperation::NonBlockingLockExclusive,
        )
        .is_ok();
        let closed = nix::unistd::close(file).map_err(io::Error::from);
        closed?;
        checked
    } else {
        true
    };
    for record in &mut records {
        normalize(record, &root);
    }
    Ok(Native {
        passed: !codes.is_empty() && codes.iter().all(|code| *code == 0),
        codes,
        records,
        reads,
        channels_finished,
        attempts: attempts(&root, end)?,
        publication: directory_files(&root.join("public/releases").join("a".repeat(40)), end)?,
        originals_after: originals(&root.join("inputs"), end)?,
        originals_before,
        primary: owner.error(),
        closes,
        fallback,
        lock_released,
    })
}
fn compare(native: Native, reference: Reference) {
    let scenario = &reference.scenario;
    let observed = &reference.observation;
    assert_eq!(
        observed["workerSettled"],
        serde_json::json!({
            "status":0,"pipesClosed":true,"reaped":true,"stderrBytes":0
        }),
        "{scenario}: original worker settlement"
    );
    assert_eq!(
        observed["sourceUnchanged"], true,
        "{scenario}: original source"
    );
    assert_eq!(
        observed["originalPythonUnchanged"], true,
        "{scenario}: original Python"
    );
    assert_eq!(
        observed["originalBaselinePythonUnchanged"], true,
        "{scenario}: original baseline Python"
    );
    assert_eq!(
        observed["originalBaselinePythonCount"], 267,
        "{scenario}: original baseline Python census"
    );
    assert!(
        observed["originalBaselinePythonPins"]
            .as_object()
            .is_some_and(|pins| pins.len() == 267),
        "{scenario}: complete original baseline Python pins"
    );
    assert!(
        observed["originalPythonCount"]
            .as_u64()
            .is_some_and(|count| count >= 267),
        "{scenario}: full maintained Python census includes the original baseline"
    );
    assert_eq!(
        observed["originalUnchanged"], true,
        "{scenario}: original input bytes"
    );
    assert_eq!(
        native.originals_before, native.originals_after,
        "{scenario}: actual native pristine inputs"
    );
    assert!(native.channels_finished, "{scenario}: channel completion");
    assert_eq!(
        native.fallback, 0,
        "{scenario}: no fallback descriptor closes"
    );
    assert!(native.lock_released, "{scenario}: actual lock released");
    assert_eq!(
        observed["lockReleased"], true,
        "{scenario}: original lock released"
    );
    assert_eq!(
        Value::from(native.passed),
        observed["passed"],
        "{scenario}: whole verdict"
    );
    assert_eq!(
        serde_json::to_value(&native.codes).unwrap(),
        observed["exitCodes"],
        "{scenario}: complete operation statuses"
    );
    assert!(
        native.reads <= 4,
        "{scenario}: exactly two frames per admitted operation"
    );
    if native.passed {
        assert!(
            native.primary.is_none(),
            "{scenario}: no successful primary error"
        );
        assert!(
            native.closes > 0,
            "{scenario}: checked original descriptor closes"
        );
    }
    let expected_attempts = reference_attempts(&reference).unwrap();
    assert_eq!(
        native.attempts, expected_attempts,
        "{scenario}: actual retained effects"
    );
    assert_eq!(
        native.publication, observed["publication"],
        "{scenario}: publication bytes"
    );
    let mut expected_records = observed["receipts"].clone();
    normalize(&mut expected_records, Path::new("/__NEVER_MATCH__"));
    for record in expected_records.as_array_mut().unwrap() {
        if let Some(evidence) = record
            .get("evidence")
            .and_then(Value::as_str)
            .map(str::to_owned)
        {
            let path = Path::new(&evidence);
            if let Some(at) = path
                .ancestors()
                .find(|p| p.file_name() == Some(OsStr::new("reference")))
            {
                normalize(record, at);
            }
        }
    }
    assert_eq!(
        Value::Array(native.records),
        expected_records,
        "{scenario}: actual receipts"
    );
}
#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_host_receiver_matches_unchanged_python() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        for scenario in SCENARIOS {
            let start = Instant::now();
            let fixture = Fixture::new();
            let mut owner = match Owner::new(start, WHOLE, RESERVE) {
                Ok(owner) => owner,
                Err(cause) => {
                    drop(fixture);
                    panic!("{scenario}: owner {cause:?}");
                }
            };
            let root = fixture.0.join("input");
            let prepared = fs::create_dir(&root);
            let python = if prepared.is_ok() {
                Some(python(&mut owner, start, &root, scenario).await)
            } else {
                None
            };
            let ready = python.as_ref().is_some_and(|python| {
                python.retired
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
                        == 1
            });
            let observed = if ready {
                let python = python.as_ref().unwrap();
                match serde_json::from_slice::<Reference>(&python.captures.output) {
                    Ok(reference) if reference.scenario == *scenario => {
                        native(&root, &reference, start, &mut owner)
                            .await
                            .map(|native| (native, reference))
                    }
                    _ => Err(error("closed whole host receiver response")),
                }
            } else {
                Err(error("bounded original receiver owner"))
            };
            let finalized = owner.finish();
            let positive = observed.as_ref().is_ok_and(|(native, _)| native.passed);
            drop(fixture);
            assert!(
                !positive || finalized.is_ok(),
                "{scenario}: original final owner fence"
            );
            assert!(ready, "{scenario}: original reference owner");
            let (native, reference) =
                observed.unwrap_or_else(|error| panic!("{scenario}: {error}"));
            compare(native, reference);
        }
    });
    drop(runtime);
}
