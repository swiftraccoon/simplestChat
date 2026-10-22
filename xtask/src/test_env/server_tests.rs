//! Actual service ownership with inert scripts; no HTTP, TCP, or UDP services.

use super::*;
use serde_json::Value;

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        Self {
            root: create_private_directory(&std::env::temp_dir(), "server-service.").unwrap(),
        }
    }
    fn tool(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/usr/bin/python3\n{body}")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn harness(&self) -> Harness {
        Harness {
            plan: Plan {
                environment: Environment::new(),
                binary: self.root.join("server"),
                workdir: self.root.clone(),
                database_url: "postgres://test_owner@127.0.0.1/simplestchat_test?sslmode=disable"
                    .into(),
                port: 3119,
                media_port: 41010,
                announce: Ipv4Addr::LOCALHOST,
                base_url: "http://127.0.0.1:3119".into(),
                readiness_url: "http://127.0.0.1:3119".into(),
                https: None,
                turn: None,
                passkey: false,
                presentation: false,
                metrics_token: None,
                artifacts: self.root.clone(),
                path: self.root.to_string_lossy().into_owned(),
            },
            server: None,
            turn: None,
            turn_config: None,
            https: None,
        }
    }
    fn report(&self, name: &str) -> Value {
        serde_json::from_slice(&fs::read(self.root.join(name)).unwrap()).unwrap()
    }

    fn environment(&self) -> Environment {
        fs::write(self.root.join("Cargo.toml"), b"fixture only").unwrap();
        fs::create_dir(self.root.join("migrations")).unwrap();
        let binary = self.tool("server", IDLE);
        let mut environment = Environment::from([
            ("DISPOSABLE_TEST_DATABASE".into(), "1".into()),
            (
                "DATABASE_URL".into(),
                "postgres://test_owner@127.0.0.1/simplestchat_test?sslmode=disable".into(),
            ),
            ("TEST_SERVER_PORT".into(), "3211".into()),
            ("TEST_MEDIA_PORT".into(), "42011".into()),
            ("TEST_ANNOUNCE_IP".into(), "127.0.0.1".into()),
            ("PASSKEY_E2E".into(), "1".into()),
            ("PRESENTATION_E2E".into(), "1".into()),
            ("LIFECYCLE_E2E".into(), "1".into()),
        ]);
        for (name, path) in [
            ("TEST_SERVER_BINARY", &binary),
            ("TEST_SERVER_WORKDIR", &self.root),
            ("PATH", &self.root),
            ("RUNNER_TEMP", &self.root),
        ] {
            environment.insert(name.into(), path.as_os_str().to_owned());
        }
        environment
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

const IDLE: &str = r#"import signal,time
from pathlib import Path
root=Path(__file__).parent
name=Path(__file__).name
def record(suffix):
 pending=root/(name+suffix+'.pending');pending.write_text(name);pending.replace(root/(name+suffix))
def finish(*_):record('.term');raise SystemExit(0)
signal.signal(signal.SIGTERM,finish)
record('.ready')
while True:time.sleep(.01)
"#;

async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn readiness_cancellation_closes_helper_then_service_and_keeps_interrupt() {
    let fixture = Fixture::new();
    fixture.tool("server", IDLE);
    fixture.tool("curl", IDLE);
    let mut harness = fixture.harness();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let command = vec!["/this-must-never-run".into()];
    let interrupt = async {
        ready(&fixture.root.join("curl.ready")).await;
        ready(&fixture.root.join("server.ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(harness.start_and_run(&command, &mut signals), interrupt);
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        143
    );
    assert!(fixture.root.join("curl.term").is_file());
    assert_eq!(harness.cleanup(143).await, 143);
    assert!(fixture.root.join("server.term").is_file());
    let report = fixture.report("server-shutdown.json");
    assert_eq!(report["groupCleanupPassed"], true);
    assert_eq!(report["passed"], true);
    assert_eq!(report["termAttempted"], true);
    assert_eq!(report["killAttempted"], false);
}

#[tokio::test]
async fn naturally_exited_service_leaders_still_require_group_retirement() {
    let fixture = Fixture::new();
    let program = fixture.tool(
        "server",
        r#"import os,signal,time
from pathlib import Path
root=Path(__file__).parent
if os.fork()==0:
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 pending=root/'worker.pending';pending.write_text(str(os.getpid()));pending.replace(root/'worker')
 while True:time.sleep(.01)
while not (root/'worker').exists():time.sleep(.01)
"#,
    );
    let mut harness = fixture.harness();
    harness.server = Some(Service::spawn(Command::new(&program)).unwrap());
    let pid = rustix::process::Pid::from_raw(
        i32::try_from(harness.server.as_ref().unwrap().pid()).unwrap(),
    )
    .unwrap();
    tokio::time::timeout(Duration::from_secs(5), crate::owned_process::exited(pid))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(harness.cleanup(0).await, 0);
    let report = fixture.report("server-shutdown.json");
    assert_eq!(report["waitStatus"], 0);
    assert_eq!(report["groupCleanupPassed"], true);
    assert_eq!(report["passed"], true);
    assert_eq!(report["termAttempted"], false);
    assert_eq!(report["killAttempted"], false);
}

#[tokio::test]
async fn generated_turn_secret_is_owned_before_a_failing_spawn() {
    let fixture = Fixture::new();
    let mut harness = fixture.harness();
    harness.plan.turn = Some(TurnPlan { port: 34790 });
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    assert!(
        harness
            .start_and_run(&["/this-must-never-run".into()], &mut signals)
            .await
            .is_err()
    );
    let config = harness.turn_config.as_ref().unwrap().clone();
    assert!(config.is_file());
    assert_eq!(harness.cleanup(23).await, 23);
    assert!(!config.exists());
}

#[tokio::test]
async fn queued_turn_readiness_cancellation_does_not_poll_a_socket() {
    let fixture = Fixture::new();
    let program = fixture.tool("turnserver", IDLE);
    let mut harness = fixture.harness();
    harness.turn = Some(Service::spawn(Command::new(program)).unwrap());
    ready(&fixture.root.join("turnserver.ready")).await;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    sender.send(130).unwrap();
    let result = harness.await_turn_ready(34790, &mut signals).await;
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        130
    );
    assert_eq!(harness.cleanup(130).await, 130);
    assert!(fixture.root.join("turnserver.term").is_file());
    assert_eq!(
        fixture.report("turn-shutdown.json")["groupCleanupPassed"],
        true
    );
}

#[tokio::test]
async fn independent_captured_environments_determine_plans_without_global_mutation() {
    let fixture = Fixture::new();
    let mut environment = fixture.environment();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let command = vec!["/this-must-never-run".into()];
    let first = plan(&fixture.root, &command, &environment, &mut signals)
        .await
        .unwrap();
    assert_eq!(first.port, 3211);
    assert_eq!(first.media_port, 42011);
    assert_eq!(first.announce, Ipv4Addr::LOCALHOST);
    assert_eq!(first.base_url, "http://localhost:3211");
    assert!(first.presentation);
    assert!(first.metrics_token.is_some());
    assert_eq!(first.artifacts.parent(), Some(fixture.root.as_path()));
    assert!(first.artifacts.is_dir());

    environment.insert("TEST_SERVER_PORT".into(), "3212".into());
    environment.insert("PASSKEY_E2E".into(), "0".into());
    environment.insert("PRESENTATION_E2E".into(), "0".into());
    environment.insert("LIFECYCLE_E2E".into(), "0".into());
    let second = plan(&fixture.root, &command, &environment, &mut signals)
        .await
        .unwrap();
    assert_eq!(second.port, 3212);
    assert_eq!(second.base_url, "http://127.0.0.1:3212");
    assert!(!second.presentation);
    assert!(second.metrics_token.is_none());
    assert_eq!(
        first
            .environment
            .get(OsStr::new("TEST_SERVER_PORT"))
            .and_then(|value| value.to_str()),
        Some("3211")
    );
    assert_eq!(first.base_url, "http://localhost:3211");
    assert_ne!(first.artifacts, second.artifacts);

    environment.insert("TEST_SERVER_PORT".into(), "".into());
    let refused = plan(&fixture.root, &command, &environment, &mut signals)
        .await
        .err()
        .unwrap();
    assert_eq!(refused.downcast_ref::<Stop>().unwrap().code, 2);
}

#[tokio::test]
async fn test_command_uses_captured_values_and_owned_overrides_without_ambient_cargo() {
    let fixture = Fixture::new();
    let program = fixture.tool(
        "test-command",
        r#"import json,os
from pathlib import Path
value={'captured':os.environ.get('CAPTURED_ONLY'),
 'ambientCargo':any(name.startswith('CARGO_') for name in os.environ),
 'pid':os.environ.get('TEST_SERVER_PID'),
 'ownedToken':os.environ.get('TEST_METRICS_TOKEN')=='owned-test-token',
 'hasToken':'TEST_METRICS_TOKEN' in os.environ,'community':os.environ.get('COMMUNITY_E2E'),
 'url':os.environ.get('BASE_URL')}
Path(__file__).with_name('captured.json').write_text(json.dumps(value))
"#,
    );
    let mut harness = fixture.harness();
    harness.plan.environment = Environment::from([
        ("CAPTURED_ONLY".into(), "original-selection".into()),
        ("TEST_SERVER_PID".into(), "unowned-pid-sentinel".into()),
        (
            "TEST_METRICS_TOKEN".into(),
            "unowned-secret-sentinel".into(),
        ),
    ]);
    let command = vec![program.to_string_lossy().into_owned()];
    for metrics in [false, true] {
        harness.plan.metrics_token = metrics.then(|| "owned-test-token".into());
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        assert_eq!(
            harness
                .run_child(&command, 1234, &mut signals)
                .await
                .unwrap(),
            0
        );
        let observed = fixture.report("captured.json");
        // Interpreter launchers can add their own SDK variables on macOS;
        // Cargo's ambient test-run context still must not cross this boundary.
        assert_eq!(observed["ambientCargo"], false);
        assert_eq!(observed["captured"], "original-selection");
        assert_eq!(observed["pid"], "1234");
        assert_eq!(observed["community"], "1");
        assert_eq!(observed["url"], "http://127.0.0.1:3119");
        assert_eq!(observed["hasToken"], metrics);
        assert_eq!(observed["ownedToken"], metrics);
    }
}

#[tokio::test]
async fn companion_finalization_follows_workload_retirement_before_server_cleanup_on_failure() {
    struct Order {
        root: PathBuf,
        action_failure: bool,
        finished: bool,
    }
    impl crate::test_env::Companion for Order {
        async fn wait(&self) {
            if self.action_failure {
                ready(&self.root.join("test-workload.ready")).await;
            } else {
                std::future::pending::<()>().await;
            }
        }
        async fn advance(&mut self, _: &mut Signals) -> Result<()> {
            Err(Stop {
                code: 130,
                message: "Owned companion interruption.".into(),
            }
            .into())
        }
        async fn finish(&mut self, status: i32, _: &mut Signals) -> i32 {
            assert!(self.root.join("test-workload.term").exists());
            assert!(!self.root.join("server.term").exists());
            self.finished = true;
            status
        }
    }
    for action_failure in [false, true] {
        let fixture = Fixture::new();
        let mut environment = fixture.environment();
        for key in ["PASSKEY_E2E", "PRESENTATION_E2E", "LIFECYCLE_E2E"] {
            environment.remove(std::ffi::OsStr::new(key));
        }
        environment.insert("E2E_ARTIFACTS".into(), fixture.root.clone().into());
        fixture.tool(
            "curl",
            r#"import time
from pathlib import Path
while not Path(__file__).with_name('server.ready').exists():time.sleep(.005)
"#,
        );
        let workload = fixture.tool(
            "test-workload",
            if action_failure {
                IDLE
            } else {
                r#"from pathlib import Path
Path(__file__).with_name('test-workload.term').write_text('finished')
raise SystemExit(23)
"#
            },
        );
        let mut order = Order {
            root: fixture.root.clone(),
            action_failure,
            finished: false,
        };
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let result = run_with_companion(
            &fixture.root,
            &[workload.to_string_lossy().into_owned()],
            &environment,
            &mut signals,
            &mut order,
        )
        .await
        .unwrap();
        assert_eq!(result, if action_failure { 130 } else { 23 });
        assert!(order.finished);
        assert!(fixture.root.join("server.term").exists());
        assert_eq!(fixture.report("server-shutdown.json")["passed"], true);
    }
}
