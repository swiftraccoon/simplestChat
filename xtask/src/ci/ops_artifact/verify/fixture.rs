//! Owned unsigned graph and synthetic successful-verifier observations.
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
pub(super) fn bytes(value: &Value) -> Vec<u8> {
    serde_json::to_vec(value).unwrap()
}
pub(super) struct Fixture {
    pub root: PathBuf,
    pub input: PathBuf,
    pub output: PathBuf,
    pub request: PathBuf,
    pub wire: Value,
    pub environment: Environment,
    pub observations: Vec<Vec<u8>>,
}
impl Fixture {
    pub fn new(evidence: usize) -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-retained-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let input = root.join("input");
        let output = root.join("output");
        let binary = super::super::fixture::binary();
        fs::write(root.join("binary"), &binary).unwrap();
        let selected = json!({"repository":"swiftraccoon/simplestChat",
            "revision":"a".repeat(40),"packageVersion":"0.1.0","protocolVersion":1,
            "target":"x86_64-unknown-linux-musl","buildRunId":123,"runAttempt":2});
        let mut manifest = selected.clone();
        let map = manifest.as_object_mut().unwrap();
        map.insert("schemaVersion".into(), json!(1));
        map.insert("artifactType".into(), json!("simplestchat-ops"));
        map.insert(
            "binary".into(),
            json!({"name":"simplestchat-ops",
            "sha256":super::super::sha(&binary),"bytes":binary.len()}),
        );
        map.insert(
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
            sources.insert(name.clone(), path);
            digests.insert(name, json!(super::super::sha(data.as_bytes())));
        }
        map.insert("evidenceDigests".into(), Value::Object(digests));
        let mut raw = b"\xef\xbb\xbf".to_vec();
        raw.extend(serde_json::to_vec_pretty(&manifest).unwrap());
        raw.extend(b"\n\n");
        let manifest = ArtifactManifest::decode(&raw).unwrap();
        let selection = Selection::decode(&bytes(&selected)).unwrap();
        prepare_artifact(
            &manifest,
            &root.join("binary"),
            &selection,
            &EvidenceSources::new(sources).unwrap(),
            &input,
        )
        .unwrap();
        fs::write(input.join(BUNDLE), b"INERT-OPAQUE-BUNDLE").unwrap();
        fs::set_permissions(input.join(BUNDLE), fs::Permissions::from_mode(0o600)).unwrap();
        let repository = json!({"id":1155881649,"full_name":"swiftraccoon/simplestChat",
            "fork":false,"owner":{"id":47705353,"login":"swiftraccoon"}});
        let attempt = json!({"id":123,"run_attempt":2,"head_sha":"a".repeat(40),
            "head_branch":"main","name":"CI","path":".github/workflows/ci.yml",
            "event":"push","status":"completed","conclusion":"success",
            "repository":repository,"head_repository":repository});
        let predicate: Value =
            serde_json::from_slice(&fs::read(input.join("ops-predicate.json")).unwrap()).unwrap();
        let subjects = [
            "ops-preparation.json",
            "ops-release.json",
            "simplestchat-ops",
        ]
        .map(|name| {
            json!({"name":name,"digest":{"sha256":
                super::super::sha(&fs::read(input.join(name)).unwrap())}})
        });
        let certificate = json!({
            "certificateIssuer":"PRIVATE-DN",
            "subjectAlternativeName":concat!("https://github.com/swiftraccoon/simplestChat/",
                ".github/workflows/ci.yml@refs/heads/main"),
            "issuer":"https://token.actions.githubusercontent.com",
            "sourceRepositoryIdentifier":"1155881649",
            "sourceRepositoryOwnerIdentifier":"47705353",
            "sourceRepositoryURI":"https://github.com/swiftraccoon/simplestChat",
            "sourceRepositoryOwnerURI":"https://github.com/swiftraccoon",
            "sourceRepositoryDigest":"a".repeat(40),"sourceRepositoryRef":"refs/heads/main",
            "buildSignerDigest":"a".repeat(40),"buildTrigger":"push",
            "runnerEnvironment":"github-hosted",
            "runInvocationURI":concat!("https://github.com/swiftraccoon/simplestChat/",
                "actions/runs/123/attempts/2")});
        let verification = json!([{"attestation":{"PRIVATE":"ignored"},
            "verificationResult":{"mediaType":
                concat!("application/vnd.dev.sigstore.verificationresult+json;",
                    "version=0.1"),
                "signature":{"certificate":certificate},"verifiedTimestamps":[{
                    "type":"TimestampAuthority","uri":"https://timestamp.example.invalid",
                    "timestamp":"2026-10-22T00:00:00Z"}],"statement":{
                    "_type":"https://in-toto.io/Statement/v1",
                    "predicateType":concat!("https://github.com/swiftraccoon/simplestChat/",
                        "attestations/ops-artifact/v1"),
                    "subject":subjects,"predicate":predicate}}}]);
        let wire = json!({"schemaVersion":1,"selection":selected,"directory":input,
            "output":output,"authentication":"gh-token"});
        let request = root.join("request.json");
        fs::write(&request, bytes(&wire)).unwrap();
        fs::write(root.join("gh"), b"#!/bin/sh\nexit 99\n").unwrap();
        fs::set_permissions(root.join("gh"), fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), root.clone().into_os_string()),
            ("GH_TOKEN".into(), "PRIVATE-TOKEN".into()),
            ("GH_DEBUG".into(), "PRIVATE-DEBUG".into()),
            ("HTTPS_PROXY".into(), "PRIVATE-PROXY".into()),
        ]);
        Self {
            root,
            input,
            output,
            request,
            wire,
            environment,
            observations: vec![
                b"gh version 2.102.0 (fixture)\n".to_vec(),
                bytes(&repository),
                bytes(&attempt),
                bytes(&verification),
            ],
        }
    }
    pub fn pin(&self) -> String {
        super::super::sha(&fs::read(&self.request).unwrap())
    }
    pub fn save_request(&self) {
        fs::write(&self.request, bytes(&self.wire)).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

pub(super) struct Clock(pub Mutex<Instant>);
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
pub(super) struct Fake {
    pub outputs: Vec<Vec<u8>>,
    pub calls: usize,
    pub fail: Option<(usize, u8)>,
    pub cancelled: Arc<AtomicU8>,
    pub replay: Vec<u8>,
    pub cleanup: bool,
    pub clock: Arc<Clock>,
    pub advance_on_call: Option<(usize, Duration)>,
    pub mutate: Option<Box<dyn FnMut(usize) + Send>>,
    pub late_setup: Option<PathBuf>,
    pub replay_cancel: u8,
    pub replay_advance: Duration,
    pub replay_unproven: bool,
}
impl Fake {
    pub fn new(fixture: &Fixture, clock: Arc<Clock>) -> Self {
        Self {
            outputs: fixture.observations.clone(),
            calls: 0,
            fail: None,
            cancelled: Arc::new(AtomicU8::new(0)),
            replay: Vec::new(),
            cleanup: true,
            clock,
            advance_on_call: None,
            mutate: None,
            late_setup: None,
            replay_cancel: 0,
            replay_advance: Duration::ZERO,
            replay_unproven: false,
        }
    }
}
impl Executor for Fake {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let index = self.calls;
        self.calls += 1;
        let proof = command.retained_ops.take().unwrap();
        proof.begin()?;
        proof.validate(command.program, &command.arguments)?;
        let (_, environment) = proof.context(command.program, &command.arguments)?;
        assert_eq!(
            environment.get(std::ffi::OsStr::new("GH_TOKEN")).unwrap(),
            "PRIVATE-TOKEN"
        );
        assert_eq!(
            environment
                .get(std::ffi::OsStr::new("GH_TELEMETRY"))
                .unwrap(),
            "disabled"
        );
        for key in ["GH_DEBUG", "HTTPS_PROXY", "GITHUB_TOKEN", "SSL_CERT_FILE"] {
            assert!(!environment.contains_key(std::ffi::OsStr::new(key)));
        }
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut writer = logs.stdout.writer()?;
        writer.write_all(&self.outputs[index]).unwrap();
        nix::unistd::close(writer).unwrap();
        let (stdout, stderr) = tokio::join!(logs.stdout.finish(), logs.stderr.finish());
        let seal = |finished: crate::ci::bounded_log::Finished| tools::Seal {
            sha256: finished.sha256,
            bytes: finished.bytes,
            metadata: finished.metadata,
        };
        proof.observed_io(Some(tools::Captures {
            stdout: seal(stdout?),
            stderr: seal(stderr?),
        }));
        if let Some(mutate) = &mut self.mutate {
            mutate(index);
        }
        if let Some((at, duration)) = self.advance_on_call
            && at == index
        {
            self.clock.advance(duration);
        }
        if let Some((at, code)) = self.fail
            && at == index
        {
            return Err(failure(code));
        }
        Ok(Vec::new())
    }
    async fn ready(&mut self) -> Result<()> {
        if self.late_setup.as_ref().is_some_and(|path| path.is_file()) && self.calls == 0 {
            self.clock.advance(Duration::from_secs(601));
            self.late_setup = None;
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
        self.replay = bytes.into();
        self.clock.advance(self.replay_advance);
        self.cancelled.store(self.replay_cancel, Ordering::SeqCst);
        if self.replay_unproven {
            self.cleanup = false;
        }
        Ok(())
    }
}
