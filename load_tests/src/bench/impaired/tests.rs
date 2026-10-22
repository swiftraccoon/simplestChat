//! Virtual owned processes and real private files exercise full acquisition and
//! retirement order. No server, socket, generator or kernel command is run.

use super::{
    controller::{self, Readiness, Services},
    plan::{Environment, Plan},
    state::{Authority, Identity},
};
use crate::bench::{
    clock::Clock,
    process::{Child, Command, Exit, Host, Sink, Stream},
    sampling::Ports,
};
use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU8, AtomicU64, Ordering},
    },
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    Port,
    Readiness,
    Root,
    Netem,
    Filter,
    Foreign,
    Delete,
    Dirty,
    GeneratorSpawn,
    GeneratorClose,
    ServerClose,
    ServerExit,
    Incomplete,
    ForeignSummary,
    Missing,
    Link,
    Oversize,
    Cancel,
    Timeout,
    LateCancel,
    Publish,
    Identity,
    Namespace,
    Mode,
    Statistics,
    GeneratorChanged,
    ServerChanged,
    MissingCounter(&'static str),
    CancelInspection(u8),
    CancelAdd(u8),
    CommandExit(i32),
    CommandExitThenCancel(i32, u8),
    CommandExitClose(i32),
    CommandIoThenCancel(u8),
    CommandTimeout,
    ClearExit(i32),
    GeneratorPollFailure(u8),
}

#[derive(Default)]
struct Journal {
    calls: Vec<(String, Vec<String>, Environment)>,
    stopped: Vec<String>,
    kernel: usize,
    generator: Option<(PathBuf, Value)>,
    active: usize,
    timeouts: Vec<Duration>,
    identities: usize,
}

pub(super) struct Fixture {
    pub root: PathBuf,
    state: Arc<Mutex<Journal>>,
    clock: Arc<AtomicU64>,
    cancellation: Arc<AtomicU8>,
    fault: Fault,
    generator_status: i32,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

pub(super) fn executable(path: &std::path::Path, contents: &[u8]) {
    fs::write(path, contents).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

impl Fixture {
    pub fn new(fault: Fault, generator_status: i32) -> (Self, Plan) {
        let root =
            std::env::temp_dir().join(format!("impaired-controller-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        for name in ["server", "generator", "sudo", "tc"] {
            executable(&root.join(name), format!("owned fixture {name}").as_bytes());
        }
        let environment = Environment::from([
            ("ACT".into(), "true".into()),
            ("LOCAL_CI_DISPOSABLE".into(), "1".into()),
            ("PATH".into(), root.clone().into_os_string()),
            (
                "SIMPLESTCHAT_BIN".into(),
                root.join("server").into_os_string(),
            ),
            (
                "LOAD_TEST_BIN".into(),
                root.join("generator").into_os_string(),
            ),
            ("DATABASE_URL".into(), "PRIVATE_SENTINEL".into()),
            (
                "LIBWEBRTC_FIELD_TRIALS".into(),
                "literal trial/value/".into(),
            ),
        ]);
        let plan = Plan::parse(
            &root,
            &["--output".into(), root.join("output").into_os_string()],
            environment,
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
                generator_status,
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
        serde_json::from_slice(&fs::read(self.root.join("output/impaired-exit.json")).unwrap())
            .unwrap()
    }
    fn summary(&self, plan: &Plan) -> Value {
        summary(plan, self.generator_status)
    }
}

pub(super) fn summary(plan: &Plan, exit: i32) -> Value {
    let latency = serde_json::to_value(crate::LatencyStats::default()).unwrap();
    let configuration = json!({"numClients":plan.clients,"numRooms":4,
        "durationSecs":plan.duration,"rampUpSecs":65,"warmupSecs":10,
        "publishRatio":1.0,"subscriptionPlan":"ring-v1","subscriptionSeed":17,
        "serverUrl":"ws://127.0.0.1:3129/ws","roomId":plan.room,
        "maxAudioConsumers":4,"maxVideoConsumers":4,"runLabel":"impaired",
        "outputDir":plan.output,"mediaConfig":{"audioEnabled":true,
            "videoEnabled":true,"videoWidth":640,"videoHeight":480,"videoFps":30}});
    let run = json!({"completed":true,"passed":exit==0,"failureReasons":["PRIVATE_SENTINEL"],
        "startedAt":"2026-10-22T00:00:00Z","finishedAt":"2026-10-22T00:02:15Z",
        "provenance":{"generatorBinarySha256":plan.generator.hash,"private":"PRIVATE_SENTINEL"},
        "configuration":configuration});
    json!({"schemaVersion":2,"successfulConnections":30,"failedConnections":0,
        "validatedConsumers":200,"failedConsumers":7,"keyframesRequested":42,
        "bandwidthEstimates":150,"totalPacketsReceived":900,"totalErrors":7,
        "skippedShortLivedConsumers":0,"p99ConnectionTimeMs":15,
        "sendMediaReady":latency,"receiveMediaReady":latency,
        "measurement":{"durationMs":60000,"packetsQueued":1000,"packetsReceived":900,
            "bytesQueued":10000,"bytesReceived":9000},
        "PRIVATE_SENTINEL":{"unrelated":"PRIVATE_SENTINEL"},"run":run})
}

#[async_trait::async_trait]
impl Clock for Fixture {
    fn elapsed_ms(&self) -> f64 {
        self.clock.load(Ordering::SeqCst) as f64
    }
    fn utc(&self) -> String {
        "2026-10-22T00:00:00.000Z".into()
    }
    async fn sleep(&self, duration: Duration) {
        self.clock
            .fetch_add(duration.as_millis() as u64, Ordering::SeqCst);
    }
}
impl Ports for Fixture {
    fn available(&self, port: u16, media: u16, workers: u32) -> Result<()> {
        assert_eq!((port, media, workers), (3129, 41100, 1));
        if self.fault == Fault::Port {
            bail!("PRIVATE_SENTINEL");
        }
        Ok(())
    }
}
#[async_trait::async_trait]
impl Readiness for Fixture {
    async fn ready(&self) -> bool {
        self.fault != Fault::Readiness
    }
}
impl Authority for Fixture {
    fn observe(&self) -> Result<Identity> {
        let mut state = self.state.lock().unwrap();
        state.identities += 1;
        Ok(Identity {
            namespace: (3, 4),
            interface: if self.fault == Fault::Identity && state.identities > 8 {
                9
            } else {
                1
            },
        })
    }
}

fn qdiscs(stage: usize) -> Value {
    if stage == 0 {
        return json!([{"kind":"noqueue","handle":"0:","root":true}]);
    }
    if stage == 7 {
        return json!([{"kind":"fq","handle":"7:","root":true}]);
    }
    let mut value = vec![json!({"kind":"prio","handle":"1:","root":true,
        "options":{"bands":3,"priomap":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}})];
    if stage >= 2 {
        value.push(json!({"kind":"netem","handle":"30:","parent":"1:3",
            "options":{"limit":200000,"delay":{"delay":0.05,"jitter":0.01,"correlation":0},
                "loss-random":{"loss":0.05,"correlation":0},"ecn":false,"gap":0}}));
    }
    Value::Array(value)
}
fn filters(stage: usize) -> Vec<u8> {
    if stage < 3 {
        return b"[]".to_vec();
    }
    let selector = |pref, port: &str, mask: &str| {
        format!(
            r#"{{"protocol":"ip","pref":{pref},
        "kind":"u32","options":{{"flowid":"1:3",
        "match":{{"value":"110000","mask":"ff0000","offmask":"","off":8}},
        "match":{{"value":"{port}","mask":"{mask}","offmask":"","off":20}}}}}}"#
        )
    };
    format!(
        "[{},{}]",
        selector(1, "a08c0000", "ffff0000"),
        selector(2, "a08c", "ffff")
    )
    .into_bytes()
}

struct FakeChild {
    state: Arc<Mutex<Journal>>,
    cancellation: Arc<AtomicU8>,
    clock: Arc<AtomicU64>,
    name: String,
    fault: Fault,
    status: i32,
    captured: Vec<u8>,
    stdout: Option<Sink>,
    stderr: Option<Sink>,
    stopped: bool,
    output: PathBuf,
    cancel_during_poll: Option<u8>,
    fail_during_poll: bool,
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
        if self.fail_during_poll {
            if self.fault == Fault::CommandTimeout {
                return Err(crate::owned_process::child::CommandTimedOut.into());
            }
            bail!("PRIVATE_SENTINEL");
        }
        if let Some(code) = self.cancel_during_poll.take() {
            self.cancellation.store(code, Ordering::SeqCst);
            return Ok(None);
        }
        if self.name == "server" && self.fault != Fault::ServerExit {
            return Ok(None);
        }
        if self.name == "generator" {
            if let Fault::GeneratorPollFailure(code) = self.fault {
                self.clock.fetch_add(1000000, Ordering::SeqCst);
                match code {
                    124 => return Err(crate::owned_process::child::CommandTimedOut.into()),
                    130 | 143 => return Err(crate::owned_process::Interrupted(code).into()),
                    _ => bail!("PRIVATE_SENTINEL"),
                }
            }
            if self.fault == Fault::Cancel {
                self.cancellation.store(143, Ordering::SeqCst);
                return Ok(None);
            }
            if self.fault == Fault::Timeout {
                self.clock.fetch_add(1000000, Ordering::SeqCst);
                return Ok(None);
            }
        }
        Ok(Some(Exit {
            code: Some(self.status),
            signal: None,
        }))
    }
    async fn stop(&mut self, _: Duration) -> Result<Exit> {
        assert!(!self.stopped);
        self.stopped = true;
        for sink in [&mut self.stdout, &mut self.stderr] {
            if let Some(Sink::File(mut file)) = sink.take() {
                file.write_all(b"owned inert log\n")?;
                nix::unistd::close(file)?;
            }
        }
        let mut state = self.state.lock().unwrap();
        state.stopped.push(self.name.clone());
        state.active -= 1;
        if (self.name == "generator" && self.fault == Fault::GeneratorClose)
            || (self.name == "server" && self.fault == Fault::ServerClose)
        {
            bail!("PRIVATE_SENTINEL");
        }
        if self.name == "generator" && self.fault != Fault::Missing {
            let (path, mut value) = state.generator.take().unwrap();
            if self.fault == Fault::Incomplete {
                value["run"]["completed"] = json!(false);
            }
            if self.fault == Fault::ForeignSummary {
                value["run"]["configuration"]["numRooms"] = json!(9);
            }
            if let Fault::MissingCounter(name) = self.fault {
                value.as_object_mut().unwrap().remove(name);
            }
            match self.fault {
                Fault::Link => std::os::unix::fs::symlink(self.name.as_str(), path)?,
                Fault::Oversize => {
                    let file = fs::File::create(path)?;
                    file.set_len(8 * 1024 * 1024 + 1)?;
                    nix::unistd::close(file)?;
                }
                _ => fs::write(path, serde_json::to_vec(&value)?)?,
            }
        }
        if self.name == "server" && self.fault == Fault::LateCancel {
            self.cancellation.store(130, Ordering::SeqCst);
        }
        if self.name == "sudo"
            && let Fault::CommandExitThenCancel(_, code) | Fault::CommandIoThenCancel(code) =
                self.fault
        {
            self.cancellation.store(code, Ordering::SeqCst);
        }
        if self.name == "sudo" && matches!(self.fault, Fault::CommandExitClose(_)) {
            bail!("PRIVATE_SENTINEL");
        }
        if self.name == "server" && self.fault == Fault::Namespace {
            fs::rename(&self.output, self.output.with_file_name("parked-output"))?;
            fs::create_dir(&self.output)?;
            fs::set_permissions(&self.output, fs::Permissions::from_mode(0o700))?;
        }
        if self.name == "server" && self.fault == Fault::Mode {
            fs::set_permissions(&self.output, fs::Permissions::from_mode(0o755))?;
        }
        if (self.name == "server" && self.fault == Fault::ServerChanged)
            || (self.name == "generator" && self.fault == Fault::GeneratorChanged)
        {
            fs::write(
                self.output.parent().unwrap().join(&self.name),
                b"changed selected executable",
            )?;
        }
        Ok(Exit {
            code: Some(self.status),
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
        let name = command.executable.file_name().unwrap().to_str().unwrap();
        let mut state = self.state.lock().unwrap();
        state.calls.push((
            name.into(),
            command.arguments.clone(),
            command.environment.clone(),
        ));
        state.timeouts.push(command.timeout);
        if name == "generator" && self.fault == Fault::GeneratorSpawn {
            bail!("PRIVATE_SENTINEL");
        }
        let mut status = if name == "generator" {
            self.generator_status
        } else {
            0
        };
        let mut captured = Vec::new();
        let mut cancel_during_poll = None;
        let mut fail_during_poll = false;
        if name == "tc" {
            if !command.cleanup
                && let Fault::CancelInspection(code) = self.fault
            {
                cancel_during_poll = Some(code);
            }
            if command.arguments[0] == "-s" && self.fault == Fault::Statistics {
                status = 42;
            }
            captured = if command.arguments[0] == "-s" {
                b"owned statistics\n".to_vec()
            } else if command.arguments[1] == "qdisc" {
                serde_json::to_vec(&qdiscs(state.kernel))?
            } else {
                filters(state.kernel)
            };
        } else if name == "sudo" {
            assert_eq!(&command.arguments[..2], ["-n", "--"]);
            let args = &command.arguments[3..];
            if args.contains(&"add".into()) {
                state.kernel += 1;
                match self.fault {
                    Fault::CancelAdd(code) => cancel_during_poll = Some(code),
                    Fault::CommandExit(code)
                    | Fault::CommandExitThenCancel(code, _)
                    | Fault::CommandExitClose(code) => {
                        status = code;
                    }
                    Fault::CommandIoThenCancel(_) | Fault::CommandTimeout => {
                        fail_during_poll = true;
                    }
                    _ => {}
                }
                if matches!(
                    (state.kernel, self.fault),
                    (1, Fault::Root) | (2, Fault::Netem) | (3, Fault::Filter)
                ) {
                    status = 42;
                }
                if self.fault == Fault::Foreign && state.kernel >= 3 {
                    state.kernel = 7;
                }
            } else {
                assert_eq!(args, ["qdisc", "del", "dev", "lo", "root", "handle", "1:"]);
                if self.fault != Fault::Dirty {
                    state.kernel = 0;
                }
                if self.fault == Fault::Delete {
                    status = 42;
                }
                if let Fault::ClearExit(code) = self.fault {
                    status = code;
                }
            }
        }
        if self.fault == Fault::Publish && name == "generator" {
            fs::create_dir(self.root.join("output/impaired-exit.json"))?;
        }
        state.active += 1;
        Ok(Box::new(FakeChild {
            state: self.state.clone(),
            cancellation: self.cancellation.clone(),
            clock: self.clock.clone(),
            name: name.into(),
            fault: self.fault,
            status,
            captured,
            stdout: Some(command.stdout),
            stderr: Some(command.stderr),
            stopped: false,
            output: self.root.join("output"),
            cancel_during_poll,
            fail_during_poll,
        }))
    }
    async fn cancelled(&self) -> u8 {
        let value = self.cancellation.load(Ordering::SeqCst);
        if value != 0 {
            value
        } else {
            std::future::pending().await
        }
    }
}

fn prepare(fixture: &Fixture, plan: &Plan) {
    fixture.state.lock().unwrap().generator = Some((
        plan.output.join("load_test_summary.json"),
        fixture.summary(plan),
    ));
}

#[tokio::test]
async fn complete_zero_and_delivery_failure_one_are_evidence_after_both_groups_close() {
    for (fault, status) in [(Fault::None, 0), (Fault::None, 1), (Fault::Statistics, 1)] {
        let (fixture, plan) = Fixture::new(fault, status);
        prepare(&fixture, &plan);
        let projection = controller::execute(&plan, &fixture.services())
            .await
            .unwrap();
        assert_eq!(projection.as_object().unwrap().len(), 8);
        assert_eq!(projection["failedConsumers"], 7);
        assert!(!projection.to_string().contains("PRIVATE_SENTINEL"));
        let evidence = fixture.evidence();
        assert_eq!(evidence["generatorExit"]["code"], status);
        assert_eq!(evidence["processCleanupPassed"], true);
        assert_eq!(evidence["kernelCleanupPassed"], true);
        assert_eq!(evidence["summaryAdmitted"], true);
        assert!(!evidence.to_string().contains("PRIVATE_SENTINEL"));
        let state = fixture.state.lock().unwrap();
        assert_eq!(state.active, 0);
        assert_eq!(state.kernel, 0);
        let gen_stop = state.stopped.iter().position(|v| v == "generator").unwrap();
        let last_tc = state.stopped.iter().rposition(|v| v == "tc").unwrap();
        let server_stop = state.stopped.iter().position(|v| v == "server").unwrap();
        assert!(gen_stop < last_tc && last_tc < server_stop);
        let environment = &state
            .calls
            .iter()
            .find(|(name, _, _)| name == "server")
            .unwrap()
            .2;
        assert!(!environment.contains_key(&OsString::from("DATABASE_URL")));
        assert_eq!(
            environment[&OsString::from("REGISTRATION_ENABLED")],
            "false"
        );
    }
}

#[tokio::test]
async fn partial_requests_and_every_failure_path_retire_all_acquired_groups_without_false_success()
{
    for fault in [
        Fault::Port,
        Fault::Readiness,
        Fault::Root,
        Fault::Netem,
        Fault::Filter,
        Fault::Delete,
        Fault::Dirty,
        Fault::Foreign,
        Fault::GeneratorSpawn,
        Fault::GeneratorClose,
        Fault::ServerClose,
        Fault::ServerExit,
        Fault::Incomplete,
        Fault::ForeignSummary,
        Fault::Missing,
        Fault::Link,
        Fault::Oversize,
        Fault::Cancel,
        Fault::Timeout,
        Fault::LateCancel,
        Fault::Publish,
        Fault::Identity,
        Fault::Namespace,
        Fault::Mode,
        Fault::GeneratorChanged,
        Fault::ServerChanged,
        Fault::MissingCounter("successfulConnections"),
    ] {
        let (fixture, plan) = Fixture::new(fault, 1);
        prepare(&fixture, &plan);
        let failure = controller::execute(&plan, &fixture.services())
            .await
            .expect_err(&format!("{fault:?}"));
        assert!(
            !failure.to_string().contains("PRIVATE_SENTINEL"),
            "{fault:?}"
        );
        assert_eq!(fixture.state.lock().unwrap().active, 0, "{fault:?}");
        if matches!(fault, Fault::Root | Fault::Netem | Fault::Filter) {
            assert_eq!(fixture.state.lock().unwrap().kernel, 0, "{fault:?}");
        }
        let expected = match fault {
            Fault::Cancel => 143,
            Fault::Timeout | Fault::Readiness => 124,
            Fault::LateCancel => 130,
            Fault::Root | Fault::Netem | Fault::Filter | Fault::Delete => 42,
            _ => 1,
        };
        assert_eq!(controller::exit_code(&failure), expected, "{fault:?}");
        if fault == Fault::Foreign {
            assert_eq!(fixture.state.lock().unwrap().kernel, 7);
        }
        if fault == Fault::GeneratorSpawn {
            assert_eq!(fixture.evidence()["processCleanupPassed"], false);
        }
    }
}

#[tokio::test]
async fn almost_expired_cli_deadlines_reserve_retirement_before_dispatch() {
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    let mut lease = super::state::Lease::new(&fixture.services()).unwrap();
    for deadline in [0.0, 1000.0, 25_000.0, 26_000.0] {
        assert!(
            lease
                .statistics(&plan, &fixture.services(), deadline)
                .await
                .is_err()
        );
        assert!(fixture.state.lock().unwrap().calls.is_empty());
    }
    lease
        .statistics(&plan, &fixture.services(), 26_001.0)
        .await
        .unwrap();
    assert_eq!(
        fixture.state.lock().unwrap().timeouts,
        [Duration::from_millis(1001)]
    );
}

#[tokio::test]
async fn bounded_command_cancellation_preserves_the_captured_signal_after_retirement() {
    for code in [130, 143] {
        for fault in [Fault::CancelInspection(code), Fault::CancelAdd(code)] {
            let (fixture, plan) = Fixture::new(fault, 0);
            prepare(&fixture, &plan);
            let error = controller::execute(&plan, &fixture.services())
                .await
                .unwrap_err();
            assert_eq!(controller::exit_code(&error), code, "{fault:?}");
            let evidence = fixture.evidence();
            assert_eq!(evidence["summaryAdmitted"], false);
            assert_eq!(evidence["processCleanupPassed"], false);
            let state = fixture.state.lock().unwrap();
            assert_eq!(state.active, 0, "{fault:?}");
            assert_eq!(state.kernel, 0, "{fault:?}");
            assert_eq!(state.calls.len(), state.stopped.len(), "{fault:?}");
            assert!(!state.calls.iter().any(|(name, _, _)| name == "generator"));
        }
    }
}

#[tokio::test]
async fn known_command_results_survive_later_cancellation_and_unproven_cleanup() {
    for code in [42, 124, 130, 143] {
        for fault in [
            Fault::CommandExit(code),
            Fault::CommandExitThenCancel(code, 143),
            Fault::CommandExitClose(code),
            Fault::ClearExit(code),
        ] {
            let (fixture, plan) = Fixture::new(fault, 0);
            prepare(&fixture, &plan);
            let error = controller::execute(&plan, &fixture.services())
                .await
                .unwrap_err();
            assert_eq!(controller::exit_code(&error), code as u8, "{fault:?}");
            assert_eq!(fixture.state.lock().unwrap().active, 0, "{fault:?}");
            assert_eq!(fixture.state.lock().unwrap().kernel, 0, "{fault:?}");
            assert_eq!(fixture.evidence()["summaryAdmitted"], false);
            if matches!(fault, Fault::CommandExitClose(_)) {
                assert_eq!(fixture.evidence()["processCleanupPassed"], false);
            }
        }
    }
    for fault in [
        Fault::CommandIoThenCancel(143),
        Fault::CommandExitClose(0),
        Fault::CommandTimeout,
    ] {
        let (fixture, plan) = Fixture::new(fault, 0);
        prepare(&fixture, &plan);
        let error = controller::execute(&plan, &fixture.services())
            .await
            .unwrap_err();
        let expected = if fault == Fault::CommandTimeout {
            124
        } else {
            1
        };
        assert_eq!(controller::exit_code(&error), expected, "{fault:?}");
        assert_eq!(fixture.state.lock().unwrap().active, 0);
        assert_eq!(fixture.evidence()["processCleanupPassed"], false);
        assert_eq!(fixture.evidence()["summaryAdmitted"], false);
    }
}

#[tokio::test]
async fn real_owned_command_body_timeout_has_a_typed_status_after_group_retirement() {
    use crate::bench::process::{self, NativeHost};
    let (fixture, _) = Fixture::new(Fault::None, 0);
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        process::run(
            &NativeHost::default(),
            Command {
                executable: "/bin/sh".into(),
                arguments: vec!["-c".into(), "while :; do sleep 1; done".into()],
                directory: fixture.root.clone(),
                environment: Environment::from([("PATH".into(), "/bin:/usr/bin".into())]),
                timeout: Duration::ZERO,
                stdout_limit: 1024,
                stderr_limit: 1024,
                input: Vec::new(),
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                cleanup: false,
            },
        ),
    )
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(process::failure_status(&error), Some(124));
    assert_eq!(error.to_string(), "command_timed_out");
}

#[tokio::test]
async fn generator_poll_failure_is_not_replaced_by_elapsed_time_after_the_poll() {
    for code in [1, 124, 130, 143] {
        let (fixture, plan) = Fixture::new(Fault::GeneratorPollFailure(code), 0);
        prepare(&fixture, &plan);
        let error = controller::execute(&plan, &fixture.services())
            .await
            .unwrap_err();
        assert_eq!(controller::exit_code(&error), code);
        assert_eq!(fixture.evidence()["summaryAdmitted"], false);
        assert!(!plan.output.join("impaired-summary.json").exists());
        assert_eq!(fixture.state.lock().unwrap().active, 0);
    }
}

#[tokio::test]
async fn all_eight_projection_counters_must_be_present_before_summary_admission() {
    for name in [
        "successfulConnections",
        "failedConnections",
        "validatedConsumers",
        "failedConsumers",
        "keyframesRequested",
        "bandwidthEstimates",
        "totalPacketsReceived",
        "totalErrors",
    ] {
        let (fixture, plan) = Fixture::new(Fault::MissingCounter(name), 0);
        prepare(&fixture, &plan);
        let error = controller::execute(&plan, &fixture.services())
            .await
            .unwrap_err();
        assert_eq!(controller::exit_code(&error), 1, "{name}");
        assert_eq!(fixture.evidence()["summaryAdmitted"], false, "{name}");
        assert!(
            !plan.output.join("impaired-summary.json").exists(),
            "{name}"
        );
        assert_eq!(fixture.state.lock().unwrap().active, 0);
        assert_eq!(fixture.state.lock().unwrap().kernel, 0);
    }
}

#[test]
fn projection_agrees_with_all_original_counter_values_without_changing_legacy_defaults() {
    let (_fixture, plan) = Fixture::new(Fault::None, 0);
    let original = summary(&plan, 0);
    let parsed = crate::bench::summary::parse(&original).unwrap();
    for name in [
        "successfulConnections",
        "failedConnections",
        "validatedConsumers",
        "failedConsumers",
        "keyframesRequested",
        "bandwidthEstimates",
        "totalPacketsReceived",
        "totalErrors",
    ] {
        let mut changed = original.clone();
        changed[name] = json!(original[name].as_u64().unwrap() + 1);
        assert!(parsed.impaired_projection(&changed).is_err(), "{name}");
        changed[name] = json!("0");
        assert!(parsed.impaired_projection(&changed).is_err(), "{name}");
    }
    for name in [
        "successfulConnections",
        "totalPacketsReceived",
        "keyframesRequested",
        "bandwidthEstimates",
    ] {
        let mut legacy = original.clone();
        legacy.as_object_mut().unwrap().remove(name);
        let legacy = crate::bench::summary::parse(&legacy).unwrap();
        assert!(legacy.impaired_projection(&original).is_err(), "{name}");
    }
}

#[test]
fn creator_witness_and_retained_leaf_refuse_foreign_owner_or_replacement_before_payload() {
    use super::output::Output;
    use std::os::unix::fs::MetadataExt;
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    let foreign = fixture.root.join("foreign-owner");
    assert!(Output::new_with(&foreign, |stamp| stamp.uid() ^ 1).is_err());
    assert_eq!(fs::read_dir(&foreign).unwrap().count(), 1);
    assert_eq!(fs::metadata(foreign.join(".creator")).unwrap().len(), 0);
    let output = Output::new(&plan.output).unwrap();
    fs::rename(&plan.output, fixture.root.join("parked")).unwrap();
    fs::create_dir(&plan.output).unwrap();
    fs::set_permissions(&plan.output, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        output
            .json("impaired-exit.json", &json!({"private":"sentinel"}))
            .is_err()
    );
    assert!(output.finish().is_err());
    assert_eq!(fs::read_dir(&plan.output).unwrap().count(), 0);
}

#[tokio::test]
async fn abnormal_generator_status_remains_primary_and_is_not_a_delivery_verdict() {
    for status in [42, 124, 130, 143] {
        let (fixture, plan) = Fixture::new(Fault::None, status);
        prepare(&fixture, &plan);
        let failure = controller::execute(&plan, &fixture.services())
            .await
            .unwrap_err();
        assert_eq!(controller::exit_code(&failure), status as u8);
        assert_eq!(fixture.evidence()["summaryAdmitted"], false);
        assert_eq!(fixture.state.lock().unwrap().active, 0);
    }
}

#[tokio::test]
async fn a_fresh_lease_never_claims_or_removes_preexisting_identical_or_foreign_state() {
    for stage in [4, 7] {
        let (fixture, plan) = Fixture::new(Fault::None, 0);
        fixture.state.lock().unwrap().kernel = stage;
        assert!(
            controller::execute(&plan, &fixture.services())
                .await
                .is_err()
        );
        let journal = fixture.state.lock().unwrap();
        assert_eq!(journal.kernel, stage);
        assert!(journal.calls.iter().all(|(name, _, _)| name != "sudo"));
        assert_eq!(journal.active, 0);
    }
}

#[test]
fn invalid_authority_arguments_and_inputs_refuse_before_any_spawn() {
    let (fixture, plan) = Fixture::new(Fault::None, 0);
    for (system, values) in [
        ("darwin", plan.environment.clone()),
        ("linux", Environment::new()),
    ] {
        assert!(
            Plan::parse(
                &fixture.root,
                &["--output".into(), plan.output.clone().into_os_string()],
                values,
                system
            )
            .is_err()
        );
    }
    for argument in [
        vec!["--output"],
        vec!["--unknown", "value"],
        vec!["--output", "value", "--output", "other"],
    ] {
        assert!(
            Plan::parse(
                &fixture.root,
                &argument.into_iter().map(OsString::from).collect::<Vec<_>>(),
                plan.environment.clone(),
                "linux"
            )
            .is_err()
        );
    }
    for (key, value) in [
        ("CLIENTS", "0"),
        ("CLIENTS", "10001"),
        ("DURATION", "2"),
        ("DURATION", "3301"),
        ("CLIENTS", "1;private"),
    ] {
        let mut environment = plan.environment.clone();
        environment.insert(key.into(), value.into());
        assert!(
            Plan::parse(
                &fixture.root,
                &["--output".into(), plan.output.clone().into_os_string()],
                environment,
                "linux"
            )
            .is_err()
        );
    }
    assert!(fixture.state.lock().unwrap().calls.is_empty());
}
