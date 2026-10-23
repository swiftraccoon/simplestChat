//! Actual inert Node/Python fixtures through the shared reserved-group owner.

use super::*;
use crate::ci::{browser_suite::process::Signals, process::Native};

fn commands(f: &mut Fixture, mode: &str) {
    let node = r#"#!/usr/bin/python3
import os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT']);mode=os.environ['FIXTURE_MODE']
assert sys.argv[1:]==['-e','process.stdout.write(require("./web/e2e/node_modules/playwright").chromium.executablePath())']
(root/'node-ready').write_text(str(os.getpid()))
if mode=='node-fail':sys.exit(42)
if mode=='node-hang':
    signal.signal(signal.SIGTERM,signal.SIG_IGN);signal.signal(signal.SIGINT,signal.SIG_IGN)
    while True:time.sleep(.01)
sys.stdout.write('x'*4097 if mode=='overflow' else str(root/'browser with spaces'))
"#;
    executable(&f.node, node.as_bytes());
    let python = r#"#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT']);mode=os.environ['FIXTURE_MODE'];tmp=root/'runner temp'
expected=['-B','build/ci_release_checks.py','--controller',str(tmp/'release-container-controller/bin/python'),'--node',str(root/'commands/node'),'--chromium',str(root/'browser with spaces'),'--http-policy-binary',str(root/'feature xtask'),'--image','sha256:'+'a'*64,'--fixture-output',str(tmp/'release-container'),'--output',str(tmp/'release-checks')]
assert sys.argv[1:]==expected
cancelled=False
def cancel(*_):
    global cancelled
    cancelled=True
if mode=='cancel-cleanup':
    signal.signal(signal.SIGTERM,cancel);signal.signal(signal.SIGINT,cancel)
(root/'coordinator-ready').write_text(str(os.getpid()))
if mode=='coordinator-fail':sys.exit(17)
if mode=='cancel-cleanup':
    while not cancelled:time.sleep(.01)
    time.sleep(.05);(root/'coordinator-cleaned').write_text('owned fixture cleanup')
print('Owned release coordinator fixture only')
"#;
    executable(&f.node.parent().unwrap().join("python3"), python.as_bytes());
    for (key, value) in [
        ("FIXTURE_ROOT", f.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
    ] {
        f.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn actual_fake_discovery_and_coordinator_keep_literal_arguments_and_failure_statuses() {
    for (mode, expected) in [
        ("", 0),
        ("node-fail", 42),
        ("coordinator-fail", 17),
        ("overflow", 1),
    ] {
        let mut f = Fixture::new();
        commands(&mut f, mode);
        let mut native = Native::new(&f.root, &f.context.environment, signals());
        let code = execute::execute(&f.context, &mut native)
            .await
            .map_or_else(|error| error.code, |()| 0);
        assert_eq!(code, expected, "{mode}");
        assert!(native.cleanup_passed());
        assert_eq!(
            f.root.join("coordinator-ready").exists(),
            mode.is_empty() || mode == "coordinator-fail"
        );
        assert!(!f.root.join("literal").exists());
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_millis(200);
        command.grace = Duration::from_millis(200);
        self.0.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
}

#[tokio::test]
async fn node_timeout_and_cancellation_never_start_the_coordinator_and_reap_discovery() {
    for code in [124, 130, 143] {
        let mut f = Fixture::new();
        commands(&mut f, "node-hang");
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &f.root,
            &f.context.environment,
            Signals::injected(receiver),
        ));
        let cancel = async {
            ready(&f.root.join("node-ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(execute::execute(&f.context, &mut native), cancel);
        assert_eq!(result.unwrap_err().code, code);
        assert!(native.cleanup_passed());
        assert!(!f.root.join("coordinator-ready").exists());
        let pid = fs::read_to_string(f.root.join("node-ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}

#[tokio::test]
async fn cancellation_allows_owned_coordinator_finalization_without_passing_or_touching_unrelated_process()
 {
    let mut f = Fixture::new();
    commands(&mut f, "cancel-cleanup");
    struct Unrelated(std::process::Child);
    impl Drop for Unrelated {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut unrelated = Unrelated(
        std::process::Command::new("/usr/bin/python3")
            .args(["-c", "import time;time.sleep(30)"])
            .spawn()
            .unwrap(),
    );
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&f.root, &f.context.environment, Signals::injected(receiver));
    let cancel = async {
        ready(&f.root.join("coordinator-ready")).await;
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(execute::execute(&f.context, &mut native), cancel);
    let alive = unrelated.0.try_wait().unwrap().is_none();
    drop(unrelated);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(native.cleanup_passed());
    assert!(alive);
    assert!(f.root.join("coordinator-cleaned").exists());
}

#[tokio::test]
async fn node_role_proof_never_falls_back_to_path_or_another_program() {
    let mut f = Fixture::new();
    commands(&mut f, "");
    let mut native = Native::new(&f.root, &f.context.environment, signals());
    assert!(
        native
            .run(Command::capture(Program::Node, &["-e", plan::EXPRESSION]))
            .await
            .is_err()
    );
    let proof = Arc::new(files::node(&f.root, &f.context.environment).unwrap());
    let mut request = Command::capture(Program::Python, &[]);
    request.node = Some(Arc::clone(&proof));
    assert!(native.run(request).await.is_err());
    Arc::try_unwrap(proof).unwrap().close().unwrap();
    assert!(!f.root.join("node-ready").exists());
    assert!(!f.root.join("coordinator-ready").exists());
}
