//! Whole builder transactions over owned real files and inert tool observations.
//!
//! These fixtures run the actual controller, extraction, metadata validation,
//! publication and finalizers. They do not execute Git, Docker or an image.

use super::files::close;
use super::process::{Completion, Invocation, Observation, Report, Role, Seal};
use super::{Attempt, Executor, FixtureInputs, Inputs, execute};
use super::{
    control::{Failure, Owner},
    options::Options,
};
use crate::test_support::Fixture;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Cursor, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

const REVISION: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const IMAGE: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const LAYER: &str = "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const OTHER: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const TAG: &str = "simplestchat-release/production:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SCENARIOS: &[&str] = &[
    "build-success",
    "export-missing-tag",
    "export-existing-tag",
    "invalid-selector",
    "invalid-output",
    "dirty-or-invalid-head",
    "guard-blocked",
    "remote-endpoint",
    "export-capability",
    "checked-env",
    "active-workload",
    "custom-builder",
    "probe-failure",
    "source-invalid",
    "migrations-invalid",
    "build-fails",
    "export-fails",
    "source-changes",
    "image-identity",
    "layer-metadata",
    "conflicting-tag",
    "tag-changes",
    "archive-wrong-layers",
    "archive-invalid",
    "receiver-boundary",
    "deadline-before-save",
];
const ENVIRONMENT: &[&str] = &[
    "DOCKER_CONTEXT",
    "DOCKER_HOST",
    "DOCKER_TLS_VERIFY",
    "DOCKER_CERT_PATH",
    "DOCKER_API_VERSION",
    "BUILDX_BUILDER",
    "BUILDKIT_HOST",
    "FIXTURE_ENV",
];

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}
fn private(path: &Path, bytes: &[u8], mode: u32) {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(mode)
        .open(path)
        .unwrap();
    file.write_all(bytes).unwrap();
    close(file).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}
fn directory(path: &Path, mode: u32) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    #[serde(default)]
    name: String,
    mutation: String,
    value: Value,
    export: bool,
}
impl Case {
    fn simple(mutation: &str, export: bool) -> Self {
        Self {
            name: "fixture".into(),
            mutation: mutation.into(),
            value: Value::Null,
            export,
        }
    }
    fn value(&self) -> &str {
        self.value.as_str().unwrap()
    }
    fn options(&self, output: PathBuf) -> std::result::Result<Options, Failure> {
        let seconds = if self.mutation == "timeout" {
            self.value
                .as_u64()
                .filter(|value| (60..=7200).contains(value))
                .ok_or(Failure::Input)?
        } else {
            120
        };
        let image = if self.mutation == "image-id" {
            let image = self.value.as_str().ok_or(Failure::Input)?;
            if !image
                .strip_prefix("sha256:")
                .is_some_and(|value| super::options::hex(value, 64))
            {
                return Err(Failure::Input);
            }
            Some(image.into())
        } else {
            self.export.then(|| IMAGE.into())
        };
        Ok(Options {
            output,
            seconds,
            image,
        })
    }
    fn environment(&self, bin: &Path) -> BTreeMap<OsString, OsString> {
        let mut environment = BTreeMap::from([("PATH".into(), bin.as_os_str().into())]);
        if self.mutation == "endpoint" {
            environment.insert("DOCKER_HOST".into(), self.value().into());
        }
        if self.mutation == "checked-env" {
            for name in &ENVIRONMENT[..7] {
                environment.insert((*name).into(), "ignored-override".into());
            }
            environment.insert("DOCKER_HOST".into(), "unix:///var/run/docker.sock".into());
            environment.insert("FIXTURE_ENV".into(), "preserved".into());
        }
        environment
    }
}

struct Recorded {
    action: String,
    argv: Vec<String>,
    cwd: PathBuf,
    environment: BTreeMap<OsString, OsString>,
    capture: bool,
    allow_failure: bool,
    archive: bool,
}
#[derive(Clone, Copy, Default)]
enum Fault {
    #[default]
    None,
    Child(i32),
    SourceExchange,
    ImageExchange,
    ArchiveSealMismatch,
    ArchiveUnclosed,
    ContextExchange,
    ContextExtra,
    ContextBeforeSeal,
    ToolMutation,
}
struct Inert {
    case: Case,
    source: Vec<u8>,
    image: Vec<u8>,
    records: Vec<Recorded>,
    heads: usize,
    statuses: usize,
    tag_reads: usize,
    temporary: Option<PathBuf>,
    context: Option<PathBuf>,
    global_metadata_absent: Option<bool>,
    output: PathBuf,
    fault: Fault,
}
impl Inert {
    fn new(case: Case, source: Vec<u8>, image: Vec<u8>, fault: Fault, output: PathBuf) -> Self {
        Self {
            case,
            source,
            image,
            records: Vec::new(),
            heads: 0,
            statuses: 0,
            tag_reads: 0,
            temporary: None,
            context: None,
            global_metadata_absent: None,
            output,
            fault,
        }
    }
    fn output(path: &Path, bytes: &[u8], exchange: bool) {
        let mut file = OpenOptions::new().write(true).open(path).unwrap();
        file.set_len(0).unwrap();
        file.write_all(bytes).unwrap();
        close(file).unwrap();
        if exchange {
            let replacement = path.with_extension("foreign");
            private(&replacement, bytes, 0o600);
            fs::rename(replacement, path).unwrap();
        }
    }
    fn observed(&mut self, request: &Invocation, owner: &mut Owner) -> (i32, Vec<u8>) {
        let arguments: Vec<_> = request
            .arguments
            .iter()
            .map(|value| value.to_str().unwrap())
            .collect();
        if request.role == Role::Git {
            match arguments[0] {
                "check-ignore" => {
                    return (
                        i32::from(
                            self.case.mutation == "output" && self.case.value == "nonignored",
                        ),
                        Vec::new(),
                    );
                }
                "rev-parse" => {
                    self.heads += 1;
                    return (
                        0,
                        if self.case.mutation == "invalid-head" {
                            b"main".to_vec()
                        } else if self.case.mutation == "changed-head" && self.heads > 1 {
                            b"cccccccccccccccccccccccccccccccccccccccc".to_vec()
                        } else {
                            REVISION.as_bytes().to_vec()
                        },
                    );
                }
                "status" => {
                    self.statuses += 1;
                    return (
                        0,
                        if self.case.mutation == "dirty"
                            || (self.case.mutation == "final-dirty" && self.statuses > 1)
                        {
                            b" M src/main.rs".to_vec()
                        } else {
                            Vec::new()
                        },
                    );
                }
                "archive" => {
                    let path = PathBuf::from(
                        arguments
                            .iter()
                            .find_map(|value| value.strip_prefix("--output="))
                            .unwrap(),
                    );
                    self.temporary = path.parent().map(Path::to_owned);
                    Self::output(
                        &path,
                        &self.source,
                        matches!(self.fault, Fault::SourceExchange),
                    );
                    if matches!(self.fault, Fault::ContextBeforeSeal) {
                        crate::release::build::source::fixture_source_extra_before_seal_once();
                    }
                    return (0, Vec::new());
                }
                _ => panic!("unexpected Git role"),
            }
        }
        if let Some(temporary) = &self.temporary
            && temporary.join("context").is_dir()
        {
            self.global_metadata_absent = Some(matches!(
                fs::symlink_metadata(temporary.join("context/pax_global_header")),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound
            ));
        }
        if arguments[..2] == ["context", "inspect"] {
            return (0, b"unix:///var/run/docker.sock".to_vec());
        }
        assert_eq!(&arguments[..2], ["--host", "unix:///var/run/docker.sock"]);
        let command = &arguments[2..];
        if command == ["image", "save", "--help"] {
            if self.case.mutation == "fail" && self.case.value == "save-help" {
                return (7, Vec::new());
            }
            return (
                0,
                if self.case.mutation == "save-help" {
                    self.case.value().as_bytes().into()
                } else {
                    b"Options:\n      --platform string   Export a specific platform\n".to_vec()
                },
            );
        }
        if command[0] == "version" {
            if self.case.mutation == "fail" && self.case.value == "server-version" {
                return (7, Vec::new());
            }
            if matches!(self.fault, Fault::ToolMutation) {
                let git = request.program.parent().unwrap().join("git");
                let mut file = OpenOptions::new().write(true).open(git).unwrap();
                file.write_all(b"changed tool\n").unwrap();
                close(file).unwrap();
            }
            return (
                0,
                if matches!(self.case.mutation.as_str(), "api" | "checked-env") {
                    self.case.value().as_bytes().into()
                } else {
                    b"1.48".to_vec()
                },
            );
        }
        if command[0] == "ps" {
            let public = command.last().unwrap().contains("simplestchat-public");
            let active = self.case.mutation
                == if public {
                    "active-public"
                } else {
                    "active-benchmark"
                };
            return (
                0,
                if active {
                    b"fixture-active-container".to_vec()
                } else {
                    Vec::new()
                },
            );
        }
        if command[..2] == ["buildx", "inspect"] {
            return (
                0,
                if self.case.mutation == "custom-builder" {
                    b"Name: default\nDriver: remote\nNodes:\n".to_vec()
                } else {
                    b"Name: default\nDriver: docker\nNodes:\n".to_vec()
                },
            );
        }
        if command[..2] == ["image", "inspect"] {
            if self.case.mutation == "fail" && self.case.value == "inspect" {
                return (7, Vec::new());
            }
            if command[3] == "{{.Id}}" {
                self.tag_reads += 1;
                return (
                    0,
                    if self.case.mutation == "tag-before"
                        || (self.case.mutation == "tag-after" && self.tag_reads == 2)
                    {
                        OTHER.as_bytes().into()
                    } else {
                        IMAGE.as_bytes().into()
                    },
                );
            }
            let mut image = json!({"id":IMAGE,"os":"linux","architecture":"amd64",
                "user":"10001:10001","labels":{"org.opencontainers.image.revision":REVISION},
                "cmd":["/app/simplestChat"],"entrypoint":null,
                "rootfs":{"Type":"layers","Layers":[LAYER]}});
            if self.case.mutation == "image" {
                image
                    .as_object_mut()
                    .unwrap()
                    .extend(self.case.value.as_object().unwrap().clone());
            }
            if self.case.mutation == "rootfs" {
                image["rootfs"] = self.case.value.clone();
            }
            return (0, serde_json::to_vec(&image).unwrap());
        }
        if command[..2] == ["image", "ls"] {
            return (
                0,
                match self.case.mutation.as_str() {
                    "existing-tag" => IMAGE.as_bytes().into(),
                    "conflicting-tag" => OTHER.as_bytes().into(),
                    _ => Vec::new(),
                },
            );
        }
        if command[..2] == ["image", "tag"] {
            return (0, Vec::new());
        }
        if command[..2] == ["buildx", "build"] {
            self.context = Some(PathBuf::from(command.last().unwrap()));
            if self.case.mutation == "deadline" {
                let elapsed = owner.now().duration_since(owner.started());
                owner.advance(elapsed + Duration::from_secs(120));
            }
            if matches!(self.fault, Fault::ContextExchange) {
                let path = self.context.as_ref().unwrap().join("Dockerfile");
                let bytes = fs::read(&path).unwrap();
                fs::rename(&path, path.with_extension("original")).unwrap();
                private(&path, &bytes, 0o644);
            }
            if matches!(self.fault, Fault::ContextExtra) {
                private(
                    &self.context.as_ref().unwrap().join("unrecorded.txt"),
                    b"unrecorded context member\n",
                    0o644,
                );
            }
            return (
                match self.fault {
                    Fault::Child(status) => status,
                    _ if self.case.mutation == "fail" && self.case.value == "build" => 7,
                    _ => 0,
                },
                Vec::new(),
            );
        }
        if command[..2] == ["image", "save"] {
            assert_eq!(command, ["image", "save", "--platform", "linux/amd64", TAG]);
            assert!(request.archive.is_some());
            return (
                0,
                if self.case.mutation == "fail" && self.case.value == "export" {
                    Vec::new()
                } else {
                    self.image.clone()
                },
            );
        }
        panic!("unexpected Docker role")
    }
}

fn action(role: Role, arguments: &[OsString]) -> String {
    let argv: Vec<_> = arguments
        .iter()
        .map(|value| value.to_str().unwrap())
        .collect();
    if role == Role::Git {
        return match argv[0] {
            "check-ignore" => "git-check-ignore",
            "rev-parse" => "git-head",
            "status" => "git-status",
            "archive" => "git-archive",
            _ => panic!("Git role"),
        }
        .into();
    }
    if argv[0] == "context" {
        return "docker-context".into();
    }
    let args = &argv[2..];
    if args == ["image", "save", "--help"] {
        return "docker-save-help".into();
    }
    match args[0] {
        "version" => "docker-api",
        "ps" if args.last().unwrap().contains("simplestchat-public") => "docker-public",
        "ps" => "docker-benchmark",
        "buildx" if args[1] == "inspect" => "docker-builder",
        "buildx" => "docker-build",
        "image" => match args[1] {
            "inspect" if args[3] == "{{.Id}}" => "docker-inspect-tag",
            "inspect" => "docker-inspect-image",
            "ls" => "docker-tag-list",
            "tag" => "docker-tag",
            "save" => "docker-save",
            _ => panic!("image role"),
        },
        _ => panic!("Docker role"),
    }
    .into()
}

impl Executor for Inert {
    async fn run(&mut self, owner: &mut Owner, mut request: Invocation) -> Completion {
        let action = action(request.role, &request.arguments);
        let (status, bytes) = self.observed(&request, owner);
        let mut argv = vec![if request.role == Role::Git {
            "git".into()
        } else {
            "docker".into()
        }];
        argv.extend(
            request
                .arguments
                .iter()
                .map(|value| value.to_str().unwrap().to_owned()),
        );
        self.records.push(Recorded {
            action,
            argv,
            cwd: request.cwd.clone(),
            environment: request.environment.clone(),
            capture: request.capture,
            allow_failure: request.allow_failure,
            archive: request.archive.is_some(),
        });
        let archive = request.archive.take().map(|mut file| {
            file.write_all(&bytes).unwrap();
            file.flush().unwrap();
            file.sync_all().unwrap();
            let mut seal = Seal {
                bytes: bytes.len() as u64,
                sha256: sha(&bytes),
                metadata: file.metadata().unwrap(),
            };
            close(file).unwrap();
            if matches!(self.fault, Fault::ImageExchange) {
                let path = self.output.join("image.tar");
                let replacement = path.with_extension("foreign");
                private(&replacement, &bytes, 0o600);
                fs::rename(replacement, path).unwrap();
            }
            if matches!(self.fault, Fault::ArchiveSealMismatch) {
                seal.sha256 = "0".repeat(64);
            }
            seal
        });
        let log_bytes = if archive.is_some() {
            &[][..]
        } else {
            &bytes[..]
        };
        request.log.write_all(log_bytes).unwrap();
        request.log.flush().unwrap();
        let seal = Seal {
            bytes: log_bytes.len() as u64,
            sha256: sha(log_bytes),
            metadata: request.log.metadata().unwrap(),
        };
        close(request.log).unwrap();
        let result = if status != 0 && !request.allow_failure {
            Err(Failure::Command)
        } else {
            Ok(Report {
                status,
                inspection: if request.capture {
                    bytes.clone()
                } else {
                    Vec::new()
                },
                log: seal.clone(),
            })
        };
        Completion {
            result,
            observation: Observation {
                // These are supplied policy observations: no PID or actual child is invented.
                pid: None,
                exit_status: Some(status),
                timed_out: false,
                output_limited: false,
                inspection_limited: false,
                stdout_bytes: bytes.len() as u64,
                stderr_bytes: 0,
                stdout_sha256: sha(&bytes),
                stderr_sha256: sha(&[]),
                log: Some(seal),
                archive_closed: archive.is_none() || !matches!(self.fault, Fault::ArchiveUnclosed),
                archive,
                group_retired: true,
                reaped: true,
                stdout_eof: true,
                stderr_eof: true,
                stdout_closed: true,
                stderr_closed: true,
                log_closed: true,
                elapsed: Duration::ZERO,
                cleanup_errors: Vec::new(),
            },
        }
    }
}

struct Run {
    _fixture: Fixture,
    root: PathBuf,
    output: PathBuf,
    attempt: Attempt,
    executor: Inert,
    primary: Option<Failure>,
    environment: BTreeMap<OsString, OsString>,
    _lock: Option<File>,
}
impl Drop for Run {
    fn drop(&mut self) {
        // Only this test's admitted unique operation directory can contain its
        // injected replacements. No production cleanup adopts these aliases.
        if let Some(path) = &self.executor.temporary
            && path.exists()
        {
            fs::remove_dir_all(path).unwrap();
        }
    }
}

async fn run(case: Case, source: Vec<u8>, image: Vec<u8>, fault: Fault) -> Run {
    let fixture = Fixture::new();
    let root = fixture.0.join("checkout");
    directory(&root, 0o700);
    let bin = fixture.0.join("bin");
    directory(&bin, 0o700);
    for name in ["git", "docker"] {
        private(&bin.join(name), b"#!/bin/sh\nexit 97\n", 0o700);
    }
    let mut output = root.join("release");
    if case.mutation == "output" {
        match case.value() {
            "existing" => directory(&output, 0o700),
            "comma" => output = root.join("bad,name"),
            "control" => output = root.join("bad\nname"),
            "missing-parent" => output = root.join("missing/child"),
            "symlink-parent" => {
                symlink(&root, root.join("alias")).unwrap();
                output = root.join("alias/child");
            }
            "nonignored" => {}
            _ => panic!("output fixture"),
        }
    }
    let environment = case.environment(&bin);
    let runtime = fixture.0.join("runtime");
    let persistent = fixture.0.join("persistent/release-state.json");
    let mut lock = None;
    if case.mutation == "guard" {
        if matches!(
            case.name.as_str(),
            "unfinished-release" | "release-bad-mode"
        ) {
            directory(persistent.parent().unwrap(), 0o700);
            private(
                &persistent,
                &serde_json::to_vec(&json!({"schemaVersion":1,
                "finalized":case.name == "release-bad-mode"}))
                .unwrap(),
                if case.name == "release-bad-mode" {
                    0o644
                } else {
                    0o600
                },
            );
        } else {
            directory(
                &runtime,
                if case.name == "bad-mode" {
                    0o755
                } else {
                    0o700
                },
            );
            let path = runtime.join("workload.lock");
            if case.name == "symlink-lock" {
                symlink(fixture.0.join("absent-lock"), &path).unwrap();
            } else {
                private(&path, &[], 0o600);
            }
            if case.name == "active-lock" {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&path)
                    .unwrap();
                rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                    .unwrap();
                lock = Some(file);
            } else if case.name != "symlink-lock" {
                let schema = if case.name == "boolean-schema" {
                    json!(true)
                } else {
                    json!(1)
                };
                private(
                    &runtime.join("current.json"),
                    &serde_json::to_vec(&json!({
                        "schemaVersion":schema,
                        "finalized":matches!(case.name.as_str(), "boolean-schema" | "bad-mode"),
                    }))
                    .unwrap(),
                    0o600,
                );
            }
        }
    }
    let mut executor = Inert::new(case.clone(), source, image, fault, output.clone());
    let mut owner = Owner::new(
        Instant::now(),
        if case.export {
            super::control::Mode::Export
        } else {
            super::control::Mode::Build
        },
        120,
    )
    .unwrap();
    let attempt = match case.options(output.clone()) {
        Ok(options) => {
            execute(
                options,
                Inputs {
                    root: root.clone(),
                    environment: environment.clone(),
                    fixtures: Some(FixtureInputs {
                        socket: true,
                        guards: Some((runtime, persistent)),
                    }),
                },
                &mut owner,
                &mut executor,
            )
            .await
        }
        Err(error) => Attempt {
            result: Err(error),
            output: None,
        },
    };
    Run {
        _fixture: fixture,
        root,
        output,
        attempt,
        executor,
        primary: owner.primary(),
        environment,
        _lock: lock,
    }
}

fn entry(
    builder: &mut tar::Builder<Vec<u8>>,
    name: &str,
    bytes: &[u8],
    mode: u32,
    directory: bool,
) {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(mode);
    header.set_entry_type(if directory {
        tar::EntryType::Directory
    } else {
        tar::EntryType::Regular
    });
    header.set_cksum();
    builder
        .append_data(&mut header, name, Cursor::new(bytes))
        .unwrap();
}
fn pax_records(fields: &[(&str, &str)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (key, value) in fields {
        let body = format!(" {key}={value}\n");
        let mut length = body.len() + 1;
        loop {
            let record = format!("{length}{body}");
            if record.len() == length {
                bytes.extend_from_slice(record.as_bytes());
                break;
            }
            length = record.len();
        }
    }
    bytes
}
fn global_pax(builder: &mut tar::Builder<Vec<u8>>, name: &str, bytes: &[u8]) {
    // Git's real source archive begins with this USTAR global record. It is
    // metadata, not an extracted file, and its comment identifies the commit.
    let mut header = tar::Header::new_ustar();
    header.set_entry_type(tar::EntryType::new(b'g'));
    header.set_mode(0o666);
    header.set_size(bytes.len().try_into().unwrap());
    header.set_cksum();
    builder
        .append_data(&mut header, name, Cursor::new(bytes))
        .unwrap();
}
fn source_archive_with_pax(headers: &[(&str, Vec<u8>)], late: bool) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    if !late {
        for (name, bytes) in headers {
            global_pax(&mut builder, name, bytes);
        }
    }
    for name in ["build", "web", "migrations"] {
        entry(&mut builder, name, &[], 0o775, true);
    }
    for name in [
        "Dockerfile",
        ".dockerignore",
        "Cargo.lock",
        "web/package-lock.json",
        "build/pip-constraints.txt",
    ] {
        entry(&mut builder, name, b"pinned input", 0o664, false);
    }
    entry(
        &mut builder,
        "build/install-openssl.sh",
        b"#!/bin/sh\n",
        0o775,
        false,
    );
    entry(
        &mut builder,
        "migrations/001_fixture.sql",
        b"SELECT 1;\n",
        0o644,
        false,
    );
    if late {
        for (name, bytes) in headers {
            global_pax(&mut builder, name, bytes);
        }
    }
    builder.into_inner().unwrap()
}
fn source_archive() -> Vec<u8> {
    source_archive_with_pax(
        &[("pax_global_header", pax_records(&[("comment", REVISION)]))],
        false,
    )
}
fn image_archive() -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    entry(
        &mut builder,
        "manifest.json",
        &serde_json::to_vec(&json!([
        {"Config":"config.json","RepoTags":[TAG],"Layers":["layer.tar"]}]))
        .unwrap(),
        0o644,
        false,
    );
    entry(
        &mut builder,
        "config.json",
        &serde_json::to_vec(&json!({
            "architecture":"amd64","os":"linux","rootfs":{"type":"layers","diff_ids":[LAYER]},
            "config":{"User":"10001:10001","Cmd":["/app/simplestChat"],"Entrypoint":null,
                "Labels":{"org.opencontainers.image.revision":REVISION}},
        }))
        .unwrap(),
        0o644,
        false,
    );
    entry(
        &mut builder,
        "layer.tar",
        b"fixture-layer-not-executed",
        0o644,
        false,
    );
    builder.into_inner().unwrap()
}
fn record(path: &Path) -> Option<Value> {
    path.is_file()
        .then(|| serde_json::from_slice(&fs::read(path).unwrap()).unwrap())
}

#[test]
fn whole_build_and_exact_export_publish_verified_closed_records_and_same_candidate_inode() {
    runtime().block_on(async {
        for export in [false, true] {
            let source = source_archive();
            assert_eq!(source[156], b'g');
            assert_eq!(&source[..17], b"pax_global_header");
            assert_eq!(
                &source[512..564],
                format!("52 comment={REVISION}\n").as_bytes()
            );
            let result = run(
                Case::simple("", export),
                source,
                image_archive(),
                Fault::None,
            )
            .await;
            assert!(result.attempt.result.is_ok(), "{:?}", result.attempt.result);
            assert_eq!(result.primary, None);
            let manifest = record(&result.output.join("release.json")).unwrap();
            assert_eq!(manifest["revision"], REVISION);
            assert_eq!(manifest["platform"], "linux/amd64");
            assert_eq!(manifest["archiveSha256"], sha(&image_archive()));
            let source = record(&result.output.join("source.json")).unwrap();
            assert_eq!(source.as_object().unwrap().len(), 2);
            assert!(source.get("exportedImageId").is_none());
            assert_eq!(source["inputsSha256"].as_object().unwrap().len(), 5);
            assert_eq!(result.executor.global_metadata_absent, Some(true));
            assert_eq!(
                record(&result.output.join("outcome.json")).unwrap()["passed"],
                true
            );
            let candidate = fs::metadata(result.output.join("release.candidate.json")).unwrap();
            let published = fs::metadata(result.output.join("release.json")).unwrap();
            assert_eq!(
                (candidate.dev(), candidate.ino()),
                (published.dev(), published.ino())
            );
            assert_eq!(published.nlink(), 2);
            assert!(
                result
                    .executor
                    .temporary
                    .as_ref()
                    .is_some_and(|path| !path.exists())
            );
        }
    });
}

#[test]
fn headerless_source_archive_compatibility_keeps_build_and_export_valid() {
    runtime().block_on(async {
        for export in [false, true] {
            let result = run(
                Case::simple("", export),
                source_archive_with_pax(&[], false),
                image_archive(),
                Fault::None,
            )
            .await;
            assert!(result.attempt.result.is_ok(), "{:?}", result.attempt.result);
            assert_eq!(result.primary, None);
            assert_eq!(result.executor.global_metadata_absent, Some(true));
            assert!(result.output.join("release.json").is_file());
        }
    });
}

#[test]
fn source_global_pax_refusals_settle_the_source_ledger_before_any_build_or_export() {
    runtime().block_on(async {
        let comment = pax_records(&[("comment", REVISION)]);
        let mismatch = "f".repeat(40);
        let path = format!("../{REVISION}");
        let size = "0".repeat(43);
        let cases = [
            (
                "unknown-key",
                vec![("pax_global_header", pax_records(&[("unknown", REVISION)]))],
                false,
            ),
            (
                "path-override",
                vec![("pax_global_header", pax_records(&[("path", &path)]))],
                false,
            ),
            (
                "size-override",
                vec![("pax_global_header", pax_records(&[("size", &size)]))],
                false,
            ),
            (
                "duplicate-comment",
                vec![(
                    "pax_global_header",
                    pax_records(&[("comment", REVISION), ("comment", REVISION)]),
                )],
                false,
            ),
            (
                "mismatched-commit",
                vec![("pax_global_header", pax_records(&[("comment", &mismatch)]))],
                false,
            ),
            (
                "late-global",
                vec![("pax_global_header", comment.clone())],
                true,
            ),
            (
                "repeated-global",
                vec![
                    ("pax_global_header", comment.clone()),
                    ("pax_global_header", comment.clone()),
                ],
                false,
            ),
            (
                "wrong-global-name",
                vec![("nested/pax_global_header", comment)],
                false,
            ),
        ];
        for (name, headers, late) in cases {
            let source = source_archive_with_pax(&headers, late);
            let source_sha = sha(&source);
            let result = run(
                Case::simple("", false),
                source,
                image_archive(),
                Fault::None,
            )
            .await;
            // The controller has already settled every native peer and its
            // extraction journal before the refusal or context facts are read.
            assert_eq!(result.attempt.result, Err(Failure::Archive), "{name}");
            assert_eq!(result.primary, Some(Failure::Archive), "{name}");
            assert_eq!(sha(&result.executor.source), source_sha, "{name}");
            assert_eq!(result.executor.global_metadata_absent, None, "{name}");
            assert!(
                result.executor.records.iter().all(
                    |record| !["docker-build", "docker-save"].contains(&record.action.as_str())
                ),
                "{name}"
            );
            for file in [
                "source.json",
                "image.tar",
                "release.candidate.json",
                "release.json",
            ] {
                assert!(!result.output.join(file).exists(), "{name}: {file}");
            }
            assert_eq!(
                record(&result.output.join("outcome.json")).unwrap()["passed"],
                false
            );
            assert!(
                result
                    .executor
                    .temporary
                    .as_ref()
                    .is_some_and(|path| !path.exists()),
                "{name}"
            );
        }
    });
}

#[test]
fn subordinate_reserved_exit_numbers_remain_command_failures_with_signed_private_status() {
    runtime().block_on(async {
        for status in [124, 130, 143, -15] {
            let result = run(
                Case::simple("", false),
                source_archive(),
                image_archive(),
                Fault::Child(status),
            )
            .await;
            assert!(matches!(result.attempt.result, Err(Failure::Command)));
            assert_eq!(result.primary, Some(Failure::Command));
            assert!(!result.output.join("release.json").exists());
            let command = result
                .executor
                .records
                .iter()
                .position(|record| record.action == "docker-build")
                .unwrap()
                + 1;
            let outcome = record(
                &result
                    .output
                    .join(format!("{command:02}-docker.outcome.json")),
            )
            .unwrap();
            assert_eq!(outcome["exitStatus"], status);
            assert_eq!(outcome["timedOut"], false);
        }
    });
}

#[test]
fn archive_stdout_transport_closes_and_binds_original_before_publication() {
    runtime().block_on(async {
        for export in [false, true] {
            let image = image_archive();
            let result = run(
                Case::simple("", export),
                source_archive(),
                image.clone(),
                Fault::None,
            )
            .await;
            assert!(result.attempt.result.is_ok(), "{:?}", result.attempt.result);
            let save = result
                .executor
                .records
                .iter()
                .position(|record| record.action == "docker-save")
                .unwrap();
            let command = &result.executor.records[save];
            assert!(command.archive && !command.capture && !command.allow_failure);
            assert!(!command.argv.iter().any(|value| value == "--output"));
            let journal = record(
                &result
                    .output
                    .join(format!("{:02}-docker.outcome.json", save + 1)),
            )
            .unwrap();
            let cleanup = &journal["cleanup"];
            assert_eq!(cleanup["archiveClosed"], true);
            assert_eq!(cleanup["archiveBytes"], u64::try_from(image.len()).unwrap());
            assert_eq!(cleanup["archiveSha256"], sha(&image));
            assert_eq!(cleanup["stdoutBytes"], cleanup["archiveBytes"]);
            assert_eq!(cleanup["stdoutSha256"], cleanup["archiveSha256"]);
            let metadata = fs::metadata(result.output.join("image.tar")).unwrap();
            assert_eq!(cleanup["archiveIdentity"]["device"], metadata.dev());
            assert_eq!(cleanup["archiveIdentity"]["inode"], metadata.ino());
            assert!(result.output.join("release.json").is_file());
            assert!(
                fs::read(result.output.join(format!("{:02}-docker.log", save + 1)))
                    .unwrap()
                    .is_empty()
            );
        }
    });
}

#[test]
fn archive_transport_missing_close_or_mismatched_seal_refuses_both_selectors() {
    runtime().block_on(async {
        for export in [false, true] {
            for fault in [Fault::ArchiveSealMismatch, Fault::ArchiveUnclosed] {
                let result = run(
                    Case::simple("", export),
                    source_archive(),
                    image_archive(),
                    fault,
                )
                .await;
                assert_eq!(result.attempt.result, Err(Failure::Changed));
                assert_eq!(result.primary, Some(Failure::Changed));
                assert_eq!(
                    record(&result.output.join("outcome.json")).unwrap()["passed"],
                    false
                );
                assert!(!result.output.join("release.candidate.json").exists());
                assert!(!result.output.join("release.json").exists());
            }
        }
    });
}

#[test]
fn original_source_image_context_and_tool_witnesses_refuse_equal_byte_exchanges() {
    runtime().block_on(async {
        for fault in [
            Fault::SourceExchange,
            Fault::ImageExchange,
            Fault::ContextExchange,
            Fault::ContextExtra,
            Fault::ContextBeforeSeal,
            Fault::ToolMutation,
        ] {
            let result = run(
                Case::simple("", false),
                source_archive(),
                image_archive(),
                fault,
            )
            .await;
            assert!(matches!(result.attempt.result, Err(Failure::Changed)));
            assert!(!result.output.join("release.json").exists());
            assert_eq!(
                record(&result.output.join("outcome.json")).unwrap()["passed"],
                false
            );
            if matches!(fault, Fault::ContextExtra) {
                assert_eq!(
                    fs::read(
                        result
                            .executor
                            .context
                            .as_ref()
                            .unwrap()
                            .join("unrecorded.txt")
                    )
                    .unwrap(),
                    b"unrecorded context member\n"
                );
                assert!(
                    result
                        .executor
                        .records
                        .iter()
                        .all(|record| record.action != "docker-save")
                );
            }
            if matches!(fault, Fault::ContextBeforeSeal) {
                assert!(
                    result
                        .executor
                        .temporary
                        .as_ref()
                        .unwrap()
                        .join("context/unrecorded-before-seal.txt")
                        .is_file()
                );
                assert!(result.executor.records.iter().all(|record| {
                    !matches!(record.action.as_str(), "docker-build" | "docker-save")
                }));
            }
        }
    });
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Reference {
    schema_version: u8,
    scenario: String,
    cases: Vec<ReferenceCase>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReferenceCase {
    name: String,
    fixture: Case,
    fixture_files: Option<InputFiles>,
    passed: bool,
    published: bool,
    output_created: bool,
    error: Option<String>,
    actions: Vec<String>,
    commands: Vec<ReferenceCommand>,
    retained_files: Vec<String>,
    source: Option<Value>,
    manifest: Option<Value>,
    outcome: Option<Value>,
    temporary_clean: bool,
    receiver_rejected: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputFiles {
    source: Pin,
    image: Pin,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Pin {
    path: PathBuf,
    sha256: String,
    bytes: u64,
}
impl Pin {
    fn read(&self, directory: &Path) -> Vec<u8> {
        assert!(self.bytes <= 2 * 1024 * 1024);
        assert_eq!(self.path.components().count(), 2);
        assert!(
            self.path
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        );
        let path = directory.join(&self.path);
        assert_eq!(fs::metadata(&path).unwrap().len(), self.bytes);
        let bytes = fs::read(path).unwrap();
        assert_eq!(bytes.len() as u64, self.bytes);
        assert_eq!(sha(&bytes), self.sha256);
        bytes
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ReferenceCommand {
    argv: Vec<String>,
    cwd: String,
    timeout_seconds: f64,
    env_overrides: Option<BTreeMap<String, String>>,
    allow_failure: bool,
    capture: bool,
}

async fn reference(scenario: &str, fixture: &Fixture) -> Reference {
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let prefix = std::env::var_os("PYTHON_CHECK_ENV")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository.join("ops/ansible/.venv"));
    let program = prefix.join("bin/python3");
    let directory = fixture.0.join(scenario);
    let log = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(fixture.0.join(format!("{scenario}.log")))
        .unwrap();
    let mut owner = Owner::new(
        Instant::now(),
        crate::release::build::control::Mode::Export,
        120,
    )
    .unwrap();
    // This exact ignored test owns the selected pinned Python process. Its
    // Invocation is not exposed by the production Git/Docker command graph.
    let completion = super::process::run(
        &mut owner,
        Invocation {
            role: Role::Git,
            program,
            arguments: vec![
                "-B".into(),
                repository
                    .join("ops/ansible/tests/release_build_oracle.py")
                    .into_os_string(),
                "--scenario".into(),
                scenario.into(),
                "--fixture-directory".into(),
                directory.into_os_string(),
            ],
            cwd: repository.into(),
            environment: BTreeMap::from([
                ("PATH".into(), prefix.join("bin").into_os_string()),
                ("LC_ALL".into(), "C".into()),
                ("LANG".into(), "C".into()),
            ]),
            body_end: Instant::now() + Duration::from_secs(30),
            log,
            archive: None,
            capture: true,
            allow_failure: false,
        },
    )
    .await;
    assert!(completion.observation.group_retired && completion.observation.reaped);
    assert!(
        completion.observation.stdout_closed
            && completion.observation.stderr_closed
            && completion.observation.log_closed
    );
    assert_eq!(completion.observation.exit_status, Some(0));
    let report = completion.result.unwrap();
    assert!(report.inspection.len() <= 1024 * 1024);
    let mut result: Reference = serde_json::from_slice(&report.inspection).unwrap();
    assert_eq!(result.schema_version, 1);
    assert_eq!(result.scenario, scenario);
    for case in &mut result.cases {
        case.fixture.name = case.name.clone();
    }
    owner.check().unwrap();
    result
}
fn normalized(
    value: &str,
    root: &Path,
    temporary: Option<&Path>,
    context: Option<&Path>,
) -> String {
    let mut value = value.to_owned();
    if let Some(context) = context {
        value = value.replace(context.to_str().unwrap(), "$CONTEXT");
    }
    if let Some(temporary) = temporary {
        value = value.replace(temporary.to_str().unwrap(), "$TEMPORARY");
    }
    value.replace(root.to_str().unwrap(), "$FIXTURE/checkout")
}
fn reference_path(value: &str) -> String {
    value
        .replace(
            "$FIXTURE/temporary-0/simplestchat-release.$TEMP/source",
            "$CONTEXT",
        )
        .replace("$CONTEXT.tar", "$TEMPORARY/source.tar")
}
fn compare_commands(actual: &Run, expected: &ReferenceCase) {
    assert_eq!(
        actual.executor.records.len(),
        expected.commands.len(),
        "{}",
        expected.name
    );
    for (actual_command, expected_command) in actual.executor.records.iter().zip(&expected.commands)
    {
        let mut argv: Vec<_> = actual_command
            .argv
            .iter()
            .map(|value| {
                normalized(
                    value,
                    &actual.root,
                    actual.executor.temporary.as_deref(),
                    actual.executor.context.as_deref(),
                )
            })
            .collect();
        if argv.get(1).map(String::as_str) == Some("check-ignore") {
            let value = argv.last_mut().unwrap();
            if value == "release" {
                *value = "$FIXTURE/checkout/release".into();
            }
        }
        let mut expected_argv: Vec<_> = expected_command
            .argv
            .iter()
            .map(|value| reference_path(value))
            .collect();
        if actual_command.action == "docker-save" {
            // The sole reference transport delta: Docker stdout enters the
            // original owned file instead of Docker replacing --output's inode.
            assert!(actual_command.archive);
            assert_eq!(expected_argv.len(), 10);
            assert_eq!(
                &expected_argv[3..7],
                ["image", "save", "--platform", "linux/amd64"]
            );
            assert_eq!(
                &expected_argv[7..9],
                ["--output", "$FIXTURE/checkout/release/image.tar"]
            );
            assert_eq!(expected_argv[9], TAG);
            drop(expected_argv.drain(7..9));
        } else {
            assert!(!actual_command.archive);
        }
        assert_eq!(
            argv, expected_argv,
            "{} {}",
            expected.name, actual_command.action
        );
        assert_eq!(
            normalized(
                actual_command.cwd.to_str().unwrap(),
                &actual.root,
                actual.executor.temporary.as_deref(),
                actual.executor.context.as_deref()
            ),
            reference_path(&expected_command.cwd),
            "{} cwd",
            expected.name
        );
        assert_eq!(actual_command.capture, expected_command.capture);
        assert_eq!(actual_command.allow_failure, expected_command.allow_failure);
        assert!(
            expected_command.timeout_seconds.is_finite() && expected_command.timeout_seconds > 0.0
        );
        let selected = |environment: &BTreeMap<OsString, OsString>| -> BTreeMap<String, String> {
            environment
                .iter()
                .filter(|(name, _)| ENVIRONMENT.contains(&name.to_str().unwrap()))
                .map(|(name, value)| {
                    let name = name.to_str().unwrap();
                    (name.into(), value.to_str().unwrap().into())
                })
                .collect()
        };
        let expected_environment = expected_command
            .env_overrides
            .clone()
            .unwrap_or_else(|| selected(&actual.environment));
        assert_eq!(selected(&actual_command.environment), expected_environment);
    }
}

#[test]
#[ignore = "requires pinned controller Python; run by operations automation"]
fn whole_reference_scenarios_match_unchanged_python() {
    runtime().block_on(async {
        let fixture = Fixture::new();
        let mut count = 0;
        for scenario in SCENARIOS {
            let expected = reference(scenario, &fixture).await;
            for expected in expected.cases {
                count += 1;
                assert_eq!(expected.name, expected.fixture.name);
                assert_eq!(expected.error.is_none(), expected.passed);
                assert!(expected.temporary_clean);
                if expected.fixture.mutation == "receiver" {
                    assert_eq!(expected.receiver_rejected, Some(true));
                }
                let inputs = expected.fixture_files.as_ref().unwrap();
                let input_directory = fixture.0.join(scenario);
                let actual = run(
                    expected.fixture.clone(),
                    inputs.source.read(&input_directory),
                    inputs.image.read(&input_directory),
                    Fault::None,
                )
                .await;
                assert_eq!(
                    actual.attempt.result.is_ok(),
                    expected.passed,
                    "{scenario} {} {:?}",
                    expected.name,
                    actual.attempt.result
                );
                assert_eq!(
                    actual.output.join("release.json").is_file(),
                    expected.published
                );
                assert_eq!(actual.attempt.output.is_some(), expected.output_created);
                assert_eq!(
                    actual
                        .executor
                        .records
                        .iter()
                        .map(|record| record.action.clone())
                        .collect::<Vec<_>>(),
                    expected.actions,
                    "{scenario} {}",
                    expected.name
                );
                compare_commands(&actual, &expected);
                assert_eq!(record(&actual.output.join("source.json")), expected.source);
                let mut manifest = record(&actual.output.join("release.json"));
                if let (Some(actual), Some(expected)) = (&mut manifest, &expected.manifest) {
                    actual["createdAt"] = expected["createdAt"].clone();
                }
                assert_eq!(manifest, expected.manifest);
                let outcome = record(&actual.output.join("outcome.json"));
                assert_eq!(outcome.is_some(), expected.outcome.is_some());
                if let (Some(actual), Some(expected_outcome)) = (outcome, expected.outcome) {
                    assert_eq!(actual["passed"], expected_outcome["passed"]);
                    assert_eq!(actual["revision"], expected_outcome["revision"]);
                    assert_eq!(
                        actual.get("exportedImageId"),
                        expected_outcome.get("exportedImageId")
                    );
                    assert_eq!(
                        actual.as_object().unwrap().len(),
                        expected_outcome.as_object().unwrap().len()
                    );
                }
                assert_eq!(
                    expected.retained_files.contains(&"release.json".into()),
                    expected.passed
                );
                assert!(
                    actual
                        .executor
                        .temporary
                        .as_ref()
                        .is_none_or(|path| !path.exists())
                );
            }
        }
        assert_eq!(count, 112);
    });
}
