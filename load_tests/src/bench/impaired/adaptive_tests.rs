//! Virtual kernel/Node/server owners and actual private artifact files. No
//! command, browser, interface or listener is started by this controller matrix.

use super::super::state::Identity;
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
enum Fault {
    None,
    Foreign,
    Namespace,
    Add(i32),
    Clear(i32),
    Cancel(u8),
    CancelTransition(u8),
    NodeExit(i32),
    NodeExitClose(i32),
    NodeExitThenCancel(i32),
    Reserve,
    CleanupBudget,
    NodeClose,
    ControlClose,
    MissingFinish,
    WrongOrder,
    Duplicate,
    Oversize,
    Extra,
    ExtraAfterFinish,
    Partial,
    Ack,
    Unknown,
    DuplicateKey,
    MissingNullable,
    Noncanonical,
    EarlyFinish,
    LateCancel,
    LateDeadline,
    ServerExit(i32),
}
#[derive(Default)]
struct State {
    kernel: usize,
    profile: String,
    calls: Vec<Vec<String>>,
    phases: Vec<String>,
    sequence: usize,
    pending: Vec<u8>,
    acknowledgments: usize,
    events: Vec<String>,
    node_stopped: bool,
    control_closed: bool,
    active_cli: usize,
    final_cli: usize,
}
struct Fixture {
    root: PathBuf,
    state: Arc<Mutex<State>>,
    clock: Arc<AtomicU64>,
    cancellation: Arc<AtomicU8>,
    fault: Fault,
    native_node: bool,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
fn executable(path: &std::path::Path) {
    fs::write(path, b"owned inert selection").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
impl Fixture {
    fn new(fault: Fault) -> (Self, Plan) {
        let root = std::env::temp_dir().join(format!("adaptive {}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("web/e2e")).unwrap();
        for name in ["server", "node", "tc", "sudo"] {
            executable(&root.join(name));
        }
        fs::write(
            root.join("web/e2e/browser-adaptive.cjs"),
            b"owned inert entry",
        )
        .unwrap();
        let environment = [
            ("ACT".into(), "true".into()),
            ("LOCAL_CI_DISPOSABLE".into(), "1".into()),
            ("PATH".into(), root.clone().into_os_string()),
            (
                "SIMPLESTCHAT_BIN".into(),
                root.join("server").into_os_string(),
            ),
            ("DATABASE_URL".into(), "PRIVATE_SENTINEL".into()),
        ]
        .into();
        let plan = Plan::parse_mode(
            &root,
            &["--output".into(), root.join("output").into_os_string()],
            environment,
            Mode::Adaptive,
            "linux",
        )
        .unwrap();
        (
            Self {
                root,
                state: Arc::default(),
                clock: Arc::default(),
                cancellation: Arc::default(),
                fault,
                native_node: false,
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
            authority: self,
        }
    }
    fn evidence(&self) -> Value {
        serde_json::from_slice(
            &fs::read(self.root.join("output/browser-adaptive-exit.json")).unwrap(),
        )
        .unwrap()
    }
}
#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        self.clock.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        "2026-10-22T00:00:00Z".into()
    }
    async fn sleep(&self, value: Duration) {
        self.clock
            .fetch_add(value.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Ports for Fixture {
    fn available(&self, port: u16, media: u16, workers: u32) -> Result<()> {
        assert_eq!((port, media, workers), (3109, 41100, 1));
        Ok(())
    }
}
#[async_trait::async_trait]
impl controller::Readiness for Fixture {
    async fn ready(&self) -> bool {
        if matches!(self.fault, Fault::Reserve | Fault::CleanupBudget) {
            self.clock.store(
                if self.fault == Fault::Reserve {
                    61000
                } else {
                    60000
                },
                Ordering::SeqCst,
            );
        }
        true
    }
}
impl Authority for Fixture {
    fn observe(&self) -> Result<Identity> {
        Ok(Identity {
            namespace: (1, 2),
            interface: if self.fault == Fault::Namespace && self.state.lock().unwrap().kernel > 0 {
                9
            } else {
                3
            },
        })
    }
}
fn qdisc(state: &State, fault: Fault) -> Vec<u8> {
    if fault == Fault::Foreign {
        return br#"[{"kind":"fq","handle":"7:","root":true}]"#.to_vec();
    }
    if state.kernel == 0 {
        return br#"[{"kind":"noqueue","handle":"0:","root":true}]"#.to_vec();
    }
    let mut values = vec![json!({"kind":"prio","handle":"1:","root":true,
        "options":{"bands":3,"priomap":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}})];
    if state.kernel >= 2 {
        let lossy = state.profile == "lossy" || state.profile == "lossyJoin";
        let options = if lossy {
            json!({"limit":200000,"delay":{"delay":0.05,"jitter":0.01,"correlation":0},
                "loss-random":{"loss":0.05,"correlation":0},"ecn":false,"gap":0})
        } else {
            json!({"limit":200000,"delay":{"delay":0.03,"jitter":0,"correlation":0},
                "rate":{"rate":if state.profile == "constrained" {50000} else {18750},
                    "packetoverhead":0,"cellsize":0,"celloverhead":0},
                "ecn":false,"gap":0})
        };
        values.push(json!({"kind":"netem","handle":"30:","parent":"1:3","options":options}));
    }
    serde_json::to_vec(&values).unwrap()
}
fn filters(state: &State) -> Vec<u8> {
    if state.kernel < 3 {
        return b"[]".to_vec();
    }
    let selector = |preference, value, mask| {
        format!(
            r#"{{"protocol":"ip","pref":{preference},
        "kind":"u32","options":{{"flowid":"1:3",
        "match":{{"value":"110000","mask":"ff0000","offmask":"","off":8}},
        "match":{{"value":"{value}","mask":"{mask}","offmask":"","off":20}}}}}}"#
        )
    };
    let first = selector(1, "a08c0000", "ffff0000");
    if state.profile == "lossyJoin" && state.kernel >= 4 {
        format!("[{first},{}]", selector(2, "a08c", "ffff")).into_bytes()
    } else {
        format!("[{first}]").into_bytes()
    }
}
struct FakeChild {
    state: Arc<Mutex<State>>,
    cancellation: Arc<AtomicU8>,
    clock: Arc<AtomicU64>,
    fault: Fault,
    name: String,
    status: i32,
    captured: Vec<u8>,
    stdout: Option<Sink>,
    stderr: Option<Sink>,
    run_id: String,
    cancel: Option<u8>,
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
            return Ok(None);
        }
        if self.name == "node" {
            if let Fault::NodeExit(code)
            | Fault::NodeExitClose(code)
            | Fault::NodeExitThenCancel(code) = self.fault
            {
                return Ok(Some(Exit {
                    code: Some(code),
                    signal: None,
                }));
            }
            let acknowledged = self.state.lock().unwrap().acknowledgments;
            if self.fault == Fault::CleanupBudget && acknowledged == 6 {
                self.clock.store(660000, Ordering::SeqCst);
                return Ok(Some(Exit {
                    code: Some(42),
                    signal: None,
                }));
            }
            if self.fault == Fault::EarlyFinish && acknowledged == 2 {
                return Ok(Some(Exit {
                    code: Some(42),
                    signal: None,
                }));
            }
            return Ok((acknowledged == 7).then_some(Exit {
                code: Some(0),
                signal: None,
            }));
        }
        if let Some(code) = self.cancel.take() {
            self.cancellation.store(code, Ordering::SeqCst);
            return Err(crate::owned_process::Interrupted(code).into());
        }
        Ok(Some(Exit {
            code: Some(self.status),
            signal: None,
        }))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        for sink in [&mut self.stdout, &mut self.stderr] {
            if let Some(Sink::File(mut file)) = sink.take() {
                file.write_all(b"PRIVATE_SENTINEL owned log\n")?;
                nix::unistd::close(file)?;
            }
        }
        let mut state = self.state.lock().unwrap();
        state.events.push(format!("stop-{}", self.name));
        if matches!(self.name.as_str(), "tc" | "sudo") {
            state.active_cli -= 1;
            if state.node_stopped {
                state.final_cli += 1;
                if self.fault == Fault::CleanupBudget {
                    self.clock.fetch_add(30000, Ordering::SeqCst);
                }
            }
        }
        if self.name == "node" {
            assert_eq!(grace, Duration::from_secs(20));
            state.node_stopped = true;
            if self.fault == Fault::CleanupBudget {
                self.clock.fetch_add(45000, Ordering::SeqCst);
            }
            self.captured = serde_json::to_vec(&super::tests::receipt(&self.run_id))?;
            ensure!(
                !matches!(self.fault, Fault::NodeClose | Fault::NodeExitClose(_)),
                "PRIVATE_SENTINEL"
            );
        }
        if self.name == "server" {
            assert_eq!(grace, Duration::from_secs(5));
            if self.fault == Fault::CleanupBudget {
                self.clock.fetch_add(30000, Ordering::SeqCst);
            }
            if self.fault == Fault::LateCancel || matches!(self.fault, Fault::NodeExitThenCancel(_))
            {
                self.cancellation.store(143, Ordering::SeqCst);
            }
            if self.fault == Fault::LateDeadline {
                self.clock.store(960000, Ordering::SeqCst);
            }
            if let Fault::ServerExit(code) = self.fault {
                return Ok(Exit {
                    code: Some(code),
                    signal: None,
                });
            }
        }
        Ok(Exit {
            code: Some(
                if self.name == "node" && self.fault == Fault::CleanupBudget {
                    42
                } else {
                    self.status
                },
            ),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        &self.captured
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
        assert_ne!(name, "node", "Node must use the controlled provider");
        self.child(command, name)
    }
    fn spawn_controlled(&self, command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        assert_eq!(command.timeout, Duration::from_secs(600));
        assert_eq!(command.stdout_limit, RECEIPT_LIMIT);
        let env = &command.environment;
        assert_eq!(env[std::ffi::OsStr::new("IMPAIR_SCRIPT")], "rust-owned");
        assert_eq!(
            env[std::ffi::OsStr::new("IMPAIRED_PROFILES")],
            "baseline,lossy,constrained,severe,recovery,lossyJoin"
        );
        assert!(!env.contains_key(std::ffi::OsStr::new("DATABASE_URL")));
        if self.native_node {
            let (child, channel) = NativeHost::default().spawn_controlled(command)?;
            return Ok((
                Box::new(TrackedChild {
                    child,
                    state: self.state.clone(),
                }),
                Box::new(TrackedControl {
                    channel,
                    state: self.state.clone(),
                }),
            ));
        }
        let run_id = env[std::ffi::OsStr::new("BROWSER_ADAPTIVE_RUN_ID")]
            .to_str()
            .unwrap()
            .to_owned();
        let control = FakeControl {
            state: self.state.clone(),
            clock: self.clock.clone(),
            fault: self.fault,
            run_id,
        };
        Ok((self.child(command, "node".into())?, Box::new(control)))
    }
    async fn cancelled(&self) -> u8 {
        loop {
            let code = self.cancellation.load(Ordering::SeqCst);
            if code != 0 {
                return code;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }
}
impl Fixture {
    fn child(&self, command: Command, name: String) -> Result<Box<dyn Child>> {
        let mut state = self.state.lock().unwrap();
        let mut captured = Vec::new();
        let mut status = 0;
        let mut cancel = None;
        if matches!(name.as_str(), "tc" | "sudo") {
            state.active_cli += 1;
        }
        if name == "tc" {
            captured = if command.arguments[1] == "qdisc" {
                qdisc(&state, self.fault)
            } else {
                filters(&state)
            };
        }
        if name == "sudo" {
            let args = &command.arguments[3..];
            if !command.cleanup {
                cancel = match self.fault {
                    Fault::Cancel(code) => Some(code),
                    Fault::CancelTransition(code) if state.sequence == 3 => Some(code),
                    _ => None,
                };
            }
            state.calls.push(args.to_vec());
            if args[1] == "del" {
                if state.node_stopped {
                    assert!(state.control_closed);
                }
                state.kernel = 0;
                state.events.push("clear".into());
                if let Fault::Clear(code) = self.fault {
                    status = code;
                }
            } else {
                state.kernel += 1;
                if args.contains(&"netem".into()) {
                    state.profile = if args.contains(&"400kbit".into()) {
                        "constrained"
                    } else if args.contains(&"150kbit".into()) {
                        "severe"
                    } else if state.sequence == 6 {
                        "lossyJoin"
                    } else {
                        "lossy"
                    }
                    .into();
                }
                if args.contains(&"dport".into()) {
                    state.profile = "lossyJoin".into();
                }
                if let Fault::Add(code) = self.fault {
                    status = code;
                }
            }
        }
        let run_id = command
            .environment
            .get(std::ffi::OsStr::new("BROWSER_ADAPTIVE_RUN_ID"))
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_owned();
        Ok(Box::new(FakeChild {
            state: self.state.clone(),
            cancellation: self.cancellation.clone(),
            clock: self.clock.clone(),
            fault: self.fault,
            name,
            status,
            captured,
            stdout: Some(command.stdout),
            stderr: Some(command.stderr),
            run_id,
            cancel,
        }))
    }
}
struct FakeControl {
    state: Arc<Mutex<State>>,
    clock: Arc<AtomicU64>,
    fault: Fault,
    run_id: String,
}
#[async_trait::async_trait]
impl Control for FakeControl {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        let mut state = self.state.lock().unwrap();
        if state.pending.is_empty() {
            if self.fault == Fault::MissingFinish && state.acknowledgments == 6 {
                self.clock.store(700000, Ordering::SeqCst);
                return Ok(0);
            }
            if state.sequence == 7 {
                if self.fault == Fault::ExtraAfterFinish {
                    buffer[0] = b'x';
                    return Ok(1);
                }
                return Ok(0);
            }
            state.sequence += 1;
            let sequence = state.sequence;
            let phase = if sequence == 7 || (sequence == 2 && self.fault == Fault::EarlyFinish) {
                "finish"
            } else {
                adaptive::PHASES[sequence - 1]
            };
            state.phases.push(phase.into());
            let clear = matches!(phase, "baseline" | "recovery" | "finish");
            let mut request = format!(
                "{{\"schemaVersion\":1,\"runId\":\"{}\",\"sequence\":{sequence},\"phase\":\"{phase}\",\"action\":\"{}\",\"profile\":{}}}\n",
                self.run_id,
                if clear { "clear" } else { "apply" },
                if clear {
                    "null".into()
                } else {
                    format!("\"{phase}\"")
                }
            );
            if sequence == 2 {
                match self.fault {
                    Fault::WrongOrder => request = request.replace("lossy", "severe"),
                    Fault::Duplicate => {
                        request = request.replace("\"sequence\":2", "\"sequence\":1")
                    }
                    Fault::Oversize => request = "x".repeat(1025),
                    Fault::Extra => request.push_str("{}\n"),
                    Fault::Unknown => {
                        request = request.replace(
                            "\"schemaVersion\":1,",
                            "\"schemaVersion\":1,\"private\":\"PRIVATE_SENTINEL\",",
                        )
                    }
                    Fault::DuplicateKey => {
                        request =
                            request.replace("\"sequence\":2,", "\"sequence\":2,\"sequence\":2,")
                    }
                    Fault::MissingNullable => {
                        request = request.replace(",\"profile\":\"lossy\"", "")
                    }
                    Fault::Noncanonical => request = request.replace("{", "{ "),
                    _ => {}
                }
            }
            state.pending = request.into_bytes();
        }
        let count = buffer
            .len()
            .min(state.pending.len())
            .min(if self.fault == Fault::Partial {
                1
            } else {
                1025
            });
        buffer[..count].copy_from_slice(&state.pending[..count]);
        state.pending.drain(..count);
        if self.fault == Fault::Partial {
            self.clock.fetch_add(1001, Ordering::SeqCst);
        }
        Ok(count)
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        ensure!(self.fault != Fault::Ack, "PRIVATE_SENTINEL");
        let value: Value = serde_json::from_slice(bytes)?;
        let mut state = self.state.lock().unwrap();
        assert_eq!(value["sequence"], state.sequence);
        assert_eq!(
            state.active_cli, 0,
            "No acknowledgment before every CLI group retires"
        );
        if value["passed"] == true {
            let phase = state.phases.last().unwrap().as_str();
            if matches!(phase, "baseline" | "recovery" | "finish") {
                assert_eq!(state.kernel, 0);
            } else {
                assert_eq!(state.profile, phase);
                assert_eq!(state.kernel, if phase == "lossyJoin" { 4 } else { 3 });
            }
            state.acknowledgments += 1;
        }
        let sequence = state.sequence;
        state.events.push(format!("ack-{sequence}"));
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        assert!(state.node_stopped, "Channel must outlive Node retirement");
        state.control_closed = true;
        state.events.push("control-close".into());
        ensure!(self.fault != Fault::ControlClose, "PRIVATE_SENTINEL");
        Ok(())
    }
}

#[tokio::test]
async fn all_six_phases_share_one_lease_and_stop_before_final_clear_and_receipt() {
    let (fixture, plan) = Fixture::new(Fault::None);
    let result = execute(&plan, &fixture.services()).await;
    let debug = {
        let state = fixture.state.lock().unwrap();
        format!("calls={:?}; phases={:?}", state.calls, state.phases)
    };
    assert!(
        result.is_ok(),
        "{result:?}; evidence={}; {debug}",
        fixture.evidence()
    );
    let state = fixture.state.lock().unwrap();
    assert_eq!(
        state.phases,
        [
            "baseline",
            "lossy",
            "constrained",
            "severe",
            "recovery",
            "lossyJoin",
            "finish"
        ]
    );
    assert_eq!(state.kernel, 0);
    let node = state
        .events
        .iter()
        .position(|value| value == "stop-node")
        .unwrap();
    let channel = state
        .events
        .iter()
        .position(|value| value == "control-close")
        .unwrap();
    let server = state
        .events
        .iter()
        .position(|value| value == "stop-server")
        .unwrap();
    assert!(node < channel && channel < server);
    assert!(
        state
            .calls
            .iter()
            .all(|args| !args.iter().any(|arg| arg == "replace"))
    );
    let evidence = fixture.evidence();
    for field in [
        "processCleanupPassed",
        "browserReceiptPassed",
        "controlSequencePassed",
        "kernelCleanupPassed",
    ] {
        assert_eq!(evidence[field], true, "{field}");
    }
    assert!(!evidence.to_string().contains("PRIVATE_SENTINEL"));
}

#[tokio::test]
async fn protocol_partial_mutation_and_finalization_failures_never_pass() {
    for fault in [
        Fault::Foreign,
        Fault::Namespace,
        Fault::Add(42),
        Fault::Clear(42),
        Fault::Cancel(130),
        Fault::Cancel(143),
        Fault::CancelTransition(130),
        Fault::CancelTransition(143),
        Fault::NodeExit(42),
        Fault::NodeExitClose(42),
        Fault::NodeExitThenCancel(42),
        Fault::Reserve,
        Fault::NodeClose,
        Fault::ControlClose,
        Fault::MissingFinish,
        Fault::WrongOrder,
        Fault::Duplicate,
        Fault::Oversize,
        Fault::Extra,
        Fault::ExtraAfterFinish,
        Fault::Partial,
        Fault::Ack,
        Fault::Unknown,
        Fault::DuplicateKey,
        Fault::MissingNullable,
        Fault::Noncanonical,
        Fault::EarlyFinish,
        Fault::LateCancel,
        Fault::LateDeadline,
        Fault::ServerExit(42),
    ] {
        let (fixture, plan) = Fixture::new(fault);
        let error = execute(&plan, &fixture.services()).await.unwrap_err();
        assert!(!error.to_string().contains("PRIVATE_SENTINEL"), "{fault:?}");
        if matches!(
            fault,
            Fault::Add(42)
                | Fault::Clear(42)
                | Fault::NodeExit(42)
                | Fault::NodeExitClose(42)
                | Fault::NodeExitThenCancel(42)
                | Fault::ServerExit(42)
                | Fault::EarlyFinish
        ) {
            assert_eq!(code(&error), 42, "{fault:?}");
        }
        if let Fault::Cancel(expected) | Fault::CancelTransition(expected) = fault {
            assert_eq!(code(&error), expected, "{fault:?}");
        }
        if matches!(fault, Fault::Partial | Fault::LateDeadline | Fault::Reserve) {
            assert_eq!(code(&error), 124, "{fault:?}");
        }
        let state = fixture.state.lock().unwrap();
        if state.events.iter().any(|value| value == "stop-node") {
            assert!(state.control_closed);
        }
        if fault == Fault::Foreign {
            assert!(state.calls.is_empty());
        }
        if matches!(fault, Fault::Cancel(_) | Fault::CancelTransition(_)) {
            assert_eq!(
                state.kernel, 0,
                "Exact partial active profile must be cleared"
            );
            assert_eq!(state.active_cli, 0);
            assert!(
                !fixture.evidence()["processCleanupPassed"]
                    .as_bool()
                    .unwrap()
            );
        }
        if !matches!(fault, Fault::LateCancel | Fault::LateDeadline)
            && let Ok(bytes) = fs::read(fixture.root.join("output/browser-adaptive-exit.json"))
        {
            let value: Value = serde_json::from_slice(&bytes).unwrap();
            assert_ne!(value["statusBeforeFinalFence"], 0, "{fault:?}");
        }
    }
}

struct TrackedChild {
    child: Box<dyn Child>,
    state: Arc<Mutex<State>>,
}
#[async_trait::async_trait]
impl Child for TrackedChild {
    fn pid(&self) -> u32 {
        self.child.pid()
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        self.child.take_stdout()
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        self.child.poll().await
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        let result = self.child.stop(grace).await;
        let mut state = self.state.lock().unwrap();
        state.node_stopped = true;
        state.events.push("stop-node".into());
        result
    }
    fn output(&self) -> &[u8] {
        self.child.output()
    }
    fn errors(&self) -> &[u8] {
        self.child.errors()
    }
}
struct TrackedControl {
    channel: Box<dyn Control>,
    state: Arc<Mutex<State>>,
}
#[async_trait::async_trait]
impl Control for TrackedControl {
    async fn read(&mut self, buffer: &mut [u8]) -> Result<usize> {
        self.channel.read(buffer).await
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.channel.write(bytes).await
    }
    fn finish(self: Box<Self>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        assert!(state.node_stopped);
        state.control_closed = true;
        state.events.push("control-close".into());
        self.channel.finish()
    }
}

#[tokio::test]
async fn actual_node_channel_and_rust_lease_exchange_all_frames_on_inherited_stdin() {
    let (mut fixture, original) = Fixture::new(Fault::None);
    fixture.native_node = true;
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let module =
        serde_json::to_string(&repository.join("web/e2e/browser-impairment-channel.cjs")).unwrap();
    let receipt = super::tests::receipt("RUN_ID");
    let entry = format!(
        r#"
const {{createChannel}}=require({module});
const runId=process.env.BROWSER_ADAPTIVE_RUN_ID;
const channel=createChannel({{runId}});
(async()=>{{
await channel.impair('clear',{{}},'baseline');
await channel.impair('apply',{{IMPAIR_LOSS:'5',IMPAIR_DELAY_MS:'50',IMPAIR_JITTER_MS:'10'}},'lossy');
await channel.impair('apply',{{IMPAIR_RATE_KBIT:'400',IMPAIR_DELAY_MS:'30'}},'constrained');
await channel.impair('apply',{{IMPAIR_RATE_KBIT:'150',IMPAIR_DELAY_MS:'30'}},'severe');
await channel.impair('clear',{{}},'recovery');
await channel.impair('apply',{{IMPAIR_LOSS:'5',IMPAIR_DELAY_MS:'50',IMPAIR_JITTER_MS:'10',
IMPAIR_DIRECTION:'both'}},'lossyJoin');
await channel.impair('clear');await channel.finish();
const receipt={receipt};receipt.runId=runId;process.stdout.write(JSON.stringify(receipt));
}})().catch(()=>{{process.stderr.write('inert channel failed');process.exitCode=1;}});
"#
    );
    fs::write(&original.entry, entry).unwrap();
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("node"))
        .find(|path| path.is_file())
        .unwrap();
    let plan = Plan::parse_mode(
        &fixture.root,
        &[
            "--output".into(),
            original.output.into_os_string(),
            "--node".into(),
            node.into_os_string(),
        ],
        original.environment,
        Mode::Adaptive,
        "linux",
    )
    .unwrap();
    // RealClock is necessary for the actual owned Node event loop; all kernel,
    // port and readiness boundaries remain virtual and no server is executed.
    let clock = RealClock::default();
    let services = Services {
        clock: &clock,
        ..fixture.services()
    };
    execute(&plan, &services).await.unwrap();
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.kernel, 0);
    assert!(state.node_stopped && state.control_closed);
    assert_eq!(fixture.evidence()["controlSequencePassed"], true);
}

#[test]
fn adaptive_admission_refuses_foreign_modes_and_requires_disposable_linux_before_execution() {
    let (fixture, plan) = Fixture::new(Fault::None);
    let arguments = ["--output".into(), plan.output.clone().into_os_string()];
    for (key, value) in [
        ("LOCAL_CI_DISPOSABLE", "0"),
        ("NODE_OPTIONS", "--require=PRIVATE_SENTINEL"),
        ("IMPAIR_SCRIPT", "none"),
        ("IMPAIRED_BLOCK_UDP", "1"),
        ("IMPAIRED_PROFILES", "baseline"),
        ("IMPAIRED_ROOM", "PRIVATE_SENTINEL"),
        ("IMPAIR_LOSS", "6"),
        ("E2E_BROWSER", "firefox"),
        ("IMPAIRED_SILENT_AUDIO", "1"),
        ("BASE_URL", "https://foreign.invalid"),
    ] {
        let mut environment = plan.environment.clone();
        environment.insert(key.into(), value.into());
        assert!(
            Plan::parse_mode(
                &fixture.root,
                &arguments,
                environment,
                Mode::Adaptive,
                "linux"
            )
            .is_err(),
            "{key}"
        );
    }
    assert!(
        Plan::parse_mode(
            &fixture.root,
            &arguments,
            plan.environment,
            Mode::Adaptive,
            "macos"
        )
        .is_err()
    );
    assert!(!plan.output.exists());
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn full_cleanup_schedule_retains_primary_and_admits_all_seven_reserved_kernel_calls() {
    let (fixture, plan) = Fixture::new(Fault::CleanupBudget);
    let error = execute(&plan, &fixture.services()).await.unwrap_err();
    assert_eq!(code(&error), 42);
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.final_cli, 7);
    assert_eq!(state.kernel, 0);
    assert_eq!(fixture.clock.load(Ordering::SeqCst), 945000);
    assert!(state.node_stopped && state.control_closed);
    assert_eq!(fixture.evidence()["kernelCleanupPassed"], true);
    assert_eq!(fixture.evidence()["processCleanupPassed"], true);
}
