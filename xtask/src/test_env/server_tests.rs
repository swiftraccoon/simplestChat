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
