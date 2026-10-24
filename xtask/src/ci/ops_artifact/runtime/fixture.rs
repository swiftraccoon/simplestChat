//! Complete inert acquisition plus an owned keeper observation. No network,
//! cryptography, VM, candidate or installer is executed by these fixtures.

use super::{plan, receipt, sources};
use crate::{
    ci::{
        command::{Command, Program},
        process::Executor,
    },
    tooling::{Failure, Result},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, atomic::Ordering},
    time::{Duration, Instant},
};

pub(super) use super::super::acquire::fixture::{Clock, bytes};
type Acquisition = super::super::acquire::fixture::Fixture;
type Acquirer = super::super::acquire::fixture::Fake;

impl plan::Clock for Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}

pub(super) fn tar_entry(builder: &mut tar::Builder<Vec<u8>>, name: &str, kind: u8, body: &[u8]) {
    let mut header = tar::Header::new_ustar();
    header.set_size(body.len() as u64);
    header.set_mode(0o600);
    header.set_entry_type(tar::EntryType::new(kind));
    header.set_cksum();
    builder.append_data(&mut header, name, body).unwrap();
}

pub(super) fn source_archive(caller: &Path, revision: &str) -> Vec<u8> {
    let mut archive = tar::Builder::new(Vec::new());
    tar_entry(
        &mut archive,
        "pax_global_header",
        b'g',
        format!("52 comment={revision}\n").as_bytes(),
    );
    for name in sources::NAMES {
        tar_entry(
            &mut archive,
            name,
            b'0',
            &fs::read(caller.join(name)).unwrap(),
        );
    }
    archive.finish().unwrap();
    archive.into_inner().unwrap()
}

pub(super) struct Fixture {
    pub acquisition: Acquisition,
    pub caller: PathBuf,
    pub output: PathBuf,
    pub request: PathBuf,
    pub wire: Value,
}
impl Fixture {
    pub fn new() -> Self {
        let acquisition = Acquisition::new(1);
        let caller = acquisition.root.join("checkout");
        fs::create_dir(&caller).unwrap();
        for name in sources::NAMES {
            let path = caller.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let body = if *name == "security/vm/debian-cloud.json" {
                bytes(&json!({
                    "distribution":"Debian 13",
                    "architecture":"amd64",
                    "sha512":"c".repeat(128)
                }))
            } else {
                format!("inert selected first-party source {name}\n").into_bytes()
            };
            fs::write(path, body).unwrap();
        }
        for name in ["git", "python3"] {
            let path = acquisition.root.join(name);
            fs::write(&path, b"#!/bin/sh\nexit 99\n").unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let output = acquisition.root.join("runtime");
        let request = acquisition.root.join("runtime-request.json");
        let wire = json!({"schemaVersion":1,"selection":acquisition.wire["selection"],
            "artifactId":456,"output":output,"authentication":"gh-token"});
        fs::write(&request, bytes(&wire)).unwrap();
        fs::set_permissions(&request, fs::Permissions::from_mode(0o600)).unwrap();
        Self {
            acquisition,
            caller,
            output,
            request,
            wire,
        }
    }
    pub fn pin(&self) -> String {
        super::super::sha(&fs::read(&self.request).unwrap())
    }
}

type ReceiptMutation = Box<dyn FnMut(&mut Value) + Send>;

pub(super) struct Fake {
    pub acquire: Acquirer,
    pub runtime_calls: usize,
    pub keeper_calls: usize,
    pub runtime_fail: Option<(usize, u8)>,
    pub runtime_advance: Option<(usize, Duration)>,
    pub mutate: Option<Box<dyn FnMut(usize) + Send>>,
    pub receipt_mutate: Option<ReceiptMutation>,
    pub source_archive: Vec<u8>,
    pub git_status: Vec<u8>,
    pub revision: String,
    pub keeper_arguments: Vec<std::ffi::OsString>,
    pub runtime_directories: Vec<PathBuf>,
    pub transfer_members: Vec<(String, u32, Vec<u8>)>,
    pub setup_late: Option<PathBuf>,
}
impl Fake {
    pub fn new(fixture: &Fixture) -> Self {
        let revision = fixture.wire["selection"]["revision"]
            .as_str()
            .unwrap()
            .to_owned();
        Self {
            acquire: Acquirer::new(&fixture.acquisition),
            runtime_calls: 0,
            keeper_calls: 0,
            runtime_fail: None,
            runtime_advance: None,
            mutate: None,
            receipt_mutate: None,
            source_archive: source_archive(&fixture.caller, &revision),
            git_status: Vec::new(),
            revision,
            keeper_arguments: Vec::new(),
            runtime_directories: Vec::new(),
            transfer_members: Vec::new(),
            setup_late: None,
        }
    }
    pub fn clock(&self) -> Arc<Clock> {
        self.acquire.clock.clone()
    }
    fn keeper(&mut self, command: &mut Command) -> Result<Vec<u8>> {
        self.keeper_calls += 1;
        self.keeper_arguments = command.arguments.clone();
        let file = Arc::try_unwrap(command.stdin.take().unwrap()).unwrap();
        let result = {
            let mut source = &file;
            source.seek(SeekFrom::Start(0)).unwrap();
            let mut transfer = Vec::new();
            source.read_to_end(&mut transfer).unwrap();
            let mut archive = tar::Archive::new(transfer.as_slice());
            for member in archive.entries().unwrap() {
                let mut member = member.unwrap();
                let name = member.path().unwrap().to_str().unwrap().to_owned();
                let mode = member.header().mode().unwrap();
                assert!(member.header().entry_type().is_file());
                assert_eq!(member.header().as_bytes()[257..263], *b"ustar\0");
                let mut body = Vec::new();
                member.read_to_end(&mut body).unwrap();
                self.transfer_members.push((name, mode, body));
            }
            let values = command
                .arguments
                .windows(2)
                .filter_map(|pair| {
                    pair[0]
                        .to_str()
                        .filter(|s| s.starts_with("--"))
                        .map(|key| (key.to_owned(), pair[1].to_str().unwrap().to_owned()))
                })
                .collect::<BTreeMap<_, _>>();
            let value = |name: &str| values.get(name).unwrap().clone();
            let number = |name: &str| value(name).parse::<u64>().unwrap();
            assert_eq!(transfer.len() as u64, number("--transfer-bytes"));
            assert_eq!(super::super::sha(&transfer), value("--transfer-sha256"));
            assert_eq!(
                self.transfer_members
                    .iter()
                    .map(|p| p.0.as_str())
                    .collect::<Vec<_>>(),
                ["ops-release.json", "ops-selection.json", "simplestchat-ops"]
            );
            for ((_, mode, body), prefix) in
                self.transfer_members
                    .iter()
                    .zip(["manifest", "selection", "binary"])
            {
                assert_eq!(*mode, 0o600);
                assert_eq!(body.len() as u64, number(&format!("--{prefix}-bytes")));
                assert_eq!(
                    super::super::sha(body),
                    value(&format!("--{prefix}-sha256"))
                );
            }
            let executable: Value = serde_json::from_str(&value("--executable-json")).unwrap();
            assert_eq!(executable.as_object().unwrap().len(), 19);
            assert_eq!(executable["sha256"], value("--binary-sha256"));
            assert_eq!(executable["bytes"], number("--binary-bytes"));
            let install = value("--install-request-json");
            assert_eq!(
                super::super::sha(install.as_bytes()),
                value("--install-request-sha256")
            );
            let mut receipt = json!({
                "schemaVersion":1,"scope":"OpsDebianKeeperReceipt","runId":value("--run-id"),
                "selection":serde_json::from_str::<Value>(&value("--selection-json")).unwrap(),
                "artifactId":number("--artifact-id"),"transferSha256":value("--transfer-sha256"),
                "transferBytes":number("--transfer-bytes"),
                "manifestSha256":value("--manifest-sha256"),
                "manifestBytes":number("--manifest-bytes"),
                "selectionSha256":value("--selection-sha256"),
                "selectionBytes":number("--selection-bytes"),
                "binarySha256":value("--binary-sha256"),
                "binaryBytes":number("--binary-bytes"),
                "installRequestSha256":value("--install-request-sha256"),
                "cloudImageSha512":"c".repeat(128),
                "install":{"firstChanged":true,"secondChanged":false},
                "cases":receipt::CASES.map(|(name,status)|json!({
                    "name":name,"exitStatus":status,"passed":true
                })),
                "guestExecuted":true,"runtimeValidated":true,
                "installValidated":true,"cleanupPassed":true,
                "passed":true,"failure":null,"vmLifecycle":{
                    "qemuReaped":true,"qemuReturnCode":0,
                    "authenticatedSsh":true,"drainerCompleted":true,
                    "qmpConnected":true,"qmpGreetingReceived":true,"qmpCapabilitiesNegotiated":true,
                    "qmpEofObserved":true,"qmpPassed":true,
                    "cloudInitPassed":true,"closeReturned":true}
            });
            if let Some(mutate) = &mut self.receipt_mutate {
                mutate(&mut receipt);
            }
            bytes(&receipt)
        };
        let closed = nix::unistd::close(file).map_err(|_| super::failure(1));
        closed.map(|()| result)
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        if command.program == Program::OpsGh {
            return self.acquire.run(command).await;
        }
        let index = self.runtime_calls;
        self.runtime_calls += 1;
        let proof = command.ops_runtime.clone().unwrap();
        let admitted = (|| {
            proof.begin(&command)?;
            let (directory, environment) = proof.context(command.program, &command.arguments)?;
            self.runtime_directories.push(directory.into());
            for key in [
                "GH_TOKEN",
                "GITHUB_TOKEN",
                "GH_DEBUG",
                "HTTPS_PROXY",
                "PYTHONPATH",
                "PYTHONHOME",
            ] {
                assert!(!environment.contains_key(std::ffi::OsStr::new(key)));
            }
            Ok(())
        })();
        if let Err(error) = admitted {
            let _ = crate::ci::process::settle_unforwarded(command).await;
            return Err(error);
        }
        let output = match command.program {
            Program::OpsRuntimeGit => match index {
                0 => format!("{}\n", self.revision).into_bytes(),
                1 => self.git_status.clone(),
                2 => self.source_archive.clone(),
                _ => panic!("unexpected Git call"),
            },
            Program::OpsRuntimePython => self.keeper(&mut command)?,
            _ => panic!("unexpected runtime role"),
        };
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut writer = logs.stdout.writer()?;
        // Oversized negative receipts must drain concurrently even on the
        // current-thread runtime; a full blocking pipe cannot drive that task.
        let writer = tokio::task::spawn_blocking(move || {
            let written = writer.write_all(&output).map_err(|_| super::failure(1));
            let closed = nix::unistd::close(writer).map_err(|_| super::failure(1));
            written.and(closed)
        });
        let (written, stdout, stderr) =
            tokio::join!(writer, logs.stdout.finish(), logs.stderr.finish());
        let seal = |value: crate::ci::bounded_log::Finished| super::super::verify::tools::Seal {
            sha256: value.sha256,
            bytes: value.bytes,
            metadata: value.metadata,
        };
        let captures = match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) if stdout.complete && stderr.complete => {
                Some(super::tools::Captures {
                    stdout: seal(stdout),
                    stderr: seal(stderr),
                })
            }
            _ => None,
        };
        let complete = captures.is_some();
        proof.observed_io(captures);
        if let Some(mutate) = &mut self.mutate {
            mutate(index);
        }
        if let Some((at, duration)) = self.runtime_advance
            && at == index
        {
            self.acquire.clock.advance(duration);
        }
        if let Some((at, code)) = self.runtime_fail
            && at == index
        {
            return Err(Failure::child_exit(code));
        }
        written.map_err(|_| super::failure(1))??;
        if !complete {
            return Err(super::failure(1));
        }
        Ok(Vec::new())
    }
    async fn ready(&mut self) -> Result<()> {
        if self.runtime_calls == 3
            && self.acquire.calls == 9
            && self.setup_late.as_ref().is_some_and(|p| p.is_file())
        {
            self.setup_late = None;
            self.acquire.clock.advance(Duration::from_secs(31));
        }
        self.acquire.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.acquire.cleanup_passed()
    }
    async fn replay(&mut self, bytes: &[u8], error: bool, end: Instant) -> Result<()> {
        self.acquire.replay(bytes, error, end).await
    }
}

pub(super) fn expect_error<T>(result: Result<T>, code: u8) {
    match result {
        Err(Failure {
            code: actual,
            message,
            ..
        }) => {
            assert_eq!(actual, code);
            assert!(!message.contains("PRIVATE"));
        }
        Ok(_) => panic!("expected refusal"),
    }
}

pub(super) fn cancelled(fake: &Fake, code: u8) {
    fake.acquire.cancelled.store(code, Ordering::SeqCst);
}
