//! Whole offline preparation against the unchanged pinned-Python file oracle.
//!
//! This ignored test owns only its private files and bounded Python child. It
//! proves no signing, network, engine, host or authenticated distribution path.

use super::*;
use crate::{owned_process, test_support::Fixture};
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::{fs::MetadataExt, process::CommandExt, process::ExitStatusExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Duration,
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
const CAPTURE: usize = 65536;
const FIXTURE_BYTES: u64 = 2 * 1024 * 1024;
const ORIGINAL: [&str; 4] = ["image.tar", "release.json", "outcome.json", "source.json"];
const SCENARIOS: &[&str] = &[
    "success",
    "required",
    "opaque-source",
    "opaque-sbom",
    "hash-only-empty",
    "hash-only-raw",
    "hash-only-symlink",
    "copied-symlink",
    "encoding-security-bom",
    "encoding-security-utf16",
    "encoding-runtime-utf32",
    "ignored-large-integer",
    "ignored-bool-null",
    "ignored-surrogate",
    "ignored-depth",
    "ignored-values",
    "security-nonfinite",
    "security-duplicate",
    "security-passed-integer",
    "self-test-integer",
    "checks-missing",
    "checks-extra",
    "check-passed-integer",
    "selected-config",
    "image-id",
    "security-revision",
    "security-archive",
    "security-platform",
    "build-passed",
    "build-revision",
    "build-image",
    "hash-sbom",
    "hash-native",
    "hash-elf",
    "hash-secret-paths",
    "hash-database-status",
    "hash-runtime-proof",
    "hash-vex",
    "hash-runtime-licenses",
    "conflict-sbom",
    "conflict-image-security",
    "conflict-runtime-proof",
    "conflict-vex",
    "conflict-predicate",
    "copy-read-sbom",
    "copy-read-image-security",
    "copy-read-runtime-proof",
    "copy-read-vex",
    "copy-write-sbom",
    "copy-write-image-security",
    "copy-write-runtime-proof",
    "copy-write-vex",
    "runtime-required-integer",
    "runtime-schema-boolean",
    "runtime-passed-integer",
    "runtime-required-mismatch",
    "runtime-image",
    "runtime-archive",
    "runtime-revision",
    "runtime-platform",
    "optional-vex-nonempty",
    "required-vex-empty",
    "required-vex-multiple",
    "required-scope",
    "required-binary-digest",
    "required-native-digest",
    "required-elf-digest",
    "required-sbom-digest",
    "expiry-today",
    "expiry-yesterday",
    "expiry-invalid",
    "expiry-unicode",
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
struct Selection {
    repository: String,
    revision: String,
    ci_run_id: u64,
    build_run_id: u64,
    run_attempt: u64,
}
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum FaultKind {
    CopyRead,
    CopyWrite,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Fault {
    kind: FaultKind,
    copy: usize,
    retained_bytes: u64,
}
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Profile {
    NofollowHashOnly,
    UnicodeScalar,
    #[serde(rename = "depth-128")]
    Depth128,
    #[serde(rename = "values-100000")]
    Values100000,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Input {
    selection: Selection,
    today: String,
    fault: Option<Fault>,
    rust_profile: Option<Profile>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Observation {
    passed: bool,
    files: BTreeMap<String, Pin>,
    original_unchanged: bool,
    copy_order: Vec<String>,
    failure_class: Option<String>,
    predicate: Option<Value>,
    predicate_sha256: Option<String>,
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
                if bytes.len().checked_add(count).is_some_and(|n| n <= CAPTURE) {
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
                .join("ops/ansible/tests/release_prepare_oracle.py")
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
    nanos: i64,
}
fn originals(directory: &Path, end: Instant) -> io::Result<Vec<Original>> {
    ORIGINAL
        .iter()
        .map(|name| {
            let (pin, metadata) = file_pin(&directory.join(name), end)?;
            Ok(Original {
                pin,
                device: metadata.dev(),
                inode: metadata.ino(),
                modified: metadata.mtime(),
                nanos: metadata.mtime_nsec(),
            })
        })
        .collect()
}
fn predicate_bytes(path: &Path, end: Instant) -> io::Result<Vec<u8>> {
    let mut file = File::open(path)?;
    let observed = (|| {
        if Instant::now() >= end {
            return Err(error("oracle predicate observation expired"));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(CAPTURE as u64 + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() > CAPTURE || Instant::now() >= end {
            return Err(error("oracle predicate observation bound"));
        }
        Ok(bytes)
    })();
    let closed = nix::unistd::close(file).map_err(io::Error::from);
    observed.and_then(|bytes| closed.map(|()| bytes))
}
fn options(directory: &Path, selection: &Selection) -> Result<Options> {
    if selection.ci_run_id != selection.build_run_id {
        return Err(Cause::Input);
    }
    Options::parse(&[
        "--artifact-dir".into(),
        directory.join("artifact").into_os_string(),
        "--security-dir".into(),
        directory.join("security").into_os_string(),
        "--repository".into(),
        selection.repository.clone().into(),
        "--revision".into(),
        selection.revision.clone().into(),
        "--run-id".into(),
        selection.ci_run_id.to_string().into(),
        "--run-attempt".into(),
        selection.run_attempt.to_string().into(),
        "--timeout-seconds".into(),
        WHOLE.as_secs().to_string().into(),
        "--finalization-seconds".into(),
        RESERVE.as_secs().to_string().into(),
    ])?
    .ok_or(Cause::Input)
}
fn profile(scenario: &str) -> Option<Profile> {
    match scenario {
        "hash-only-symlink" => Some(Profile::NofollowHashOnly),
        "ignored-surrogate" => Some(Profile::UnicodeScalar),
        "ignored-depth" => Some(Profile::Depth128),
        "ignored-values" => Some(Profile::Values100000),
        _ => None,
    }
}
fn fault(fault: Option<&Fault>) -> Result<()> {
    if let Some(fault) = fault {
        let (_, name) = COPIES.get(fault.copy).ok_or(Cause::Input)?;
        if fault.retained_bytes != 3 {
            return Err(Cause::Input);
        }
        files::copy_fault(
            (*name).to_owned(),
            matches!(fault.kind, FaultKind::CopyRead),
            fault.retained_bytes,
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_prepare_scenarios_match_unchanged_python() {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            for scenario in SCENARIOS {
                let start = Instant::now();
                let mut owner = Owner::new(start, WHOLE, RESERVE).unwrap();
                let fixture = Fixture::new();
                let directory = fixture.0.join("input");
                let python = python(&mut owner, start, &directory, scenario).await;
                let python_error = owner.error();
                // Every native child/pipe finalizer has already run. These
                // assertions cannot abandon an admitted process or descriptor.
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
                assert_eq!(reference.input.rust_profile, profile(scenario));
                let artifact = directory.join("artifact");
                let before = originals(&artifact, start + WHOLE);
                if before.is_err() {
                    owner.record(Cause::Io);
                }
                owner.today = chrono::NaiveDate::from_ymd_opt(2050, 1, 1);
                let options = options(&directory, &reference.input.selection);
                let parsed = options.is_ok();
                files::start_probe(None);
                let actual = fault(reference.input.fault.as_ref())
                    .and_then(|()| options.and_then(|options| execute(&options, &mut owner)));
                if let Err(cause) = actual {
                    owner.record(cause);
                }
                let probes = files::take_probe();
                let after = originals(&artifact, start + WHOLE);
                let observed = snapshot(&artifact, start + WHOLE);
                let predicate = actual.is_ok().then(|| {
                    predicate_bytes(&artifact.join("release-predicate.json"), start + WHOLE)
                });
                let _ = owner.check(Window::Final);
                let final_error = owner.error();
                drop(owner);
                // Rust State.finish, all retained test reads and the original
                // signal registration are retired before checking verdicts.
                assert_eq!(probes.1, 0, "{scenario}: no descriptor fallback");
                if parsed {
                    assert!(probes.0 > 0, "{scenario}: actual retained files closed");
                } else {
                    assert_eq!(probes.0, 0, "{scenario}: selectors refuse before admission");
                }
                assert_eq!(before.unwrap(), after.unwrap(), "{scenario}");
                assert!(reference.observation.original_unchanged, "{scenario}");
                let observed = observed.unwrap();
                if reference.input.rust_profile.is_some() {
                    assert!(reference.observation.passed, "{scenario}: reference domain");
                    assert!(
                        actual.is_err(),
                        "{scenario}: explicit narrower Rust profile"
                    );
                    assert!(!observed.contains_key("release-predicate.json"));
                    continue;
                }
                assert_eq!(
                    actual.is_ok(),
                    reference.observation.passed,
                    "{scenario}: {actual:?}"
                );
                assert_eq!(final_error.is_none(), actual.is_ok(), "{scenario}");
                assert_eq!(observed, reference.observation.files, "{scenario}");
                assert!(reference.observation.copy_order.len() <= COPIES.len());
                assert!(
                    reference
                        .observation
                        .copy_order
                        .iter()
                        .zip(COPIES)
                        .all(|(name, (_, expected))| name == expected)
                );
                assert_eq!(
                    reference.observation.failure_class.as_deref(),
                    (!reference.observation.passed).then_some("release_attestation_failed")
                );
                if actual.is_ok() {
                    let predicate = predicate.unwrap().unwrap();
                    assert_eq!(
                        Some(files::hex(&Sha256::digest(&predicate))),
                        reference.observation.predicate_sha256,
                        "{scenario}: original predicate bytes"
                    );
                    assert_eq!(
                        Some(serde_json::from_slice::<Value>(&predicate).unwrap()),
                        reference.observation.predicate,
                        "{scenario}: predicate semantic fields"
                    );
                }
            }
        });
}
