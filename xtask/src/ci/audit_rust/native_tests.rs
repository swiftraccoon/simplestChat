//! Execute owned Python/scanner fixtures through the shared native supervisor.

use super::*;
use crate::ci::{browser_suite::process::Signals, process::Native};

fn executable(fixture: &mut Fixture, mode: &str) {
    let bin = fixture.root.join("fake commands");
    fs::create_dir(&bin).unwrap();
    let source = r#"#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT']);mode=os.environ['FIXTURE_MODE']
args=sys.argv[1:]
assert args[0]=='build/security_tools.py'
stage='install' if args[1]=='install' else 'resolve'
with (root/'events').open('a') as out:out.write(json.dumps([stage,args])+'\n')
if mode==stage+'-fail':
    print('PRIVATE-SENTINEL',file=sys.stderr);sys.exit(42)
if mode==stage+'-hang':
    signal.signal(signal.SIGINT,signal.SIG_IGN);signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'ready').write_text(str(os.getpid()))
    while True:time.sleep(.01)
if stage=='install':
    assert args[1:-1]==['install','--tools','cargo-audit','--directory']
    tools=Path(args[-1]);(tools/'bin').mkdir(parents=True)
    scanner=tools/'bin/cargo-audit'
    scanner.write_text('''#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT']);mode=os.environ['FIXTURE_MODE']
assert sys.argv[1:]==['audit','--deny','unsound','--ignore','RUSTSEC-2021-0145','--ignore','RUSTSEC-2026-0097']
(root/'audit-started').write_text('owned fixture only')
if mode=='audit-hang':
    signal.signal(signal.SIGINT,signal.SIG_IGN);signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'ready').write_text(str(os.getpid()))
    while True:time.sleep(.01)
if mode=='audit-fail':sys.exit(17)
print('Owned fixture advisory output')
''')
    scanner.chmod(0o700)
else:
    assert args[1:-1]==['path','cargo-audit','--directory']
    if mode=='resolve-overflow':print('x'*4097)
    else:print(str(Path(args[-1])/'bin/cargo-audit'))
"#;
    let path = bin.join("python3");
    fs::write(&path, source).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    for (key, value) in [
        ("PATH", bin.into_os_string()),
        ("FIXTURE_ROOT", fixture.root.clone().into_os_string()),
        ("FIXTURE_MODE", mode.into()),
    ] {
        fixture.context.environment.insert(key.into(), value);
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn actual_installer_and_selected_scanner_fixtures_preserve_literal_arguments_and_output_mode()
{
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
    execute::execute(&fixture.context, &mut native)
        .await
        .unwrap();
    assert!(fixture.root.join("audit-started").is_file());
    assert_eq!(
        fs::read_to_string(fixture.root.join("events"))
            .unwrap()
            .lines()
            .count(),
        2
    );
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["passed"], true);
    assert!(!fixture.root.join("literal").exists());
}

#[tokio::test]
async fn actual_reference_and_scanner_failure_statuses_and_output_overflow_are_not_hidden() {
    for (mode, code, stage) in [
        ("install-fail", 42, "install"),
        ("resolve-fail", 42, "resolve"),
        ("audit-fail", 17, "audit"),
        ("resolve-overflow", 1, "resolve"),
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, mode);
        let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
        let error = execute::execute(&fixture.context, &mut native)
            .await
            .unwrap_err();
        assert_eq!(error.code, code, "{mode}");
        assert!(!error.message.contains("PRIVATE-SENTINEL"));
        assert!(fixture.staging().is_empty());
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["failure"], stage);
        assert_eq!(report["cleanupPassed"], true);
    }
}

struct Short<'a>(Native<'a>);
impl Executor for Short<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_secs(2);
        command.grace = Duration::from_millis(30);
        self.0.run(command).await
    }
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
async fn interrupt_and_timeout_reap_owned_reference_or_audit_groups_before_namespace_retirement() {
    for (code, mode) in [
        (124, "install-hang"),
        (130, "audit-hang"),
        (143, "audit-hang"),
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, mode);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &fixture.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        ));
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(execute::execute(&fixture.context, &mut native), cancel);
        assert_eq!(result.unwrap_err().code, code);
        let report = fixture.report();
        assert_eq!(report["passed"], false);
        assert_eq!(report["exitCode"], code);
        assert_eq!(report["cleanupPassed"], true);
        assert!(fixture.staging().is_empty());
        let pid: i32 = fs::read_to_string(fixture.root.join("ready"))
            .unwrap()
            .parse()
            .unwrap();
        assert!(
            rustix::process::test_kill_process(rustix::process::Pid::from_raw(pid).unwrap())
                .is_err()
        );
    }
}

struct Changed<'a>(Native<'a>);
impl Executor for Changed<'_> {
    fn cleanup_passed(&self) -> bool {
        self.0.cleanup_passed()
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
    async fn run(&mut self, command: Command) -> Result<Vec<u8>> {
        if command.program == Program::CargoAudit {
            let path = command
                .executable
                .as_ref()
                .unwrap()
                .validate()
                .unwrap()
                .to_owned();
            fs::write(path, b"PRIVATE-SENTINEL").unwrap();
        }
        self.0.run(command).await
    }
}
#[tokio::test]
async fn native_rechecks_selected_inode_immediately_before_spawn_without_path_fallback() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Changed(Native::new(
        &fixture.root,
        &fixture.context.environment,
        signals(),
    ));
    assert!(
        execute::execute(&fixture.context, &mut native)
            .await
            .is_err()
    );
    assert!(!fixture.root.join("audit-started").exists());
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["failure"], "audit");
}

#[tokio::test]
async fn missing_or_wrong_role_proof_and_prior_cancellation_create_no_child() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Native::new(&fixture.root, &fixture.context.environment, signals());
    let error = native
        .run(Command::capture(Program::CargoAudit, &["audit"]))
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    let owned = files::Owned::new(&fixture.context).unwrap();
    let path = owned.path.join("tools/bin/cargo-audit");
    scanner(&path);
    let selected = Arc::new(
        Executable::open(
            &owned.path,
            owned.descriptor().unwrap(),
            path.as_os_str().as_encoded_bytes(),
            &mut true,
        )
        .unwrap(),
    );
    let mut command = Command::capture(Program::Python, &["--version"]);
    command.executable = Some(Arc::clone(&selected));
    assert!(native.run(command).await.is_err());
    Arc::try_unwrap(selected).unwrap().close().unwrap();
    let (reporter, cleanup) = owned.cleanup();
    cleanup.unwrap();
    reporter
        .write(&serde_json::json!({"passed":false}))
        .unwrap();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(143).unwrap();
    let mut native = Native::new(
        &fixture.root,
        &fixture.context.environment,
        Signals::injected(receiver),
    );
    assert_eq!(
        execute::execute(&fixture.context, &mut native)
            .await
            .unwrap_err()
            .code,
        143
    );
    assert!(!fixture.root.join("events").exists());
    assert!(fixture.staging().is_empty());
}
