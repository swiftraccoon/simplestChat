//! Owned fake engine/clock observations only; no service, database or browser.

use super::{
    engine::{Engine, Operation, Request},
    plan,
    policy::{Clock, Supervisor},
};
use crate::{owned_process::Signals, test_env::Companion, tooling};
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
    rc::Rc,
    time::Duration,
};

#[derive(Clone)]
struct FakeClock(Rc<Cell<Duration>>);
impl FakeClock {
    fn new() -> Self {
        Self(Rc::new(Cell::new(Duration::ZERO)))
    }
    fn set(&self, seconds: u64) {
        self.0.set(Duration::from_secs(seconds));
    }
}
impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.0.get()
    }
    async fn sleep(&self, duration: Duration) {
        self.0.set(self.0.get() + duration);
    }
}
struct Fake {
    values: VecDeque<tooling::Result<Value>>,
    calls: Rc<RefCell<Vec<Request>>>,
    settled: bool,
    daemons: VecDeque<String>,
}
impl Fake {
    fn new(values: Vec<Value>) -> Self {
        Self {
            values: values.into_iter().map(Ok).collect(),
            calls: Rc::default(),
            settled: true,
            daemons: VecDeque::new(),
        }
    }
}
impl Engine for Fake {
    async fn call(&mut self, request: Request, _: &mut Signals) -> tooling::Result<Vec<u8>> {
        self.calls.borrow_mut().push(request);
        if request.operation == Operation::Daemon {
            let id = self
                .daemons
                .pop_front()
                .unwrap_or_else(|| "owned-daemon".into());
            return Ok(serde_json::to_vec(&id).unwrap());
        }
        self.values
            .pop_front()
            .expect("unexpected fake engine call")
            .map(|value| serde_json::to_vec(&value).unwrap())
    }
    fn settled(&self) -> bool {
        self.settled
    }
}
fn context() -> plan::Context {
    let environment = tooling::Environment::from([
        ("ACT".into(), "true".into()),
        ("LOCAL_CI_DISPOSABLE".into(), "1".into()),
        ("SOAK_POSTGRES_ID".into(), "a".repeat(64).into()),
        (
            "SOAK_POSTGRES_NETWORK".into(),
            "github_network_fixture".into(),
        ),
        ("SOAK_POSTGRES_PORT".into(), "5432".into()),
        ("SESSION_SOAK_E2E".into(), "1".into()),
        ("DISPOSABLE_TEST_DATABASE".into(), "1".into()),
        ("E2E_ARTIFACTS".into(), "/owned/soak".into()),
        ("DATABASE_URL".into(), plan::DATABASE.into()),
        ("TEST_DATABASE_URL".into(), plan::DATABASE.into()),
        ("PRIVATE_SENTINEL".into(), "never-in-public-report".into()),
    ]);
    plan::Context::new(
        std::path::Path::new("/owned/checkout"),
        environment,
        "Linux",
    )
    .unwrap()
}
fn inspection(running: bool) -> Value {
    json!({"id":"a".repeat(64),"image":format!("sha256:{}","b".repeat(64)),
        "networks":{"github_network_fixture":{"NetworkID":"c".repeat(64)}},
        "ports":{"5432/tcp":[{"HostIp":"0.0.0.0","HostPort":"5432"}]},
        "status":if running {"running"}else{"exited"},"running":running,
        "paused":false,"restarting":false,"dead":false,"exit":0,"oom":false,
        "error":"","health":if running {Some("healthy")}else{None}})
}
fn image() -> Value {
    json!(format!("sha256:{}", "b".repeat(64)))
}
fn signals() -> Signals {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    // A closed fixture input parks forever, like a registration with no signal.
    drop(sender);
    Signals::injected(receiver)
}

#[test]
fn context_keeps_closed_full_profile_service_selection_and_literal_workload() {
    let selected = context();
    assert_eq!(
        selected.workload(),
        ["npm", "--prefix", "web/e2e", "run", "test:session-soak"]
    );
    assert_eq!(
        selected.environment[std::ffi::OsStr::new("PRIVATE_SENTINEL")],
        "never-in-public-report"
    );
    for (name, value) in [
        ("SOAK_POSTGRES_ID", "short"),
        ("SOAK_POSTGRES_NETWORK", "$(private)"),
        ("SOAK_POSTGRES_PORT", "5433"),
        ("SESSION_SOAK_PROFILE", "smoke"),
        ("TURN_E2E", "1"),
        ("TEST_BROWSER_HTTPS", "1"),
        ("ACT", "false"),
        (
            "DATABASE_URL",
            "postgres://test_owner@127.0.0.1:5433/other?sslmode=disable",
        ),
        (
            "TEST_DATABASE_URL",
            "postgres://test_owner@127.0.0.1:5432/other?sslmode=disable",
        ),
        ("DOCKER_HOST", "tcp://127.0.0.1:2375"),
        ("DOCKER_CONTEXT", "another-local-daemon"),
        ("DOCKER_TLS_VERIFY", "1"),
    ] {
        let mut environment = selected.environment.clone();
        environment.insert(name.into(), value.into());
        assert!(
            plan::Context::new(&selected.root, environment, "Linux").is_err(),
            "{name}"
        );
    }
}

#[test]
fn native_docker_argv_freezes_local_endpoint_without_port_discovery() {
    let selected = context();
    let native = super::engine::Native {
        context: &selected,
        settled: true,
    };
    for operation in [
        Operation::Daemon,
        Operation::Image,
        Operation::Inspect,
        Operation::Stop,
        Operation::Start,
    ] {
        let arguments = native.arguments(operation);
        assert_eq!(&arguments[..2], ["--host", plan::ENDPOINT]);
        assert!(!arguments.contains(&"ps"));
        if matches!(operation, Operation::Stop | Operation::Start) {
            assert_eq!(
                arguments,
                [
                    "--host",
                    plan::ENDPOINT,
                    if operation == Operation::Stop {
                        "stop"
                    } else {
                        "start"
                    },
                    &selected.id
                ]
            );
        }
    }
    assert_eq!(
        selected.environment[std::ffi::OsStr::new("DOCKER_HOST")],
        plan::ENDPOINT
    );
    assert!(
        !selected
            .environment
            .contains_key(std::ffi::OsStr::new("DOCKER_CONTEXT"))
    );
    assert_eq!(
        selected.environment[std::ffi::OsStr::new("SESSION_SOAK_PROFILE")],
        "full"
    );
}

#[tokio::test]
async fn daemon_identity_change_before_stop_never_reaches_mutation() {
    let selected = context();
    let clock = FakeClock::new();
    let mut fake = Fake::new(vec![image(), inspection(true), inspection(true)]);
    fake.daemons = ["first", "first", "first", "replacement"]
        .map(str::to_owned)
        .into();
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    assert!(owner.advance(&mut signals).await.is_err());
    owner.engine.daemons.push_back("replacement".into());
    assert_eq!(owner.finish(1, &mut signals).await, 1);
    assert!(!owner.report.restoration_observed);
    assert!(
        calls
            .borrow()
            .iter()
            .all(|v| !matches!(v.operation, Operation::Stop | Operation::Start))
    );
}

#[tokio::test]
async fn changed_daemon_before_start_refuses_both_normal_start_and_cleanup_start() {
    let selected = context();
    let clock = FakeClock::new();
    let mut fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
        inspection(false),
        inspection(false),
    ]);
    fake.daemons = std::iter::repeat_n("owned-daemon".to_owned(), 7)
        .chain(["replacement".to_owned(), "replacement".to_owned()])
        .collect();
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    owner.advance(&mut signals).await.unwrap();
    clock.set(608);
    assert!(owner.advance(&mut signals).await.is_err());
    assert_eq!(owner.finish(1, &mut signals).await, 1);
    assert!(!owner.report.restoration_observed);
    assert!(
        calls
            .borrow()
            .iter()
            .all(|v| v.operation != Operation::Start)
    );
}

#[tokio::test]
async fn daemon_identity_is_opaque_bounded_and_keeps_both_documented_forms() {
    for daemon in [
        "c20f7230-59a2-4824-a2f4-fda71c982ee6",
        "AAAA:BBBB:CCCC:DDDD:EEEE:FFFF",
    ] {
        let selected = context();
        let clock = FakeClock::new();
        let mut fake = Fake::new(vec![image(), inspection(true)]);
        fake.daemons = vec![daemon.to_owned(), daemon.to_owned()].into();
        Supervisor::new(&selected, fake, clock)
            .admit(&mut signals())
            .await
            .unwrap();
    }
    for daemon in [String::new(), "x".repeat(257), "private\nvalue".into()] {
        let selected = context();
        let clock = FakeClock::new();
        let mut fake = Fake::new(vec![]);
        fake.daemons.push_back(daemon);
        assert!(
            Supervisor::new(&selected, fake, clock)
                .admit(&mut signals())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn cleanup_polls_same_service_through_late_stop_and_starting_health() {
    let selected = context();
    let clock = FakeClock::new();
    let mut starting = inspection(true);
    starting["health"] = json!("starting");
    let mut fake = Fake::new(vec![image(), inspection(true), inspection(true)]);
    fake.values.extend([
        Err(plan::failed(130)),
        Ok(starting.clone()),
        Ok(inspection(false)),
        Ok(json!(null)),
        Ok(starting),
        Ok(inspection(true)),
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    assert!(owner.advance(&mut signals).await.is_err());
    assert_eq!(owner.finish(130, &mut signals).await, 130);
    assert!(owner.report.restoration_observed);
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation == Operation::Start)
            .count(),
        1
    );
    assert!(!owner.report.outage_completed);
}

#[tokio::test]
async fn active_completion_timestamp_precedes_cleanup_reserve_and_late_zero_cannot_pass() {
    for timely in [true, false] {
        let selected = context();
        let clock = FakeClock::new();
        let fake = Fake::new(vec![inspection(true)]);
        let mut owner = Supervisor::new(&selected, fake, clock.clone());
        owner.report.stop_attempted = true;
        owner.report.outage_completed = true;
        owner.test_identity("b".repeat(64), "owned-daemon".into());
        clock.0.set(if timely {
            plan::WATCHDOG - Duration::from_secs(1)
        } else {
            plan::WATCHDOG
        });
        owner.workload_ended();
        clock.0.set(plan::WATCHDOG + Duration::from_secs(15));
        let status = owner.finish(0, &mut signals()).await;
        assert_eq!(status, if timely { 0 } else { 124 });
        assert_eq!(owner.finish(status, &mut signals()).await, status);
    }
}

#[tokio::test]
async fn exact_600_then_eight_seconds_follow_positive_stopped_observation() {
    let selected = context();
    let clock = FakeClock::new();
    let fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
        inspection(false),
        inspection(false),
        json!(null),
        inspection(true),
        inspection(true),
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation != Operation::Daemon)
            .count(),
        2
    );
    owner.wait().await;
    assert_eq!(clock.now(), Duration::from_secs(600));
    owner.advance(&mut signals).await.unwrap();
    assert!(owner.report.stop_attempted && owner.report.stopped_observed);
    assert!(!owner.report.outage_completed);
    owner.wait().await;
    assert_eq!(clock.now(), Duration::from_secs(608));
    owner.advance(&mut signals).await.unwrap();
    assert_eq!(owner.finish(0, &mut signals).await, 0);
    assert!(owner.report.outage_completed && owner.report.restoration_observed);
    assert!(owner.report.docker_cli_groups_settled);
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation == Operation::Stop)
            .count(),
        1
    );
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation == Operation::Start)
            .count(),
        1
    );
    assert!(calls.borrow().last().unwrap().cleanup);
    assert!(
        !serde_json::to_string(&owner.report)
            .unwrap()
            .contains("never-in-public-report")
    );
}

#[tokio::test]
async fn no_early_stop_and_no_success_after_short_workload() {
    for status in [0, 23, 124, 130, 143] {
        let selected = context();
        let clock = FakeClock::new();
        let fake = Fake::new(vec![image(), inspection(true)]);
        let calls = fake.calls.clone();
        let mut owner = Supervisor::new(&selected, fake, clock);
        let mut signals = signals();
        owner.admit(&mut signals).await.unwrap();
        assert_eq!(
            owner.finish(status, &mut signals).await,
            if status == 0 { 1 } else { status }
        );
        assert_eq!(
            calls
                .borrow()
                .iter()
                .filter(|v| v.operation != Operation::Daemon)
                .count(),
            2
        );
        assert!(!owner.report.stop_attempted);
    }
}

#[tokio::test]
async fn failed_or_interrupted_stop_still_restores_exact_service_and_keeps_primary() {
    for code in [23, 124, 130, 143] {
        let selected = context();
        let clock = FakeClock::new();
        let mut fake = Fake::new(vec![image(), inspection(true), inspection(true)]);
        fake.values.extend([
            Err(plan::failed(code)),
            Ok(inspection(false)),
            Ok(json!(null)),
            Ok(inspection(true)),
        ]);
        let calls = fake.calls.clone();
        let mut owner = Supervisor::new(&selected, fake, clock.clone());
        let mut signals = signals();
        owner.admit(&mut signals).await.unwrap();
        clock.set(600);
        assert_eq!(
            owner
                .advance(&mut signals)
                .await
                .unwrap_err()
                .downcast_ref::<tooling::Failure>()
                .unwrap()
                .code,
            code
        );
        assert_eq!(
            owner.finish(i32::from(code), &mut signals).await,
            i32::from(code)
        );
        assert!(owner.report.restoration_observed);
        let calls = calls.borrow();
        assert!(
            calls
                .iter()
                .filter(|v| v.operation == Operation::Start)
                .all(|v| v.cleanup)
        );
    }
}

#[tokio::test]
async fn missing_same_identity_and_bad_terminal_state_never_start_or_pass() {
    for field in [
        "id",
        "image",
        "networks",
        "ports",
        "paused",
        "restarting",
        "status",
    ] {
        let selected = context();
        let clock = FakeClock::new();
        let mut changed = inspection(true);
        match field {
            "id" => changed[field] = json!("d".repeat(64)),
            "image" => changed[field] = json!(format!("sha256:{}", "d".repeat(64))),
            "networks" | "ports" => changed[field] = json!({}),
            "status" => changed[field] = json!("restarting"),
            _ => changed[field] = json!(true),
        }
        let fake = Fake::new(vec![image(), inspection(true), changed]);
        let calls = fake.calls.clone();
        let mut owner = Supervisor::new(&selected, fake, clock.clone());
        let mut signals = signals();
        owner.admit(&mut signals).await.unwrap();
        clock.set(600);
        assert!(owner.advance(&mut signals).await.is_err());
        assert_eq!(owner.finish(1, &mut signals).await, 1);
        assert!(
            calls
                .borrow()
                .iter()
                .all(|v| !matches!(v.operation, Operation::Stop | Operation::Start))
        );
    }
}

#[tokio::test]
async fn restoration_deadline_reserves_cli_settlement_and_server_retirement() {
    let selected = context();
    let clock = FakeClock::new();
    let fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    owner.advance(&mut signals).await.unwrap();
    clock
        .0
        .set(plan::STEP - plan::HARNESS_RESERVE - plan::CLI_SETTLE);
    assert_eq!(owner.finish(130, &mut signals).await, 130);
    assert!(!owner.report.restoration_observed);
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation != Operation::Daemon)
            .count(),
        5,
        "no child when its settlement allowance does not fit"
    );
}

#[tokio::test]
async fn early_outage_timer_and_watchdog_never_create_success_or_extra_stop() {
    let selected = context();
    let clock = FakeClock::new();
    let fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
        inspection(false),
        json!(null),
        inspection(true),
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    owner.advance(&mut signals).await.unwrap();
    clock.set(607);
    assert!(owner.advance(&mut signals).await.is_err());
    assert_eq!(owner.finish(1, &mut signals).await, 1);
    assert!(!owner.report.outage_completed && owner.report.restoration_observed);
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation == Operation::Stop)
            .count(),
        1
    );

    let fake = Fake::new(vec![image(), inspection(true)]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    clock.set(0);
    owner.admit(&mut signals).await.unwrap();
    clock.0.set(plan::WATCHDOG);
    assert_eq!(
        owner
            .advance(&mut signals)
            .await
            .unwrap_err()
            .downcast_ref::<tooling::Failure>()
            .unwrap()
            .code,
        124
    );
    assert_eq!(owner.finish(124, &mut signals).await, 124);
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation != Operation::Daemon)
            .count(),
        2
    );
}

#[tokio::test]
async fn changed_restore_identity_never_gets_a_start_and_cannot_turn_zero_into_pass() {
    let selected = context();
    let clock = FakeClock::new();
    let mut changed = inspection(false);
    changed["id"] = json!("d".repeat(64));
    let fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
        changed,
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let mut signals = signals();
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    owner.advance(&mut signals).await.unwrap();
    assert_eq!(owner.finish(0, &mut signals).await, 1);
    assert!(!owner.report.restoration_observed);
    assert!(
        calls
            .borrow()
            .iter()
            .all(|v| v.operation != Operation::Start)
    );
}

#[tokio::test]
async fn cleanup_uses_only_remaining_body_budget_and_never_skips_pending_cancel() {
    let selected = context();
    let clock = FakeClock::new();
    let fake = Fake::new(vec![
        image(),
        inspection(true),
        inspection(true),
        json!(null),
        inspection(false),
        inspection(true),
    ]);
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock.clone());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    owner.admit(&mut signals).await.unwrap();
    clock.set(600);
    owner.advance(&mut signals).await.unwrap();
    clock
        .0
        .set(plan::STEP - plan::HARNESS_RESERVE - plan::CLI_SETTLE - Duration::from_secs(3));
    sender.send(143).unwrap();
    assert_eq!(owner.finish(143, &mut signals).await, 143);
    assert!(owner.report.restoration_observed);
    assert_eq!(
        signals.pending().await,
        Some(143),
        "cleanup did not consume or ignore final signal ownership"
    );
    let calls = calls.borrow();
    let last = calls.last().unwrap();
    assert!(last.cleanup);
    assert_eq!(last.timeout, Duration::from_secs(3));
}

#[tokio::test]
async fn unproven_initial_cli_cleanup_refuses_before_any_mutation() {
    let selected = context();
    let clock = FakeClock::new();
    let mut fake = Fake::new(vec![image(), inspection(true)]);
    fake.settled = false;
    let calls = fake.calls.clone();
    let mut owner = Supervisor::new(&selected, fake, clock);
    assert!(owner.admit(&mut signals()).await.is_err());
    assert_eq!(
        calls
            .borrow()
            .iter()
            .filter(|v| v.operation != Operation::Daemon)
            .count(),
        2
    );
}

#[tokio::test]
async fn native_fake_cli_keeps_captured_environment_and_discards_raw_ids_and_errors() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let directory = std::env::temp_dir()
        .canonicalize()
        .unwrap()
        .join(format!("soak-native-fixture-{}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let program = directory.join("docker");
    fs::write(
        &program,
        r#"#!/usr/bin/python3
import json,os,sys
from pathlib import Path
value={'args':sys.argv[1:],'cwd':os.getcwd(),'sentinel':os.environ.get('PRIVATE_SENTINEL'),
 'ambientCargo':any(key.startswith('CARGO_') for key in os.environ),
 'stdoutNull':os.fstat(1).st_rdev==os.stat('/dev/null').st_rdev,
 'stderrNull':os.fstat(2).st_rdev==os.stat('/dev/null').st_rdev}
Path('captured.json').write_text(json.dumps(value))
print('PRIVATE-CID-AND-OUTPUT')
print('PRIVATE-ERROR',file=sys.stderr)
"#,
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
    let mut selected = context();
    selected.root = directory.clone();
    selected
        .environment
        .insert("PATH".into(), directory.clone().into());
    let mut native = super::engine::Native {
        context: &selected,
        settled: true,
    };
    let output = native
        .call(
            Request {
                operation: Operation::Stop,
                timeout: Duration::from_secs(2),
                cleanup: false,
            },
            &mut signals(),
        )
        .await
        .unwrap();
    assert!(output.is_empty() && native.settled());
    let captured: Value =
        serde_json::from_slice(&fs::read(directory.join("captured.json")).unwrap()).unwrap();
    assert_eq!(
        captured["args"],
        json!(["--host", plan::ENDPOINT, "stop", selected.id])
    );
    assert_eq!(captured["sentinel"], "never-in-public-report");
    assert_eq!(captured["ambientCargo"], false);
    assert_eq!(captured["stdoutNull"], true);
    assert_eq!(captured["stderrNull"], true);
    fs::remove_dir_all(directory).unwrap();
}
