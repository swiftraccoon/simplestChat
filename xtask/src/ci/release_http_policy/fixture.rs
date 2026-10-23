//! Entirely owned files and inert responses; no Docker, sockets or TLS endpoint.

use super::*;
use crate::ci::{
    command::Command,
    ops_artifact::verify::tools::{Captures, Seal},
    process::Executor,
};
use sha2::{Digest, Sha256};
use std::{
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

pub(super) struct Fixture {
    pub root: PathBuf,
    pub config: config::Config,
    pub tool: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "release-http-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let ca = root.join("fixture.pem");
        fs::write(&ca, b"inert ascii CA").unwrap();
        let tool = root.join("selected docker");
        fs::write(&tool, b"inert executable").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o700)).unwrap();
        let config = config::Config {
            token: "c".repeat(32),
            caddy: "a".repeat(64),
            app: "b".repeat(64),
            ca,
            sha256: crate::ci::io::hex(&Sha256::digest(b"inert ascii CA")),
        };
        Self { root, config, tool }
    }
    pub fn owners(&self) -> (tools::Tools, files::Scratch, ca::Ca) {
        let (ca, _) = ca::Ca::open(&self.config).unwrap();
        (
            tools::Tools::open(&self.tool).unwrap(),
            files::Scratch::open(&self.root).unwrap(),
            ca,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
pub(super) struct Clock {
    pub start: Instant,
    pub elapsed: Mutex<Duration>,
}
impl Clock {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            elapsed: Mutex::new(Duration::ZERO),
        })
    }
    pub fn set(&self, seconds: u64) {
        *self.elapsed.lock().unwrap() = Duration::from_secs(seconds);
    }
}
impl plan::Clock for Clock {
    fn now(&self) -> Instant {
        self.start + *self.elapsed.lock().unwrap()
    }
}
pub(super) fn inspection(config: &config::Config, index: usize) -> Vec<u8> {
    serde_json::to_vec(
        &serde_json::json!({"id":if index.is_multiple_of(2) {&config.caddy} else {&config.app},
        "running":true,"labels":{"simplestchat.release-test":config.token,
        "com.docker.compose.project":"simplestchat-public",
        "com.docker.compose.service":if index.is_multiple_of(2) {"caddy"} else {"simplestchat"}},
        "ports":{"443/tcp":[{"HostIp":"127.0.0.1","HostPort":"443"}],"80/tcp":null}}),
    )
    .unwrap()
}
pub(super) fn response(index: usize) -> native::Response {
    let table: serde_json::Value =
        serde_json::from_str(include_str!("../../../../security/http-policy.json")).unwrap();
    let mut headers = table["headers"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect::<std::collections::BTreeMap<_, _>>();
    headers.insert(
        "strict-transport-security".into(),
        "max-age=31536000".into(),
    );
    headers.insert(
        "content-security-policy".into(),
        table["contentSecurityPolicy"].as_str().unwrap().into(),
    );
    headers.insert(
        "content-type".into(),
        policy::ENDPOINTS[index].content_type.into(),
    );
    headers.insert("cache-control".into(), "no-store".into());
    let body = match index {
        0 => b"<!DOCTYPE html><html>Fixture</html>".to_vec(),
        1 => serde_json::to_vec(&serde_json::json!({"version":1,"accounts":true,
            "passwordLogin":true,"passkeyLogin":false,"passwordRegistration":"invite",
            "passkeyRegistration":"disabled","roomDirectory":true,"roomCreation":true,
            "adHocRooms":false}))
        .unwrap(),
        2 => br#"{"error":"Missing authorization"}"#.to_vec(),
        _ => b"Not Found".to_vec(),
    };
    native::Response {
        status: policy::ENDPOINTS[index].status,
        headers,
        body,
    }
}
pub(super) struct Http {
    pub calls: Arc<Mutex<Vec<String>>>,
    pub fail_at: Option<usize>,
    pub late_at: Option<usize>,
    pub clock: Arc<Clock>,
    pub pending: bool,
}
impl Http {
    pub fn new(clock: Arc<Clock>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail_at: None,
            late_at: None,
            clock,
            pending: false,
        }
    }
}
impl native::Http for Http {
    async fn request(&self, endpoint: policy::Endpoint) -> Result<native::Response> {
        let index = {
            let mut calls = self.calls.lock().unwrap();
            let index = calls.len();
            calls.push(endpoint.path.into());
            index
        };
        if self.pending {
            return std::future::pending().await;
        }
        if self.late_at == Some(index) {
            self.clock.set(25);
        }
        if self.fail_at == Some(index) {
            return Err(fail(1, "http_policy_failed"));
        }
        Ok(response(index))
    }
}
pub(super) struct Fake<'a> {
    pub fixture: &'a Fixture,
    pub calls: usize,
    pub fail_at: Option<(usize, u8)>,
    pub bad_owner: Option<usize>,
    pub cancel: Option<u8>,
    pub clean: bool,
    pub clock: Arc<Clock>,
    pub late_setup: bool,
    ready_calls: usize,
    pub interrupt: Arc<AtomicU8>,
}
impl<'a> Fake<'a> {
    pub fn new(fixture: &'a Fixture, clock: Arc<Clock>) -> Self {
        Self {
            fixture,
            calls: 0,
            fail_at: None,
            bad_owner: None,
            cancel: None,
            clean: true,
            clock,
            late_setup: false,
            ready_calls: 0,
            interrupt: Arc::new(AtomicU8::new(0)),
        }
    }
}
pub(super) async fn finish(log: crate::ci::bounded_log::Log) -> Result<Seal> {
    let v = log.finish().await?;
    if !v.complete {
        return Err(fail(1, "fixture_container_inspection"));
    }
    Ok(Seal {
        sha256: v.sha256,
        bytes: v.bytes,
        metadata: v.metadata,
    })
}
impl Executor for Fake<'_> {
    async fn ready(&mut self) -> Result<()> {
        self.ready_calls += 1;
        if self.late_setup && self.ready_calls == 2 {
            self.clock.set(180);
        }
        let code = self
            .cancel
            .unwrap_or_else(|| self.interrupt.load(Ordering::SeqCst));
        if code == 0 {
            Ok(())
        } else {
            Err(fail(code, "http_policy_interrupted"))
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        let proof = command.release_http.take().unwrap();
        proof.begin(&command)?;
        let index = self.calls;
        self.calls += 1;
        assert_eq!(
            command.arguments.last().unwrap().to_str(),
            if index.is_multiple_of(2) {
                Some(self.fixture.config.caddy.as_str())
            } else {
                Some(self.fixture.config.app.as_str())
            }
        );
        let logs = Arc::try_unwrap(command.separate_logs.take().unwrap()).unwrap();
        let mut bytes = inspection(&self.fixture.config, index);
        if self.bad_owner == Some(index) {
            bytes = b"{}".to_vec();
        }
        let mut writer = logs.stdout.writer().unwrap();
        writer.write_all(&bytes).unwrap();
        files::close(writer).unwrap();
        let (out, err) = tokio::join!(finish(logs.stdout), finish(logs.stderr));
        proof.observed_io(
            out.and_then(|stdout| err.map(|stderr| Captures { stdout, stderr }))
                .ok(),
        );
        self.fail_at
            .filter(|(at, _)| *at == index)
            .map_or(Ok(vec![]), |(_, code)| {
                Err(fail(code, "fixed_native_failure"))
            })
    }
}
