//! Fake allowlisted tools and owned files, never a PostgreSQL daemon or socket.

use super::*;
use serde_json::{Value, json};

struct Fixture {
    directory: PathBuf,
}
impl Fixture {
    fn new(statuses: &[i32], fast: i32, immediate: i32, block: &str) -> Self {
        let directory =
            create_private_directory(&std::env::temp_dir(), "postgres-service.").unwrap();
        fs::write(
            directory.join("config.json"),
            serde_json::to_vec(
                &json!({"statuses":statuses,"fast":fast,"immediate":immediate,"block":block}),
            )
            .unwrap(),
        )
        .unwrap();
        let program = r#"#!/usr/bin/python3
import json,signal,sys,time
from pathlib import Path
root=Path(__file__).parent
config=json.loads((root/'config.json').read_text())
args=sys.argv[1:];tool=Path(__file__).name
with (root/'events').open('a') as events:events.write(json.dumps([tool,*args])+'\n')
phase='start' if tool=='pg_ctl' and 'start' in args else ('stop' if tool=='pg_ctl' and 'stop' in args else tool)
if phase==config['block']:
 def finish(*_):
  (root/'term').write_text(phase);sys.exit(0)
 signal.signal(signal.SIGTERM,finish)
 pending=root/'ready.pending';pending.write_text(phase);pending.replace(root/'ready')
 while phase!='stop' or not (root/'release').exists():time.sleep(.01)
if tool=='pg_ctl' and 'status' in args:
 counter=root/'counter'
 index=int(counter.read_text()) if counter.exists() else 0
 counter.write_text(str(index+1))
 sys.exit(config['statuses'][min(index,len(config['statuses'])-1)])
if tool=='pg_ctl' and 'stop' in args:sys.exit(config['immediate'] if 'immediate' in args else config['fast'])
sys.exit(0)
"#;
        for tool in ["pg_ctl", "initdb", "createdb"] {
            let path = directory.join(tool);
            fs::write(&path, program).unwrap();
            fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
        }
        Self { directory }
    }
    fn cluster(&self) -> Cluster {
        Cluster {
            path: self.directory.to_string_lossy().into_owned(),
            environment: Environment::new(),
            workdir: self.directory.clone(),
            data: self.directory.join("data"),
            directory: self.directory.clone(),
            identity: None,
            start_attempted: true,
            start_confirmed: true,
        }
    }
    fn events(&self) -> Vec<Value> {
        fs::read_to_string(self.directory.join("events"))
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn stops(&self) -> Vec<Value> {
        self.events()
            .into_iter()
            .filter(|event| {
                event
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|value| value == "stop")
            })
            .collect()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[tokio::test]
async fn uncertain_status_attempts_owned_stop_and_cannot_turn_into_success() {
    for primary in [0, 23] {
        let fixture = Fixture::new(&[4, 3], 0, 0, "");
        let mut cluster = fixture.cluster();
        assert_eq!(
            cluster.cleanup(primary).await,
            if primary == 0 { 1 } else { primary }
        );
        assert_eq!(
            fixture.stops(),
            [json!([
                "pg_ctl",
                "-D",
                cluster.data,
                "-w",
                "-t",
                "15",
                "stop",
                "-m",
                "fast"
            ])]
        );
    }
}

#[tokio::test]
async fn failed_fast_stop_attempts_immediate_and_still_preserves_primary_failure() {
    for primary in [0, 37] {
        let fixture = Fixture::new(&[0, 3], 1, 0, "");
        assert_eq!(
            fixture.cluster().cleanup(primary).await,
            if primary == 0 { 1 } else { primary }
        );
        let stops = fixture.stops();
        assert_eq!(stops.len(), 2);
        assert_eq!(stops[0].as_array().unwrap().last().unwrap(), "fast");
        assert_eq!(stops[1].as_array().unwrap().last().unwrap(), "immediate");
    }
}

#[tokio::test]
async fn stop_zero_without_final_not_running_proof_fails() {
    let fixture = Fixture::new(&[0, 0], 0, 0, "");
    assert_eq!(fixture.cluster().cleanup(0).await, 1);
    assert_eq!(fixture.stops().len(), 1);
    let fixture = Fixture::new(&[3], 0, 0, "");
    assert_eq!(fixture.cluster().cleanup(0).await, 0);
    assert!(fixture.stops().is_empty());
}

#[tokio::test]
async fn unsuccessful_dispatched_start_remains_unproven_even_after_status_three() {
    let fixture = Fixture::new(&[3], 0, 0, "");
    let mut cluster = fixture.cluster();
    cluster.start_confirmed = false;
    assert_eq!(cluster.cleanup_observing(0, || false).await, 1);
    let fixture = Fixture::new(&[3, 0, 3], 0, 0, "");
    let mut cluster = fixture.cluster();
    cluster.start_confirmed = false;
    // Registration and iteration admission are count-driven, not elapsed scheduling.
    let mut iterations = 0;
    assert_eq!(
        cluster
            .cleanup_observing(143, || {
                iterations += 1;
                iterations < 2
            })
            .await,
        143
    );
    assert_eq!(fixture.stops().len(), 1);
}

#[tokio::test]
async fn cancellation_during_each_setup_phase_retires_helper_before_owned_cleanup() {
    for phase in ["initdb", "start", "createdb"] {
        let fixture = Fixture::new(&[0, 3], 0, 0, phase);
        let mut cluster = fixture.cluster();
        cluster.start_attempted = false;
        cluster.start_confirmed = false;
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let command = vec!["/this-must-never-run".into()];
        let interrupt = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !fixture.directory.join("ready").is_file() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            sender.send(143).unwrap();
        };
        let (result, ()) = tokio::join!(
            cluster.start_and_run(15434, &command, &mut signals),
            interrupt
        );
        assert_eq!(
            result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
            143
        );
        assert_eq!(
            fs::read_to_string(fixture.directory.join("term")).unwrap(),
            phase
        );
        assert_eq!(cluster.start_attempted, phase != "initdb");
        assert_eq!(cluster.start_confirmed, phase == "createdb");
        assert_eq!(cluster.cleanup_observing(143, || false).await, 143);
        if phase != "initdb" {
            assert_eq!(fixture.stops().len(), 1);
        }
    }
}

#[tokio::test]
async fn interruption_during_cleanup_waits_for_stop_and_cannot_publish_success() {
    for primary in [0, 23] {
        let fixture = Fixture::new(&[0, 3], 0, 0, "stop");
        let mut cluster = fixture.cluster();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let interrupt = async {
            tokio::time::timeout(Duration::from_secs(5), async {
                while !fixture.directory.join("ready").is_file() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            sender.send(143).unwrap();
            fs::write(fixture.directory.join("release"), "finish the owned stop").unwrap();
        };
        let (status, ()) = tokio::join!(cluster.finish(primary, &mut signals), interrupt);
        assert_eq!(status, if primary == 0 { 143 } else { 23 });
        assert_eq!(fixture.stops().len(), 1);
        assert_eq!(
            fixture
                .events()
                .last()
                .unwrap()
                .as_array()
                .unwrap()
                .last()
                .unwrap(),
            "status"
        );
    }
}

#[tokio::test]
async fn captured_environments_stay_independent_and_child_pg_defaults_are_removed() {
    let first = Fixture::new(&[3], 0, 0, "");
    let second = Fixture::new(&[3], 0, 0, "");
    let script = "import json,os;from pathlib import Path;Path('child.json').write_text(json.dumps(dict(os.environ)))";
    let command = vec!["/usr/bin/python3".into(), "-c".into(), script.into()];
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    for (fixture, tag) in [(&first, "first"), (&second, "second")] {
        let mut cluster = fixture.cluster();
        cluster.environment = Environment::from([
            ("KEPT".into(), tag.into()),
            ("PGHOST".into(), "remote.invalid".into()),
            ("PGSERVICE".into(), "production".into()),
            (
                "DATABASE_URL".into(),
                "postgres://remote.invalid/live".into(),
            ),
        ]);
        assert_eq!(
            cluster
                .run_child(
                    &command,
                    "postgres://owned@127.0.0.1/owned_test",
                    &mut signals
                )
                .await
                .unwrap(),
            0
        );
        let report: Value =
            serde_json::from_slice(&fs::read(fixture.directory.join("child.json")).unwrap())
                .unwrap();
        assert_eq!(report["KEPT"], tag);
        assert_eq!(
            report["DATABASE_URL"],
            "postgres://owned@127.0.0.1/owned_test"
        );
        assert_eq!(report["TEST_DATABASE_URL"], report["DATABASE_URL"]);
        assert_eq!(report["DISPOSABLE_TEST_DATABASE"], "1");
        assert!(
            !report
                .as_object()
                .unwrap()
                .keys()
                .any(|name| name.starts_with("PG"))
        );
    }
    let environment = Environment::from([(
        "RUNNER_TEMP".into(),
        first.directory.clone().into_os_string(),
    )]);
    assert_eq!(temp_root(&environment).unwrap(), first.directory);
    assert!(
        temp_root(&Environment::from([(
            "RUNNER_TEMP".into(),
            "relative".into()
        )]))
        .is_err()
    );
}
