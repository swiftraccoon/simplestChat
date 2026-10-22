//! Virtual commands/kernel and private files; no listener or kernel is touched.
use super::super::state::Identity;
use super::*;
use crate::bench::process::Stream;
use serde_json::Value;
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    Add(u8, i32),
    Lost(u8),
    Remove(u8),
    Foreign,
    Baseline,
    Namespace,
    Version,
    Cancel(u8),
    CancelAdmission(u8),
    Node(i32),
    NodeClose,
    Ack,
    Extra,
    Early,
    Late,
    Reserve,
    Intent,
    Poll(u8),
    Settlement,
    WrongOrder,
    MissingUnblock,
}
#[derive(Default)]
struct State {
    stage: u8,
    calls: Vec<Vec<String>>,
    active: usize,
    sequence: u8,
    stopped: Vec<String>,
    acknowledgments: usize,
    node_stopped: bool,
    control_closed: bool,
    fault_used: bool,
    final_cli: usize,
}
pub(super) struct Fixture {
    pub(super) root: PathBuf,
    state: Arc<Mutex<State>>,
    clock: Arc<AtomicU64>,
    cancellation: Arc<AtomicU8>,
    fault: Fault,
    native_node: bool,
    rules: udp::policy::Rules,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Fixture {
    pub(super) fn new(fault: Fault) -> (Self, Plan) {
        let root = std::env::temp_dir().join(format!("tcp lease {}", uuid::Uuid::new_v4()));
        fs::create_dir_all(root.join("web/e2e")).unwrap();
        for name in ["server", "node", "sudo", "tc", "iptables"] {
            fs::write(root.join(name), b"owned inert selection").unwrap();
            fs::set_permissions(root.join(name), fs::Permissions::from_mode(0o700)).unwrap();
        }
        fs::write(root.join("web/e2e/browser-tcp.cjs"), b"owned inert entry").unwrap();
        let plan = Plan::parse_mode(
            &root,
            &["--output".into(), root.join("output").into_os_string()],
            [
                ("ACT".into(), "true".into()),
                ("LOCAL_CI_DISPOSABLE".into(), "1".into()),
                ("PATH".into(), root.clone().into_os_string()),
                (
                    "SIMPLESTCHAT_BIN".into(),
                    root.join("server").into_os_string(),
                ),
                ("DATABASE_URL".into(), "PRIVATE_SENTINEL".into()),
            ]
            .into(),
            Mode::Tcp,
            "linux",
        )
        .unwrap();
        let rules = udp::policy::Rules::new(&plan.run_id).unwrap();
        (
            Self {
                root,
                state: Arc::default(),
                clock: Arc::default(),
                cancellation: Arc::default(),
                fault,
                native_node: false,
                rules,
            },
            plan,
        )
    }
    pub(super) fn services(&self) -> Services<'_> {
        Services {
            host: self,
            clock: self,
            ports: self,
            readiness: self,
            authority: self,
        }
    }
    fn listing(&self, state: &State) -> Vec<u8> {
        let mut text = "-P INPUT ACCEPT\n-P FORWARD ACCEPT\n-P OUTPUT ACCEPT\n".to_owned();
        if state.stage > 0 {
            text += &format!("-N {}\n", self.rules.chain);
        }
        if state.stage == 4 {
            text += &format!("-A INPUT {}\n", self.rules.rendering(4));
        }
        text += "-A INPUT -m comment --comment \"PRIVATE_SENTINEL unrelated\" -j ACCEPT\n";
        for index in 2..=state.stage.min(3) {
            text += &format!("-A {} {}\n", self.rules.chain, self.rules.rendering(index));
        }
        if self.fault == Fault::Foreign && state.stage > 0 {
            text += &format!("-A {} -j ACCEPT\n", self.rules.chain);
        }
        if self.fault == Fault::Baseline && state.stage > 0 {
            text += "-A OUTPUT -j DROP\n";
        }
        text.into_bytes()
    }
    pub(super) fn set_time(&self, milliseconds: u64) {
        self.clock.store(milliseconds, Ordering::SeqCst);
    }
    pub(super) fn call_count(&self) -> usize {
        self.state.lock().unwrap().calls.len()
    }
    fn evidence(&self) -> Value {
        serde_json::from_slice(&fs::read(self.root.join("output/browser-tcp-exit.json")).unwrap())
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
    async fn sleep(&self, delay: Duration) {
        self.clock
            .fetch_add(delay.as_millis() as u64, Ordering::SeqCst);
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
        if self.fault == Fault::Reserve {
            self.clock.store(150_001, Ordering::SeqCst);
        }
        true
    }
}
impl Authority for Fixture {
    fn observe(&self) -> Result<Identity> {
        Ok(Identity {
            namespace: (1, 2),
            interface: if self.fault == Fault::Namespace && self.state.lock().unwrap().stage > 0 {
                9
            } else {
                3
            },
        })
    }
}
struct VirtualChild {
    state: Arc<Mutex<State>>,
    cancellation: Arc<AtomicU8>,
    clock: Arc<AtomicU64>,
    name: String,
    fault: Fault,
    exit: i32,
    output: Vec<u8>,
    poll_failure: Option<u8>,
    stdout: Option<Sink>,
    stderr: Option<Sink>,
    run_id: String,
}
#[async_trait::async_trait]
impl Child for VirtualChild {
    fn pid(&self) -> u32 {
        12345
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        None
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        if let Some(code) = self.poll_failure.take() {
            return match code {
                124 => Err(crate::owned_process::child::CommandTimedOut.into()),
                130 | 143 => Err(crate::owned_process::Interrupted(code).into()),
                _ => Err(anyhow::anyhow!("PRIVATE_SENTINEL")),
            };
        }
        if self.name == "server" {
            return Ok(None);
        }
        if self.name == "node" {
            if self.fault == Fault::Settlement && self.state.lock().unwrap().acknowledgments == 1 {
                self.clock.store(750_000, Ordering::SeqCst);
                return Ok(Some(Exit {
                    code: Some(42),
                    signal: None,
                }));
            }
            if let Fault::Node(code) = self.fault {
                return Ok(Some(Exit {
                    code: Some(code),
                    signal: None,
                }));
            }
            if self.state.lock().unwrap().acknowledgments
                < if self.fault == Fault::Early {
                    2
                } else if self.fault == Fault::MissingUnblock {
                    3
                } else {
                    4
                }
            {
                return Ok(None);
            }
        }
        Ok(Some(Exit {
            code: Some(self.exit),
            signal: None,
        }))
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        assert_eq!(grace.as_secs(), if self.name == "node" { 20 } else { 5 });
        for sink in [&mut self.stdout, &mut self.stderr] {
            if let Some(Sink::File(mut file)) = sink.take() {
                file.write_all(b"PRIVATE_SENTINEL stopped log\n")?;
                nix::unistd::close(file)?;
            }
        }
        let mut state = self.state.lock().unwrap();
        state.active -= 1;
        state.stopped.push(self.name.clone());
        if self.fault == Fault::Settlement && matches!(self.name.as_str(), "node" | "server") {
            self.clock.fetch_add(
                if self.name == "node" { 45_000 } else { 30_000 },
                Ordering::SeqCst,
            );
        }
        if self.name == "node" {
            state.node_stopped = true;
            self.output = serde_json::to_vec(&super::tests::receipt(&self.run_id))?;
            ensure!(self.fault != Fault::NodeClose, "PRIVATE_SENTINEL");
        }
        if self.name == "server" && self.fault == Fault::Late {
            self.cancellation.store(143, Ordering::SeqCst);
            self.clock.store(1_300_000, Ordering::SeqCst);
        }
        Ok(Exit {
            code: Some(self.exit),
            signal: None,
        })
    }
    fn output(&self) -> &[u8] {
        &self.output
    }
    fn errors(&self) -> &[u8] {
        &[]
    }
}
struct Channel {
    cancellation: Arc<AtomicU8>,
    state: Arc<Mutex<State>>,
    run_id: String,
    fault: Fault,
}
#[async_trait::async_trait]
impl Control for Channel {
    async fn read(&mut self, bytes: &mut [u8]) -> Result<usize> {
        let pending = {
            let state = self.state.lock().unwrap();
            !state.node_stopped
                && (state.sequence != state.acknowledgments as u8 || state.sequence >= 4)
        };
        if pending {
            return std::future::pending().await;
        }
        let mut state = self.state.lock().unwrap();
        if state.node_stopped {
            if self.fault == Fault::Extra {
                bytes[0] = b'x';
                return Ok(1);
            }
            return Ok(0);
        }
        let phases = if self.fault == Fault::WrongOrder {
            ["baseline", "block", "finish", "unblock"]
        } else if self.fault == Fault::Early {
            ["finish", "unblock", "finish", "unblock"]
        } else {
            ["block", "baseline", "finish", "unblock"]
        };
        let phase = phases[usize::from(state.sequence)];
        let action = match phase {
            "block" => "block-udp",
            "unblock" => "unblock-udp",
            _ => "clear",
        };
        state.sequence += 1;
        let frame = format!(
            concat!(
                "{{\"schemaVersion\":1,\"runId\":\"{}\",\"sequence\":{},",
                "\"phase\":\"{phase}\",\"action\":\"{action}\",\"profile\":null}}\n"
            ),
            self.run_id,
            state.sequence,
            phase = phase,
            action = action
        );
        if let Fault::CancelAdmission(code) = self.fault {
            self.cancellation.store(code, Ordering::SeqCst);
        }
        bytes[..frame.len()].copy_from_slice(frame.as_bytes());
        Ok(frame.len())
    }
    async fn write(&mut self, bytes: &[u8]) -> Result<()> {
        let reply: Value = serde_json::from_slice(bytes)?;
        let mut state = self.state.lock().unwrap();
        assert_eq!(state.active, 2, "every CLI retired before reply");
        if let Fault::CancelAdmission(code) = self.fault {
            assert_eq!(reply["status"], code);
            assert_eq!(reply["passed"], false);
        }
        if reply["passed"] == true {
            if matches!(state.sequence, 1..=3) && self.fault != Fault::Early {
                assert_eq!(state.stage, 4, "block is held through both clears");
            } else {
                assert_eq!(state.stage, 0);
            }
        }
        state.acknowledgments += 1;
        ensure!(self.fault != Fault::Ack, "PRIVATE_SENTINEL");
        Ok(())
    }
    fn finish(self: Box<Self>) -> Result<()> {
        let mut state = self.state.lock().unwrap();
        assert!(state.node_stopped);
        state.control_closed = true;
        Ok(())
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
        let mut exit = 0;
        let mut output = Vec::new();
        let mut state = self.state.lock().unwrap();
        state.calls.push(command.arguments.clone());
        if self.fault == Fault::Settlement && command.cleanup && state.node_stopped {
            state.final_cli += 1;
            self.clock.fetch_add(30_000, Ordering::SeqCst);
        }
        if name == "sudo" {
            assert_eq!(&command.arguments[..2], ["-n", "--"]);
            assert_eq!(command.timeout, Duration::from_secs(5));
            let args = &command.arguments[3..];
            if args == ["--version"] {
                output = if self.fault == Fault::Version {
                    b"iptables v1.8.13 (nf_tables)\n"
                } else {
                    b"iptables v1.8.10 (nf_tables)\n"
                }
                .to_vec();
            } else {
                assert_eq!(&args[..4], ["-w", "2", "-t", "filter"]);
                match args[4].as_str() {
                    "-S" => output = self.listing(&state),
                    "-N" | "-A" | "-I" => {
                        assert!(self.root.join("output/browser-tcp-intent.json").is_file());
                        let next = state.stage + 1;
                        assert_eq!(&args[4..], self.rules.mutation(next, false));
                        state.stage = next;
                        match self.fault {
                            Fault::Add(index, code) if index == next => exit = code,
                            Fault::Lost(index) if index == next => {
                                exit = 42;
                            }
                            Fault::Cancel(code) if !state.fault_used => {
                                self.cancellation.store(code, Ordering::SeqCst);
                                state.fault_used = true;
                            }
                            _ => {}
                        }
                    }
                    "-D" | "-X" => {
                        assert_eq!(&args[4..], self.rules.mutation(state.stage, true));
                        if self.fault == Fault::Remove(state.stage) {
                            exit = 42;
                        } else {
                            state.stage -= 1;
                        }
                    }
                    _ => panic!("unexpected fixed command"),
                }
            }
        } else if name == "tc" {
            assert!(matches!(command.arguments[1].as_str(), "qdisc" | "filter"));
            output = if command.arguments[1] == "qdisc" {
                br#"[{"kind":"noqueue","handle":"0:","root":true}]"#.to_vec()
            } else {
                b"[]".to_vec()
            };
        } else {
            assert!(matches!(name.as_str(), "node" | "server"));
            assert!(
                !command
                    .environment
                    .contains_key(std::ffi::OsStr::new("DATABASE_URL"))
            );
            assert_eq!(
                command.environment[std::ffi::OsStr::new("WEBRTC_SERVER_TCP")],
                "true"
            );
            if name == "node" {
                assert_eq!(
                    command.environment[std::ffi::OsStr::new("IMPAIRED_BLOCK_UDP")],
                    "1"
                );
                assert_eq!(
                    command.environment[std::ffi::OsStr::new("IMPAIRED_PROFILES")],
                    "baseline"
                );
            }
        }
        let run_id = command
            .environment
            .get(std::ffi::OsStr::new("BROWSER_TCP_RUN_ID"))
            .map(|value| value.to_str().unwrap().to_owned())
            .unwrap_or_default();
        let poll_failure = if let Fault::Poll(code) = self.fault {
            if name == "sudo" && !state.fault_used && state.stage > 0 {
                state.fault_used = true;
                Some(code)
            } else {
                None
            }
        } else {
            None
        };
        state.active += 1;
        Ok(Box::new(VirtualChild {
            state: self.state.clone(),
            cancellation: self.cancellation.clone(),
            clock: self.clock.clone(),
            name,
            fault: self.fault,
            exit,
            output,
            poll_failure,
            stdout: Some(command.stdout),
            stderr: Some(command.stderr),
            run_id,
        }))
    }
    fn spawn_controlled(&self, command: Command) -> Result<(Box<dyn Child>, Box<dyn Control>)> {
        let run_id = command.environment[std::ffi::OsStr::new("BROWSER_TCP_RUN_ID")]
            .to_str()
            .unwrap()
            .to_owned();
        if self.native_node {
            let (child, channel) = NativeHost::default().spawn_controlled(command)?;
            self.state.lock().unwrap().active += 1;
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
        if self.fault == Fault::Intent {
            fs::write(self.root.join("output/browser-tcp-intent.json"), b"foreign").unwrap();
        }
        Ok((
            self.spawn(command)?,
            Box::new(Channel {
                cancellation: self.cancellation.clone(),
                state: self.state.clone(),
                run_id,
                fault: self.fault,
            }),
        ))
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
async fn tcp_lease_retains_block_through_clear_and_retires_every_owner_before_success() {
    let (fixture, plan) = Fixture::new(Fault::None);
    execute(&plan, &fixture.services()).await.unwrap();
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.stage, 0);
    assert_eq!(state.active, 0);
    assert!(state.control_closed);
    assert_eq!(state.acknowledgments, 4);
    let value = fixture.evidence();
    assert_eq!(value["kernelCleanupPassed"], true);
    assert_eq!(value["controlSequencePassed"], true);
    assert!(!value.to_string().contains("PRIVATE_SENTINEL"));
    assert_eq!(plan.mode.budget().as_secs(), 1200);
    assert_eq!(plan.mode.reserve().as_secs(), 450);
}
#[tokio::test]
async fn partial_mutations_and_lost_responses_are_scoped_and_never_publish_success() {
    for stage in 1..=4 {
        for fault in [
            Fault::Add(stage, 42),
            Fault::Lost(stage),
            Fault::Remove(stage),
        ] {
            let (fixture, plan) = Fixture::new(fault);
            let error = execute(&plan, &fixture.services()).await.unwrap_err();
            assert_eq!(code(&error), 42, "{fault:?}");
            assert_eq!(fixture.state.lock().unwrap().active, 0);
            if !matches!(fault, Fault::Remove(_)) {
                assert_eq!(fixture.state.lock().unwrap().stage, 0, "{fault:?}");
            }
            assert_eq!(fixture.evidence()["browserReceiptPassed"], true);
        }
    }
}
#[tokio::test]
async fn foreign_changes_invalid_receipts_and_cancellation_cannot_be_success() {
    for fault in [
        Fault::Foreign,
        Fault::Baseline,
        Fault::Namespace,
        Fault::Version,
        Fault::NodeClose,
        Fault::Ack,
        Fault::Extra,
        Fault::Early,
        Fault::WrongOrder,
        Fault::MissingUnblock,
        Fault::Late,
        Fault::Reserve,
        Fault::Intent,
        Fault::Cancel(130),
        Fault::Cancel(143),
        Fault::CancelAdmission(130),
        Fault::CancelAdmission(143),
    ] {
        let (fixture, plan) = Fixture::new(fault);
        let error = execute(&plan, &fixture.services()).await.unwrap_err();
        let expected = match fault {
            Fault::Cancel(code) | Fault::CancelAdmission(code) => code,
            Fault::Late => 143,
            Fault::Reserve => 124,
            _ => 1,
        };
        assert_eq!(code(&error), expected, "{fault:?}");
        assert_eq!(fixture.state.lock().unwrap().active, 0, "{fault:?}");
        if matches!(fault, Fault::Foreign | Fault::Baseline | Fault::Namespace) {
            assert!(
                fixture.state.lock().unwrap().stage > 0,
                "foreign state must not be removed"
            );
        }
        assert!(!error.to_string().contains("PRIVATE_SENTINEL"));
    }
}
#[tokio::test]
async fn observed_primary_failures_survive_later_retirement() {
    for status in [1, 42, 124, 130, 143] {
        let (fixture, plan) = Fixture::new(Fault::Node(status));
        assert_eq!(
            code(&execute(&plan, &fixture.services()).await.unwrap_err()),
            status as u8
        );
        assert_eq!(fixture.state.lock().unwrap().active, 0);
    }
}
#[test]
fn authority_and_fixed_tcp_options_refuse_before_any_command() {
    let (fixture, plan) = Fixture::new(Fault::None);
    for (key, value) in [
        ("WEBRTC_SERVER_TCP", "false"),
        ("IMPAIRED_BLOCK_UDP", "0"),
        ("IMPAIRED_PROFILES", "lossy"),
        ("IMPAIRED_SILENT_AUDIO", "1"),
        ("NODE_OPTIONS", "--eval x"),
    ] {
        let mut environment = plan.environment.clone();
        environment.insert(key.into(), value.into());
        assert!(
            Plan::parse_mode(
                &fixture.root,
                &["--output".into(), plan.output.clone().into_os_string()],
                environment,
                Mode::Tcp,
                "linux"
            )
            .is_err()
        );
    }
    assert!(
        Plan::parse_mode(
            &fixture.root,
            &["--output".into(), plan.output.clone().into_os_string()],
            plan.environment.clone(),
            Mode::Tcp,
            "macos"
        )
        .is_err()
    );
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn known_cli_exits_and_typed_timeouts_survive_every_cleanup_attempt() {
    for status in [1, 42, 124, 130, 143] {
        for fault in [Fault::Add(1, status), Fault::Poll(status as u8)] {
            let (fixture, plan) = Fixture::new(fault);
            let expected = if matches!(fault, Fault::Poll(42)) {
                1
            } else {
                status as u8
            };
            assert_eq!(
                code(&execute(&plan, &fixture.services()).await.unwrap_err()),
                expected
            );
            assert_eq!(fixture.state.lock().unwrap().active, 0);
            assert_eq!(fixture.state.lock().unwrap().stage, 0);
            if matches!(fault, Fault::Poll(_)) {
                assert_eq!(fixture.evidence()["processCleanupPassed"], false);
            }
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
        state.active -= 1;
        state.stopped.push("node".into());
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
        {
            let mut state = self.state.lock().unwrap();
            let reply: Value = serde_json::from_slice(bytes)?;
            assert_eq!(
                state.active, 2,
                "all mutation/readback CLI groups retired before ACK"
            );
            assert_eq!(reply["passed"], true);
            assert_eq!(state.stage, if reply["sequence"] == 4 { 0 } else { 4 });
            state.acknowledgments += 1;
        }
        self.channel.write(bytes).await
    }
    fn finish(self: Box<Self>) -> Result<()> {
        {
            let mut state = self.state.lock().unwrap();
            assert!(state.node_stopped);
            state.control_closed = true;
        }
        self.channel.finish()
    }
}
#[tokio::test]
async fn actual_tcp_facade_uses_fd0_four_step_channel_with_inert_browser_owner() {
    let (mut fixture, original) = Fixture::new(Fault::None);
    fixture.native_node = true;
    let repository = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap();
    let facade = serde_json::to_string(&repository.join("web/e2e/browser-tcp.cjs")).unwrap();
    let record = super::tests::receipt("RUN_ID")["records"][0].clone();
    let entry = format!(
        r#"
const {{run}}=require({facade});
const record={record};
run({{
 env:process.env,playwright:{{}},
 createOwner({{persist}}){{return{{
  launchBrowser:async()=>({{}}),
  closeBrowser:async()=>persist([{{browserName:'chromium',launchMode:'browser-server',
   browserCleanup:record}}]),
  closeAll:async()=>persist([{{browserName:'chromium',launchMode:'browser-server',
   browserCleanup:record}}])
 }}}},
 async suite(options){{
  await options.impair('block-udp');
  const browser=await options.launchBrowser('chromium',{{}});
  try{{await options.impair('clear',{{}},'baseline');}}
  finally{{await options.impair('clear');await options.impair('unblock-udp');
    await options.closeBrowser(browser);}}
 }}
}}).then(result=>{{process.stdout.write(JSON.stringify(result.receipt));
 process.exitCode=result.status;}},()=>{{process.exitCode=1;}});
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
        Mode::Tcp,
        "linux",
    )
    .unwrap();
    fixture.rules = udp::policy::Rules::new(&plan.run_id).unwrap();
    let clock = RealClock::default();
    let services = Services {
        clock: &clock,
        ..fixture.services()
    };
    execute(&plan, &services).await.unwrap();
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.acknowledgments, 4);
    assert!(state.node_stopped && state.control_closed);
    assert_eq!(state.stage, 0);
    assert_eq!(state.active, 0);
    assert_eq!(fixture.evidence()["browserReceiptPassed"], true);
}

#[tokio::test]
async fn full_live_block_cleanup_keeps_twelve_cli_slots_and_both_retirement_allowances() {
    let (fixture, plan) = Fixture::new(Fault::Settlement);
    let error = execute(&plan, &fixture.services()).await.unwrap_err();
    assert_eq!(code(&error), 42);
    let state = fixture.state.lock().unwrap();
    assert_eq!(state.final_cli, 12);
    assert_eq!(state.stage, 0);
    assert_eq!(state.active, 0);
    assert_eq!(fixture.clock.load(Ordering::SeqCst), 1_185_000);
    assert_eq!(fixture.evidence()["kernelCleanupPassed"], true);
}
