//! Owned inert CLIs exercise the same setup/readiness helper as real tools.

use super::*;
use crate::owned_process::Signals;
use std::{
    fs,
    path::{Path, PathBuf},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        Self(
            super::super::postgres::create_private_directory(
                &std::env::temp_dir(),
                "helper-owner.",
            )
            .unwrap(),
        )
    }
    fn command(&self, script: &str) -> Command {
        let mut command = Command::new("/usr/bin/python3");
        command
            .arg("-c")
            .arg(script)
            .arg(&self.0)
            .stderr(Stdio::null());
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

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
async fn captures_bounded_output_and_preserves_nonzero_exit() {
    let fixture = Fixture::new();
    let output = run(
        fixture.command("import sys;print('fixed');sys.exit(23)"),
        Duration::from_secs(5),
        Some(64),
        None,
    )
    .await
    .unwrap();
    assert_eq!(output.status, 23);
    assert_eq!(output.stdout, b"fixed\n");
    let error = run(
        fixture.command("print('x'*65)"),
        Duration::from_secs(5),
        Some(64),
        None,
    )
    .await
    .err()
    .unwrap();
    assert!(error.to_string().contains("exceeded its bound"));
}

#[tokio::test]
async fn startup_interruption_retires_helper_descendants_and_preserves_status() {
    let fixture = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    let command = fixture.command(
        r#"
import os,signal,sys,time
from pathlib import Path
root=Path(sys.argv[1])
def record(name,value):
 p=root/(name+'.pending');p.write_text(str(value));p.replace(root/name)
child=os.fork()
if child==0:
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 record('worker',os.getpid())
 while True:time.sleep(.01)
def finish(*_): record('term','observed');sys.exit(0)
signal.signal(signal.SIGTERM,finish)
while not (root/'worker').exists():time.sleep(.01)
record('ready',os.getpid())
while True:time.sleep(.01)
"#,
    );
    let interrupt = async {
        ready(&fixture.0.join("ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(
        run(command, Duration::from_secs(10), None, Some(&mut signals)),
        interrupt
    );
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        143
    );
    assert!(fixture.0.join("term").is_file());
    let worker = fs::read_to_string(fixture.0.join("worker")).unwrap();
    let worker = rustix::process::Pid::from_raw(worker.parse().unwrap()).unwrap();
    let gone = rustix::process::test_kill_process(worker).is_err();
    // A grandchild can remain a zombie until the system adopter reaps it.
    if !gone {
        let status = Command::new("/bin/ps")
            .args([
                "-p",
                &worker.as_raw_nonzero().get().to_string(),
                "-o",
                "stat=",
            ])
            .output()
            .unwrap();
        assert!(
            String::from_utf8(status.stdout)
                .unwrap()
                .trim()
                .chars()
                .next()
                .is_none_or(|state| state == 'Z')
        );
    }
}

#[tokio::test]
async fn prior_cancellation_does_not_acquire_a_helper() {
    let fixture = Fixture::new();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(130).unwrap();
    let mut signals = Signals::injected(receiver);
    let result = run(
        fixture.command("import sys;from pathlib import Path;Path(sys.argv[1],'started').touch()"),
        Duration::from_secs(5),
        None,
        Some(&mut signals),
    )
    .await;
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        130
    );
    assert!(!fixture.0.join("started").exists());
}

#[tokio::test]
async fn cancellable_in_memory_readiness_does_not_wait_for_its_deadline() {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut signals = Signals::injected(receiver);
    sender.send(130).unwrap();
    let result = wait(&mut signals, std::future::pending::<()>()).await;
    assert_eq!(
        result.err().unwrap().downcast_ref::<Stop>().unwrap().code,
        130
    );
}

#[tokio::test]
async fn zero_exited_leader_retires_stdout_holding_worker_before_final_read() {
    let fixture = Fixture::new();
    let command = fixture.command(
        r#"
import os,signal,sys,time
from pathlib import Path
root=Path(sys.argv[1])
if os.fork()==0:
 signal.signal(signal.SIGTERM,signal.SIG_IGN)
 pending=root/'worker.pending';pending.write_text(str(os.getpid()));pending.replace(root/'worker')
 while True:time.sleep(.01)
while not (root/'worker').exists():time.sleep(.01)
print('closed',flush=True)
sys.exit(0)
"#,
    );
    let output = run(command, Duration::from_secs(5), Some(64), None)
        .await
        .unwrap();
    assert_eq!(output.status, 0);
    assert_eq!(output.stdout, b"closed\n");
    assert!(fixture.0.join("worker").is_file());
}
