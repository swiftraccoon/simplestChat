//! Inert fork fixtures with explicit release-file handshakes, never services.

use super::*;
use crate::owned_process::{self, Group, Signals};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture {
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "test child owner {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self { root }
    }
    fn command(&self, status: i32, cancel: bool) -> std::process::Command {
        let mut command = std::process::Command::new("/usr/bin/python3");
        command.args([
            "-c",
            r#"
import json,os,signal,sys,time
from pathlib import Path
root=Path(sys.argv[1]);status=int(sys.argv[2]);cancel=sys.argv[3]=='true'
def record(name,contents):
    pending=root/(name+'.pending')
    pending.write_text(contents)
    pending.replace(root/name)
child=os.fork()
if child==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    record('worker',json.dumps({'pid':os.getpid(),'group':os.getpgrp()}))
    while True:time.sleep(.01)
def terminate(*_): record('term','TERM received')
if cancel:signal.signal(signal.SIGTERM,terminate)
while not (root/'worker').exists():time.sleep(.01)
record('leader',json.dumps({'pid':os.getpid(),'group':os.getpgrp()}))
while not (root/'release').exists():time.sleep(.01)
sys.exit(status)
"#,
        ]);
        command
            .arg(&self.root)
            .arg(status.to_string())
            .arg(cancel.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }
    fn pid(&self, name: &str) -> i32 {
        serde_json::from_slice::<serde_json::Value>(&fs::read(self.root.join(name)).unwrap())
            .unwrap()["pid"]
            .as_i64()
            .unwrap()
            .try_into()
            .unwrap()
    }
    fn release(&self) {
        fs::write(self.root.join("release"), b"release leader").unwrap();
    }
    fn same_owned_group(&self) {
        let leader: serde_json::Value =
            serde_json::from_slice(&fs::read(self.root.join("leader")).unwrap()).unwrap();
        let worker: serde_json::Value =
            serde_json::from_slice(&fs::read(self.root.join("worker")).unwrap()).unwrap();
        assert_eq!(leader["pid"], leader["group"]);
        assert_eq!(leader["group"], worker["group"]);
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
async fn retired(pid: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let output = tokio::process::Command::new("/bin/ps")
                .args(["-p", &pid.to_string(), "-o", "stat="])
                .env_clear()
                .env("LC_ALL", "C")
                .stdin(Stdio::null())
                .kill_on_drop(true)
                .output()
                .await
                .unwrap();
            let state = String::from_utf8(output.stdout).unwrap();
            if (!output.status.success() && state.trim().is_empty())
                || state.trim().starts_with('Z')
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}
fn unrelated() -> Group {
    use std::os::unix::process::CommandExt;
    let child = std::process::Command::new("/usr/bin/python3")
        .args(["-c", "import time;time.sleep(30)"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    Group::new(child)
}

#[tokio::test]
async fn ordinary_completion_retires_same_group_descendants_before_reaping_leader() {
    for status in [0, 23] {
        let f = Fixture::new();
        let other = unrelated();
        let mut signals = signals();
        let release = async {
            ready(&f.root.join("leader")).await;
            f.same_owned_group();
            f.release();
        };
        let (result, ()) = tokio::join!(
            run_test_child_with_cleanup(
                f.command(status, false),
                Duration::from_millis(100),
                &mut signals,
                owned_process::finish
            ),
            release
        );
        assert_eq!(result.unwrap(), status);
        retired(f.pid("worker")).await;
        retired(f.pid("leader")).await;
        assert!(rustix::process::test_kill_process(other.pid()).is_ok());
    }
}

#[tokio::test]
async fn interruption_preserves_shell_status_and_finishes_descendants_after_leader_cleanup() {
    for code in [130, 143] {
        let f = Fixture::new();
        let other = unrelated();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let cancel = async {
            ready(&f.root.join("leader")).await;
            f.same_owned_group();
            sender.send(code).unwrap();
            ready(&f.root.join("term")).await;
            f.release();
        };
        let (result, ()) = tokio::join!(
            run_test_child_with_cleanup(
                f.command(0, true),
                Duration::from_secs(2),
                &mut signals,
                owned_process::finish
            ),
            cancel
        );
        assert_eq!(result.unwrap(), i32::from(code));
        retired(f.pid("worker")).await;
        retired(f.pid("leader")).await;
        assert!(rustix::process::test_kill_process(other.pid()).is_ok());
    }
}

#[tokio::test]
async fn unproven_group_cleanup_cannot_pass_or_replace_an_existing_failure() {
    for (status, expected) in [(0, 1), (23, 23)] {
        let f = Fixture::new();
        let mut signals = signals();
        let release = async {
            ready(&f.root.join("leader")).await;
            f.release();
        };
        let unproven = |pid| async move {
            assert!(owned_process::finish(pid).await);
            false
        };
        let (result, ()) = tokio::join!(
            run_test_child_with_cleanup(
                f.command(status, false),
                Duration::from_millis(100),
                &mut signals,
                unproven
            ),
            release
        );
        assert_eq!(result.unwrap(), expected);
        retired(f.pid("worker")).await;
    }
}

#[tokio::test]
async fn dropping_supervision_future_finishes_the_still_reserved_owned_group() {
    let f = Fixture::new();
    let other = unrelated();
    let mut signals = signals();
    let command = f.command(0, false);
    let task = tokio::spawn(async move {
        run_test_child_with_cleanup(
            command,
            Duration::from_millis(100),
            &mut signals,
            owned_process::finish,
        )
        .await
    });
    ready(&f.root.join("leader")).await;
    f.same_owned_group();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    retired(f.pid("worker")).await;
    retired(f.pid("leader")).await;
    assert!(rustix::process::test_kill_process(other.pid()).is_ok());
}

#[tokio::test]
async fn prior_cancellation_and_spawn_failure_never_create_owned_processes() {
    let f = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(143).unwrap();
    let mut signals = Signals::injected(receiver);
    assert_eq!(
        run_test_child_with_cleanup(
            f.command(0, false),
            Duration::from_millis(100),
            &mut signals,
            owned_process::finish
        )
        .await
        .unwrap(),
        143
    );
    assert!(!f.root.join("leader").exists());
    let mut signals = super::tests::signals();
    assert!(
        run_test_child_with_cleanup(
            std::process::Command::new(f.root.join("missing")),
            Duration::from_millis(100),
            &mut signals,
            owned_process::finish
        )
        .await
        .is_err()
    );
}

#[tokio::test]
async fn cancellation_during_final_group_cleanup_cannot_publish_a_successful_command() {
    let f = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let release = async {
        ready(&f.root.join("leader")).await;
        f.release();
    };
    let finish = |pid| async move {
        let cleaned = owned_process::finish(pid).await;
        sender.send(143).unwrap();
        cleaned
    };
    let (result, ()) = tokio::join!(
        run_test_child_with_cleanup(
            f.command(0, false),
            Duration::from_millis(100),
            &mut signals,
            finish,
        ),
        release
    );
    assert_eq!(result.unwrap(), 143);
    retired(f.pid("worker")).await;
}
