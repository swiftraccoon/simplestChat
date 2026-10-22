//! Inert fork fixtures for service ownership; no listeners or backend commands.

use super::*;
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "test service owner {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }

    fn spawn(&self, mode: &str, status: i32) -> Service {
        let mut command = Command::new("/usr/bin/python3");
        command.args([
            "-c",
            r#"
import json,os,signal,sys,time
from pathlib import Path
root=Path(sys.argv[1]);mode=sys.argv[2];status=int(sys.argv[3])
def record(name,contents):
    pending=root/(name+'.pending')
    pending.write_text(contents)
    pending.replace(root/name)
worker=os.fork()
if worker==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    record('worker',json.dumps({'pid':os.getpid(),'group':os.getpgrp()}))
    while True:time.sleep(.01)
def term(*_):
    record('term','TERM received')
    if mode=='cooperative':sys.exit(status)
    while not (root/'release').exists():time.sleep(.01)
if mode in ('cooperative','pending'):signal.signal(signal.SIGTERM,term)
if mode=='stubborn':signal.signal(signal.SIGTERM,signal.SIG_IGN)
while not (root/'worker').exists():time.sleep(.01)
record('leader',json.dumps({'pid':os.getpid(),'group':os.getpgrp()}))
while not (root/'release').exists():time.sleep(.01)
sys.exit(status)
"#,
        ]);
        command
            .arg(&self.0)
            .arg(mode)
            .arg(status.to_string())
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        Service::spawn(command).unwrap()
    }

    fn pid(&self, name: &str) -> u32 {
        serde_json::from_slice::<serde_json::Value>(&fs::read(self.0.join(name)).unwrap()).unwrap()
            ["pid"]
            .as_u64()
            .unwrap()
            .try_into()
            .unwrap()
    }

    fn release(&self) {
        fs::write(self.0.join("release"), b"release leader").unwrap();
    }

    fn same_owned_group(&self, service: &Service) {
        let leader: serde_json::Value =
            serde_json::from_slice(&fs::read(self.0.join("leader")).unwrap()).unwrap();
        let worker: serde_json::Value =
            serde_json::from_slice(&fs::read(self.0.join("worker")).unwrap()).unwrap();
        assert_eq!(leader["pid"], leader["group"]);
        assert_eq!(leader["group"], worker["group"]);
        assert_eq!(service.pid(), self.pid("leader"));
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
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

async fn ended(service: &Service) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !service.exited().unwrap() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

async fn retired(pid: u32) {
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
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

fn unrelated() -> Service {
    let mut command = Command::new("/usr/bin/python3");
    command
        .args(["-c", "import time;time.sleep(30)"])
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    Service::spawn(command).unwrap()
}

#[tokio::test]
async fn repeated_nowait_observation_keeps_descendant_cleanup_owned_after_natural_exit() {
    for status in [0, 23] {
        let fixture = Fixture::new();
        let service = fixture.spawn("natural", status);
        let other = unrelated();
        ready(&fixture.0.join("leader")).await;
        fixture.same_owned_group(&service);
        assert!(!service.exited().unwrap());
        fixture.release();
        ended(&service).await;
        assert!(service.exited().unwrap());
        assert!(service.exited().unwrap());
        let record = service.stop(Duration::from_secs(1)).await;
        assert_eq!(record.wait_status, Some(status));
        assert!(record.group_cleanup_passed);
        assert!(!record.term_attempted);
        assert!(!record.term_sent);
        assert!(!record.kill_attempted);
        assert!(!record.kill_sent);
        retired(fixture.pid("worker")).await;
        retired(fixture.pid("leader")).await;
        assert!(!other.exited().unwrap());
    }
}

#[tokio::test]
async fn cooperative_leader_exit_still_retires_stubborn_descendants_without_forced_leader_flags() {
    let fixture = Fixture::new();
    let service = fixture.spawn("cooperative", 0);
    let other = unrelated();
    ready(&fixture.0.join("leader")).await;
    fixture.same_owned_group(&service);
    let record = service.stop(Duration::from_secs(2)).await;
    ready(&fixture.0.join("term")).await;
    assert!(record.term_attempted);
    assert!(record.term_sent);
    assert!(!record.kill_attempted);
    assert!(!record.kill_sent);
    assert_eq!(record.wait_status, Some(0));
    assert!(record.group_cleanup_passed);
    retired(fixture.pid("worker")).await;
    assert!(!other.exited().unwrap());
}

#[tokio::test]
async fn leader_grace_expiry_records_actual_escalation_and_retires_only_the_owned_group() {
    let fixture = Fixture::new();
    let service = fixture.spawn("stubborn", 0);
    let other = unrelated();
    ready(&fixture.0.join("leader")).await;
    fixture.same_owned_group(&service);
    let record = service.stop(Duration::from_millis(20)).await;
    assert!(record.term_attempted);
    assert!(record.term_sent);
    assert!(record.kill_attempted);
    assert!(record.kill_sent);
    assert_eq!(record.wait_status, Some(137));
    assert!(record.group_cleanup_passed);
    retired(fixture.pid("worker")).await;
    retired(fixture.pid("leader")).await;
    assert!(!other.exited().unwrap());
}

#[tokio::test]
async fn dropping_a_service_retires_live_and_naturally_completed_groups() {
    for complete in [false, true] {
        let fixture = Fixture::new();
        let service = fixture.spawn("natural", 0);
        let other = unrelated();
        ready(&fixture.0.join("leader")).await;
        if complete {
            fixture.release();
            ended(&service).await;
        }
        drop(service);
        retired(fixture.pid("worker")).await;
        retired(fixture.pid("leader")).await;
        assert!(!other.exited().unwrap());
    }
}

#[tokio::test]
async fn cancelling_the_stopping_future_keeps_the_group_guard_armed() {
    let fixture = Fixture::new();
    let service = fixture.spawn("pending", 0);
    let other = unrelated();
    ready(&fixture.0.join("leader")).await;
    let task = tokio::spawn(service.stop(Duration::from_secs(30)));
    ready(&fixture.0.join("term")).await;
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    retired(fixture.pid("worker")).await;
    retired(fixture.pid("leader")).await;
    assert!(!other.exited().unwrap());
}

#[tokio::test]
async fn unproven_final_cleanup_is_not_hidden_by_a_zero_leader_status() {
    let fixture = Fixture::new();
    let service = fixture.spawn("natural", 0);
    ready(&fixture.0.join("leader")).await;
    fixture.release();
    ended(&service).await;
    let record = service
        .stop_with_cleanup(Duration::from_secs(1), |pid| async move {
            assert!(owned_process::finish(pid).await);
            false
        })
        .await;
    assert_eq!(record.wait_status, Some(0));
    assert!(!record.group_cleanup_passed);
    assert!(!record.kill_attempted);
    retired(fixture.pid("worker")).await;
}

#[test]
fn spawn_failure_is_fixed_and_never_creates_a_service() {
    let fixture = Fixture::new();
    let command = Command::new(fixture.0.join("PRIVATE-MISSING-CANDIDATE"));
    let error = Service::spawn(command).err().unwrap();
    assert_eq!(error.to_string(), "Cannot start the owned test service.");
    assert!(!fixture.0.join("leader").exists());
}
