//! Real private files and virtual children; no browser/server/socket is started.

use super::*;
use crate::bench::process::Stream;
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Fault {
    None,
    Ports,
    Ready,
    ServerSpawn,
    NodeSpawn,
    NodeClose,
    ServerClose,
    ServerExit,
    ServerStoppedExit(i32),
    Cancel(u8),
    Timeout,
    PollError(u8),
    MissingReceipt,
    Unknown,
    NestedUnknown,
    WrongRun,
    UnobservedBrowser,
    ForcedBrowser,
    MissingNullable,
    Namespace,
    Publish,
    LateCancel,
    LateDeadline,
    EntryChanged,
}
#[derive(Default)]
struct Journal {
    calls: Vec<(
        String,
        Vec<String>,
        super::super::plan::Environment,
        Duration,
    )>,
    stopped: Vec<String>,
    active: usize,
}
pub(super) struct Fixture {
    pub root: PathBuf,
    state: Arc<Mutex<Journal>>,
    time: Arc<AtomicU64>,
    cancellation: Arc<AtomicU8>,
    fault: Fault,
    node_status: i32,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
pub(super) fn executable(path: &std::path::Path, bytes: &[u8]) {
    fs::write(path, bytes).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
impl Fixture {
    pub fn new(fault: Fault, node_status: i32) -> (Self, Plan) {
        let root = std::env::temp_dir().join(format!("browser mechanics {}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        fs::create_dir_all(root.join("web/e2e")).unwrap();
        executable(&root.join("server"), b"owned inert server");
        executable(&root.join("node"), b"owned inert node");
        fs::write(
            root.join("web/e2e/browser-mechanics.cjs"),
            b"owned inert entry",
        )
        .unwrap();
        let env = super::super::plan::Environment::from([
            ("PATH".into(), root.clone().into_os_string()),
            ("HOME".into(), root.clone().into_os_string()),
            (
                "SIMPLESTCHAT_BIN".into(),
                root.join("server").into_os_string(),
            ),
            ("DATABASE_URL".into(), "PRIVATE_SENTINEL".into()),
            ("NODE_PATH".into(), "PRIVATE_SENTINEL".into()),
            (
                "PLAYWRIGHT_BROWSERS_PATH".into(),
                "/owned/runtime with spaces".into(),
            ),
            (
                "LIBWEBRTC_FIELD_TRIALS".into(),
                "literal trial/value/".into(),
            ),
            ("WEBRTC_MIN_OUTGOING_BITRATE".into(), "0".into()),
            ("WEBRTC_SERVER_TCP".into(), "true".into()),
        ]);
        let plan = Plan::parse(
            &root,
            &["--output".into(), root.join("output").into_os_string()],
            env,
        )
        .unwrap();
        (
            Self {
                root,
                state: Arc::default(),
                time: Arc::default(),
                cancellation: Arc::default(),
                fault,
                node_status,
            },
            plan,
        )
    }
    fn services(&self) -> Services<'_> {
        Services {
            host: self,
            clock: self,
            ports: self,
            readiness: self,
            authority: &NativeAuthority,
        }
    }
    fn evidence(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.root.join("output/browser-mechanics-exit.json")).unwrap(),
        )
        .unwrap()
    }
}
pub(super) fn receipt(run_id: &str) -> Value {
    json!({"schemaVersion":1,"runId":run_id,"completed":true,"phasePassed":true,
        "browserCleanupPassed":true,"cancelled":null,"browserCount":1,
        "records":[{"browserName":"chromium","launchMode":"browser-server",
            "completed":true,"passed":true,"gracefulCloseCompleted":true,
            "forceKillRequested":false,"exitStatusObserved":true,"exitCode":0,
            "signalCode":null}]})
}
#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        self.time.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        "2026-10-22T00:00:00Z".into()
    }
    async fn sleep(&self, duration: Duration) {
        self.time
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Ports for Fixture {
    fn available(&self, port: u16, media: u16, workers: u32) -> Result<()> {
        assert_eq!((port, media, workers), (3109, 41100, 1));
        ensure!(self.fault != Fault::Ports, "PRIVATE_SENTINEL");
        Ok(())
    }
}
#[async_trait::async_trait]
impl controller::Readiness for Fixture {
    async fn ready(&self) -> bool {
        self.fault != Fault::Ready
    }
}
struct FakeChild {
    state: Arc<Mutex<Journal>>,
    time: Arc<AtomicU64>,
    cancellation: Arc<AtomicU8>,
    name: String,
    fault: Fault,
    status: i32,
    run_id: String,
    root: PathBuf,
    stdout: Option<Sink>,
    stderr: Option<Sink>,
    output: Vec<u8>,
    stopped: bool,
}
#[async_trait::async_trait]
impl Child for FakeChild {
    fn pid(&self) -> u32 {
        12345
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if self.name == "server" {
            return Ok((self.fault == Fault::ServerExit).then_some(Exit {
                code: Some(42),
                signal: None,
            }));
        }
        match self.fault {
            Fault::Cancel(code) => {
                self.cancellation.store(code, Ordering::SeqCst);
                return Ok(None);
            }
            Fault::Timeout => {
                self.time.fetch_add(1_000_000, Ordering::SeqCst);
                return Ok(None);
            }
            Fault::PollError(code) => {
                self.time.fetch_add(1_000_000, Ordering::SeqCst);
                return match code {
                    124 => Err(crate::owned_process::child::CommandTimedOut.into()),
                    130 | 143 => Err(crate::owned_process::Interrupted(code).into()),
                    _ => Err(anyhow::anyhow!("PRIVATE_SENTINEL")),
                };
            }
            _ => {}
        }
        Ok(Some(Exit {
            code: Some(self.status),
            signal: None,
        }))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        assert_eq!(
            grace,
            Duration::from_secs(if self.name == "node" { 20 } else { 5 })
        );
        assert!(!self.stopped);
        self.stopped = true;
        for sink in [&mut self.stdout, &mut self.stderr] {
            if let Some(Sink::File(mut file)) = sink.take() {
                file.write_all(b"owned inert PRIVATE_SENTINEL log\n")?;
                nix::unistd::close(file)?;
            }
        }
        {
            let mut state = self.state.lock().unwrap();
            state.active -= 1;
            state.stopped.push(self.name.clone());
        }
        if self.name == "node" {
            let mut value = receipt(&self.run_id);
            match self.fault {
                Fault::Unknown => value["private"] = json!("PRIVATE_SENTINEL"),
                Fault::NestedUnknown => value["records"][0]["private"] = json!("PRIVATE_SENTINEL"),
                Fault::WrongRun => value["runId"] = json!("foreign"),
                Fault::UnobservedBrowser => {
                    value["records"][0]["exitStatusObserved"] = json!(false)
                }
                Fault::ForcedBrowser => value["records"][0]["forceKillRequested"] = json!(true),
                Fault::MissingNullable => {
                    value.as_object_mut().unwrap().remove("cancelled");
                }
                _ => {}
            }
            if self.fault != Fault::MissingReceipt {
                self.output = serde_json::to_vec(&value)?;
            }
        }
        if self.name == "server" {
            match self.fault {
                Fault::LateCancel => self.cancellation.store(130, Ordering::SeqCst),
                Fault::LateDeadline => {
                    self.time.store(1_000_000, Ordering::SeqCst);
                }
                Fault::EntryChanged => fs::write(
                    self.root.join("web/e2e/browser-mechanics.cjs"),
                    b"changed owned entry",
                )?,
                Fault::Namespace => {
                    fs::rename(self.root.join("output"), self.root.join("retired"))?;
                    fs::create_dir(self.root.join("output"))?;
                }
                _ => {}
            }
        }
        ensure!(
            !((self.name == "node" && self.fault == Fault::NodeClose)
                || (self.name == "server" && self.fault == Fault::ServerClose)),
            "PRIVATE_SENTINEL"
        );
        Ok(Exit {
            code: Some(if self.name == "server" {
                if let Fault::ServerStoppedExit(status) = self.fault {
                    status
                } else {
                    self.status
                }
            } else {
                self.status
            }),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        assert!(self.stopped);
        &self.output
    }
    fn errors(&self) -> &[u8] {
        &[]
    }
}
#[async_trait::async_trait]
impl Host for Fixture {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let name = command
            .executable
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(matches!(name.as_str(), "server" | "node"));
        let mut state = self.state.lock().unwrap();
        state.calls.push((
            name.clone(),
            command.arguments.clone(),
            command.environment.clone(),
            command.timeout,
        ));
        ensure!(
            !((name == "server" && self.fault == Fault::ServerSpawn)
                || (name == "node" && self.fault == Fault::NodeSpawn)),
            "PRIVATE_SENTINEL"
        );
        if name == "node" && self.fault == Fault::Publish {
            fs::create_dir(self.root.join("output/browser-mechanics-owner.json"))?;
        }
        state.active += 1;
        Ok(Box::new(FakeChild {
            state: self.state.clone(),
            time: self.time.clone(),
            cancellation: self.cancellation.clone(),
            fault: self.fault,
            status: if name == "node" { self.node_status } else { 0 },
            run_id: command
                .environment
                .get(std::ffi::OsStr::new("BROWSER_MECHANICS_RUN_ID"))
                .map(|value| value.to_str().unwrap().to_owned())
                .unwrap_or_default(),
            name,
            root: self.root.clone(),
            stdout: Some(command.stdout),
            stderr: Some(command.stderr),
            output: Vec::new(),
            stopped: false,
        }))
    }
    async fn cancelled(&self) -> u8 {
        let code = self.cancellation.load(Ordering::SeqCst);
        if code != 0 {
            code
        } else {
            std::future::pending().await
        }
    }
}

#[tokio::test]
async fn completed_mechanics_owns_only_server_and_node_and_closes_before_receipt() {
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    execute(&plan, &fixture.services()).await.unwrap();
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.active, 0);
    assert_eq!(state.stopped, ["node", "server"]);
    assert_eq!(state.calls.len(), 2);
    assert_eq!(state.calls[1].1, [plan.entry.to_str().unwrap()]);
    assert_eq!(state.calls[1].3, NODE_BUDGET);
    assert!(
        !state.calls[0]
            .2
            .contains_key(std::ffi::OsStr::new("DATABASE_URL"))
    );
    assert!(
        !state.calls[1]
            .2
            .contains_key(std::ffi::OsStr::new("NODE_PATH"))
    );
    assert_eq!(
        state.calls[1].2[std::ffi::OsStr::new("IMPAIR_SCRIPT")],
        "none"
    );
    assert_eq!(fixture.evidence()["processCleanupPassed"], true);
    assert_eq!(fixture.evidence()["browserReceiptPassed"], true);
    assert!(!fixture.evidence().to_string().contains("PRIVATE_SENTINEL"));
}

#[tokio::test]
async fn every_acquired_child_retires_and_failure_cannot_publish_success() {
    for fault in [
        Fault::Ports,
        Fault::Ready,
        Fault::ServerSpawn,
        Fault::NodeSpawn,
        Fault::NodeClose,
        Fault::ServerClose,
        Fault::ServerExit,
        Fault::Cancel(130),
        Fault::Cancel(143),
        Fault::Timeout,
        Fault::MissingReceipt,
        Fault::Unknown,
        Fault::NestedUnknown,
        Fault::WrongRun,
        Fault::UnobservedBrowser,
        Fault::ForcedBrowser,
        Fault::MissingNullable,
        Fault::Namespace,
        Fault::Publish,
        Fault::LateCancel,
        Fault::LateDeadline,
        Fault::EntryChanged,
    ] {
        let (fixture, plan) = Fixture::new(fault, 0);
        let error = execute(&plan, &fixture.services()).await.unwrap_err();
        let expected = match fault {
            Fault::Ready | Fault::Timeout | Fault::LateDeadline => 124,
            Fault::Cancel(code) => code,
            Fault::LateCancel => 130,
            Fault::ServerExit => 42,
            _ => 1,
        };
        assert_eq!(code(&error), expected, "{fault:?}");
        assert_eq!(fixture.state.lock().unwrap().active, 0, "{fault:?}");
        assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    }
}

#[tokio::test]
async fn known_failures_survive_later_cleanup_signal_and_elapsed_time() {
    for value in [1, 42, 124, 130, 143] {
        for fault in [
            Fault::None,
            Fault::ServerClose,
            Fault::LateCancel,
            Fault::LateDeadline,
        ] {
            let (fixture, plan) = Fixture::new(fault, value);
            let error = execute(&plan, &fixture.services()).await.unwrap_err();
            assert_eq!(code(&error), value as u8, "{fault:?}");
            assert_eq!(fixture.state.lock().unwrap().active, 0);
        }
    }
    for value in [1, 124, 130, 143] {
        let (fixture, plan) = Fixture::new(Fault::PollError(value), 0);
        let error = execute(&plan, &fixture.services()).await.unwrap_err();
        assert_eq!(code(&error), value);
        assert_eq!(fixture.state.lock().unwrap().active, 0);
    }
    let (fixture, plan) = Fixture::new(Fault::ServerStoppedExit(42), 0);
    let error = execute(&plan, &fixture.services()).await.unwrap_err();
    assert_eq!(code(&error), 42);
    assert_eq!(fixture.state.lock().unwrap().active, 0);
}

#[tokio::test]
async fn full_node_body_and_retirement_reserve_are_admitted_before_dispatch() {
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    fixture.time.store(30_001, Ordering::SeqCst);
    let error = execute(&plan, &fixture.services()).await.unwrap_err();
    assert_eq!(code(&error), 124);
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}

#[test]
fn conflicting_or_ambient_injection_options_refuse_before_acquisition() {
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    for (name, value) in [
        ("IMPAIRED_BLOCK_UDP", "1"),
        ("CANARY_MODE", "1"),
        ("IMPAIRED_ROOM", "foreign"),
        ("CANARY_CORRELATION_FILE", "private"),
        ("IMPAIR_SCRIPT", "/foreign/helper"),
        ("IMPAIRED_PROFILES", "lossy"),
        ("BASE_URL", "http://foreign"),
        ("NODE_OPTIONS", "--import private"),
        ("E2E_BROWSER", "firefox"),
        ("IMPAIR_LOSS", "5"),
    ] {
        let mut env = plan.environment.clone();
        env.insert(name.into(), value.into());
        assert!(
            Plan::parse(
                &fixture.root,
                &["--output".into(), plan.output.clone().into_os_string()],
                env
            )
            .is_err(),
            "{name}"
        );
    }
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}
