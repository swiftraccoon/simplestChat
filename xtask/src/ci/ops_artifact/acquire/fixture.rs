//! Complete inert unsigned/signature-output graph. No cryptography or network runs.

use super::*;
use crate::{
    ci::{command::Command, process::Executor},
    tooling::Environment,
};
use serde_json::{Value, json};
use simplestchat_ops::distribution::manifest::{
    ArtifactManifest, Selection,
    assembly::{EvidenceSources, prepare_artifact},
};
use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub(in crate::ci::ops_artifact) fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
pub(in crate::ci::ops_artifact) struct Fixture {
    pub root: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub request: PathBuf,
    pub wire: Value,
    pub environment: Environment,
    pub outputs: Vec<Vec<u8>>,
}
impl Fixture {
    pub fn new(evidence: usize) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-acquire-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let input = root.join("donor");
        let output = root.join("acquired");
        let binary = super::super::fixture::binary();
        fs::write(root.join("binary"), &binary).unwrap();
        let selected = json!({"repository":"swiftraccoon/simplestChat",
            "revision":"a".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
            "target":"x86_64-unknown-linux-musl","buildRunId":123,"runAttempt":2});
        let mut manifest = selected.clone();
        let object = manifest.as_object_mut().unwrap();
        object.insert("schemaVersion".into(), json!(1));
        object.insert("artifactType".into(), json!("simplestchat-ops"));
        object.insert(
            "binary".into(),
            json!({"name":"simplestchat-ops",
            "sha256":super::super::sha(&binary),"bytes":binary.len()}),
        );
        object.insert(
            "inputDigests".into(),
            json!({"PRIVATE-SOURCE":"b".repeat(64)}),
        );
        let mut digests = serde_json::Map::new();
        let mut sources = BTreeMap::new();
        for n in 0..evidence {
            let data = format!("PRIVATE-EVIDENCE-{n}");
            let path = root.join(format!("evidence-{n}"));
            fs::write(&path, &data).unwrap();
            let name = format!("PRIVATE-LOGICAL/{n:02}");
            digests.insert(name.clone(), json!(super::super::sha(data.as_bytes())));
            sources.insert(name, path);
        }
        object.insert("evidenceDigests".into(), Value::Object(digests));
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend(serde_json::to_vec_pretty(&manifest).unwrap());
        raw.extend(b"\n\n");
        prepare_artifact(
            &ArtifactManifest::decode(&raw).unwrap(),
            &root.join("binary"),
            &Selection::decode(&bytes(&selected)).unwrap(),
            &EvidenceSources::new(sources).unwrap(),
            &input,
        )
        .unwrap();
        fs::write(input.join("ops-attestation.jsonl"), b"INERT-OPAQUE-BUNDLE").unwrap();
        fs::set_permissions(
            input.join("ops-attestation.jsonl"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let repository = json!({"id":1155881649,"full_name":"swiftraccoon/simplestChat",
            "fork":false,"owner":{"id":47705353,"login":"swiftraccoon"}});
        let attempt = json!({"id":123,"run_attempt":2,"head_sha":"a".repeat(40),
            "head_branch":"main","name":"CI","path":".github/workflows/ci.yml",
            "event":"push","status":"completed","conclusion":"success",
            "repository":repository,"head_repository":repository});
        let subjects = [
            "ops-preparation.json",
            "ops-release.json",
            "simplestchat-ops",
        ]
        .map(|name| {
            json!({"name":name,"digest":{"sha256":
                super::super::sha(&fs::read(input.join(name)).unwrap())}})
        });
        let predicate: Value =
            serde_json::from_slice(&fs::read(input.join("ops-predicate.json")).unwrap()).unwrap();
        let verification = json!([{"attestation":{"PRIVATE":"ignored"},"verificationResult":{
            "mediaType":"application/vnd.dev.sigstore.verificationresult+json;version=0.1",
            "signature":{"certificate":{
                "issuer":"https://token.actions.githubusercontent.com",
                "subjectAlternativeName":concat!("https://github.com/swiftraccoon/",
                    "simplestChat/.github/workflows/ci.yml@refs/heads/main"),
                "sourceRepositoryIdentifier":"1155881649",
                "sourceRepositoryOwnerIdentifier":"47705353",
                "sourceRepositoryURI":"https://github.com/swiftraccoon/simplestChat",
                "sourceRepositoryOwnerURI":"https://github.com/swiftraccoon",
                "sourceRepositoryDigest":"a".repeat(40),
                "sourceRepositoryRef":"refs/heads/main","buildSignerDigest":"a".repeat(40),
                "buildTrigger":"push","runnerEnvironment":"github-hosted",
                "runInvocationURI":concat!("https://github.com/swiftraccoon/simplestChat/",
                    "actions/runs/123/attempts/2")}},
            "verifiedTimestamps":[{"type":"TimestampAuthority",
                "uri":"https://timestamp.example.invalid","timestamp":"2026-10-23T00:00:00Z"}],
            "statement":{"_type":"https://in-toto.io/Statement/v1",
                "predicateType":concat!("https://github.com/swiftraccoon/simplestChat/",
                    "attestations/ops-artifact/v1"),"subject":subjects,"predicate":predicate}}}]);
        let mut archive = rawzip::ZipArchiveWriter::new(Vec::new());
        let mut entries = fs::read_dir(&input)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect::<Vec<_>>();
        entries.sort();
        for name in entries {
            let data = fs::read(input.join(&name)).unwrap();
            let (mut entry, config) = archive
                .new_file(name.to_str().unwrap())
                .compression_method(rawzip::CompressionMethod::STORE)
                .unix_permissions(0o100600)
                .start()
                .unwrap();
            let mut writer = config.wrap(&mut entry);
            writer.write_all(&data).unwrap();
            let (_, descriptor) = writer.finish().unwrap();
            entry.finish(descriptor).unwrap();
        }
        let zip = archive.finish().unwrap();
        let url = "https://api.github.com/repos/swiftraccoon/simplestChat/actions/artifacts/456";
        let artifact = json!({"id":456,
            "name":format!("simplestchat-ops-{}-x86_64-unknown-linux-musl", "a".repeat(40)),
            "url":url,"archive_download_url":format!("{url}/zip"),"expired":false,
            "size_in_bytes":zip.len(),"digest":format!("sha256:{}", super::super::sha(&zip)),
            "workflow_run":{"id":123,"head_sha":"a".repeat(40),
                "repository_id":1155881649,"head_repository_id":1155881649}});
        let wire = json!({"schemaVersion":1,"selection":selected,"artifactId":456,
            "output":output,"authentication":"gh-token"});
        let request = root.join("request.json");
        fs::write(&request, bytes(&wire)).unwrap();
        fs::write(root.join("gh"), b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(root.join("gh"), fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), root.clone().into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE-TOKEN".into()),
            ("GITHUB_TOKEN".into(), "PRIVATE-OTHER-TOKEN".into()),
            ("GH_DEBUG".into(), "PRIVATE-DEBUG".into()),
            ("HTTPS_PROXY".into(), "PRIVATE-PROXY".into()),
        ]);
        let version = b"gh version 2.102.0 (inert)\n".to_vec();
        let outputs = vec![
            version.clone(),
            bytes(&repository),
            bytes(&attempt),
            bytes(&artifact),
            zip,
            version,
            bytes(&repository),
            bytes(&attempt),
            bytes(&verification),
        ];
        Self {
            root,
            input,
            output,
            request,
            wire,
            environment,
            outputs,
        }
    }
    pub fn pin(&self) -> String {
        super::super::sha(&fs::read(&self.request).unwrap())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
pub(in crate::ci::ops_artifact) struct Clock(pub Mutex<Instant>);
impl Clock {
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }
    pub fn advance(&self, duration: Duration) {
        *self.0.lock().unwrap() += duration;
    }
}
impl plan::Clock for Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
pub(in crate::ci::ops_artifact) struct Fake {
    pub outputs: Vec<Vec<u8>>,
    pub calls: usize,
    pub fail: Option<(usize, u8)>,
    pub clock: Arc<Clock>,
    pub advance: Option<(usize, Duration)>,
    pub mutate: Option<Box<dyn FnMut(usize) + Send>>,
    pub cancelled: Arc<AtomicU8>,
    pub cleanup: bool,
    pub replays: Vec<Vec<u8>>,
    pub replay_cancel: u8,
    pub replay_advance: Duration,
    pub setup_late: Option<PathBuf>,
    pub replay_mutate: Option<Box<dyn FnMut() + Send>>,
    pub replay_fail: Option<u8>,
}
impl Fake {
    pub fn new(fixture: &Fixture) -> Self {
        Self {
            outputs: fixture.outputs.clone(),
            calls: 0,
            fail: None,
            clock: Arc::new(Clock::new()),
            advance: None,
            mutate: None,
            cancelled: Arc::new(AtomicU8::new(0)),
            cleanup: true,
            replays: Vec::new(),
            replay_cancel: 0,
            replay_advance: Duration::ZERO,
            setup_late: None,
            replay_mutate: None,
            replay_fail: None,
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let index = self.calls;
        self.calls += 1;
        let acquire = command.ops_acquire.clone();
        let retained = command.retained_ops.clone();
        let environment = if let Some(proof) = &acquire {
            proof.begin(&command)?;
            proof.context(command.program, &command.arguments)?.1
        } else {
            let proof = retained.as_ref().unwrap();
            proof.begin()?;
            proof.context(command.program, &command.arguments)?.1
        };
        assert_eq!(
            environment[std::ffi::OsStr::new("GH_TOKEN")],
            "PRIVATE-TOKEN"
        );
        assert_eq!(
            environment[std::ffi::OsStr::new("GH_TELEMETRY")],
            "disabled"
        );
        for key in [
            "GH_DEBUG",
            "GITHUB_TOKEN",
            "HTTPS_PROXY",
            "SSL_CERT_FILE",
            "NODE_OPTIONS",
        ] {
            assert!(!environment.contains_key(std::ffi::OsStr::new(key)));
        }
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut writer = logs.stdout.writer()?;
        let written = writer
            .write_all(&self.outputs[index])
            .map_err(|_| failure(1));
        let closed = nix::unistd::close(writer).map_err(|_| failure(1));
        let (stdout, stderr) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
        let seal = |value: crate::ci::bounded_log::Finished| super::super::verify::tools::Seal {
            sha256: value.sha256,
            bytes: value.bytes,
            metadata: value.metadata,
        };
        let captures = match (stdout, stderr) {
            (Ok(stdout), Ok(stderr)) if stdout.complete && stderr.complete => {
                Some(tools::Captures {
                    stdout: seal(stdout),
                    stderr: seal(stderr),
                })
            }
            _ => None,
        };
        let complete = captures.is_some();
        if let Some(proof) = acquire {
            proof.observed_io(captures);
        } else {
            retained.unwrap().observed_io(captures);
        }
        if let Some(mutate) = &mut self.mutate {
            mutate(index);
        }
        if let Some((at, duration)) = self.advance
            && at == index
        {
            self.clock.advance(duration);
        }
        if let Some((at, code)) = self.fail
            && at == index
        {
            return Err(crate::tooling::Failure::child_exit(code));
        }
        written?;
        closed?;
        if !complete {
            return Err(failure(1));
        }
        Ok(Vec::new())
    }
    async fn ready(&mut self) -> Result<()> {
        if self.setup_late.as_ref().is_some_and(|path| path.is_file()) && self.calls == 0 {
            self.setup_late = None;
            self.clock.advance(Duration::from_secs(1441));
        }
        match self.cancelled.load(Ordering::SeqCst) {
            0 => Ok(()),
            code => Err(failure(code)),
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.cleanup
    }
    async fn replay(&mut self, bytes: &[u8], _: bool, _: Instant) -> Result<()> {
        self.replays.push(bytes.into());
        if let Some(mutate) = &mut self.replay_mutate {
            mutate();
        }
        self.clock.advance(self.replay_advance);
        self.cancelled.store(self.replay_cancel, Ordering::SeqCst);
        if let Some(code) = self.replay_fail {
            Err(failure(code))
        } else {
            Ok(())
        }
    }
}
