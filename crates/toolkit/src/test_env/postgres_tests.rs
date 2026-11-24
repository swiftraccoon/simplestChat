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
        assert!(!block.contains('\''));
        let statuses = statuses.iter().map(i32::to_string).collect::<Vec<_>>();
        fs::write(
            directory.join("config"),
            format!(
                "statuses='{}'\nfast={fast}\nimmediate={immediate}\nblock='{block}'\n",
                statuses.join(" ")
            ),
        )
        .unwrap();
        // `config` sets the answers: each `pg_ctl status` exits with the next
        // of `statuses` (the last one repeats), `pg_ctl stop` with `fast` or,
        // when asked for, `immediate`; the phase named `block` waits.
        let program = r#"root=${0%/*}
tool=${0##*/}
. "$root/config"
printf '%s\n' "$(json_strings "$tool" "$@")" >>"$root/events"
member() {
  member_word=$1
  shift
  for member_item in "$@"; do
    if [ "$member_item" = "$member_word" ]; then return 0; fi
  done
  return 1
}
if [ "$tool" = pg_ctl ] && member start "$@"; then
  phase=start
elif [ "$tool" = pg_ctl ] && member stop "$@"; then
  phase=stop
else
  phase=$tool
fi
if [ "$phase" = "$block" ]; then
  trap 'printf "%s" "$phase" >"$root/term"; exit 0' TERM
  printf '%s' "$phase" >"$root/ready.pending"
  /bin/mv -f "$root/ready.pending" "$root/ready"
  while [ "$phase" != stop ] || [ ! -e "$root/release" ]; do /bin/sleep 0.01; done
fi
if [ "$tool" = pg_ctl ] && member status "$@"; then
  index=0
  if [ -e "$root/counter" ]; then index=$(/bin/cat "$root/counter"); fi
  printf '%s' "$((index + 1))" >"$root/counter"
  position=0
  for status in $statuses; do
    if [ "$position" -eq "$index" ]; then break; fi
    position=$((position + 1))
  done
  exit "$status"
fi
if [ "$tool" = pg_ctl ] && member stop "$@"; then
  if member immediate "$@"; then exit "$immediate"; fi
  exit "$fast"
fi
exit 0
"#;
        let program = format!("#!/bin/sh\n{}{program}", crate::fake_shell::PRELUDE);
        for tool in ["pg_ctl", "initdb", "createdb"] {
            let path = directory.join(tool);
            fs::write(&path, &program).unwrap();
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
    let command = vec![
        "/bin/sh".into(),
        "-c".into(),
        "exec /usr/bin/env >child.environment".into(),
    ];
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
        let printed = fs::read_to_string(fixture.directory.join("child.environment")).unwrap();
        let report = printed
            .lines()
            .map(|line| line.split_once('=').unwrap())
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(report["KEPT"], tag);
        assert_eq!(
            report["DATABASE_URL"],
            "postgres://owned@127.0.0.1/owned_test"
        );
        assert_eq!(report["TEST_DATABASE_URL"], report["DATABASE_URL"]);
        assert_eq!(report["DISPOSABLE_TEST_DATABASE"], "1");
        assert!(!report.keys().any(|name| name.starts_with("PG")));
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
