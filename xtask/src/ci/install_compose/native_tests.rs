//! The shared owner runs fixture executables in a PATH containing no real tools.

use super::*;
use crate::ci::{browser_suite::process::Signals, process::Native};
use std::os::unix::fs::PermissionsExt;

fn executable(fixture: &mut Fixture, mode: &str) {
    let directory = fixture.root.join("fake commands");
    fs::create_dir(&directory).unwrap();
    let source = r#"#!/usr/bin/python3
import json,os,signal,sys,time
from pathlib import Path
root=Path(os.environ['FIXTURE_ROOT'])
args=sys.argv[1:]
program=Path(sys.argv[0]).name
stage='download' if program=='curl' else 'caller_version' if program=='docker' else 'root_version' if args[0]=='env' else 'install_directory' if args[1]=='-d' else 'install_file'
mode=os.environ['FIXTURE_MODE']
with (root/'events').open('a') as out: out.write(json.dumps([program,args,stage])+'\n')
if mode==stage+'-fail':
    print('PRIVATE-SENTINEL',file=sys.stderr);sys.exit(42)
if stage=='install_file':
    assert args[:3]==['install','-m','0755']
    assert args[3].endswith('/verified') and Path(args[3]).read_bytes()==b'owned fixture compose\n'
    assert args[4]=='/usr/local/lib/docker/cli-plugins/docker-compose'
    (root/'installed-fixture').write_text('owned fixture only')
if mode==stage+'-hang':
    signal.signal(signal.SIGINT,signal.SIG_IGN);signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'ready').write_text(str(os.getpid()))
    while True: time.sleep(.01)
if stage=='download':
    assert '--output' not in args
    if mode=='swap-download':
        prefix=next(path for path in (root/'runner temp').iterdir() if path.is_dir())
        target=root/'private-target';target.write_bytes(b'foreign sentinel')
        (prefix/'download').unlink();(prefix/'download').symlink_to(target)
    sys.stdout.buffer.write(b'owned fixture compose\n')
elif stage=='install_directory':
    assert args==['install','-d','-m','0755','/usr/local/lib/docker/cli-plugins']
elif stage=='caller_version':
    assert args==['compose','version','--short'];print('5.6.0')
elif stage=='root_version':
    assert args==['env','-i','PATH=/usr/sbin:/usr/bin:/sbin:/bin','LC_ALL=C','timeout','--signal=TERM','--kill-after=2s','10s','/usr/bin/docker','--host','unix:///var/run/docker.sock','compose','version','--short']
    print('5.6.0')
"#;
    for name in ["curl", "sudo", "docker"] {
        let path = directory.join(name);
        fs::write(&path, source).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fixture
        .context
        .environment
        .insert("PATH".into(), directory.into_os_string());
    fixture
        .context
        .environment
        .insert("FIXTURE_ROOT".into(), fixture.root.clone().into_os_string());
    fixture
        .context
        .environment
        .insert("FIXTURE_MODE".into(), mode.into());
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}

#[tokio::test]
async fn actual_fixture_processes_authenticate_owned_stdout_before_both_literal_version_checks() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    execute::install(&fixture.context, &mut native)
        .await
        .unwrap();
    assert_eq!(
        fs::read_to_string(fixture.root.join("events"))
            .unwrap()
            .lines()
            .count(),
        5
    );
    assert!(fixture.root.join("installed-fixture").is_file());
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["passed"], true);
    assert!(!fixture.root.join("literal").exists());
}

#[tokio::test]
async fn download_path_swap_cannot_redirect_a_single_byte_of_curl_output() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "swap-download");
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        signals(),
    );
    assert!(
        execute::install(&fixture.context, &mut native)
            .await
            .is_err()
    );
    assert_eq!(
        fs::read(fixture.root.join("private-target")).unwrap(),
        b"foreign sentinel"
    );
    assert!(!fixture.root.join("installed-fixture").exists());
    assert!(fixture.staging().is_empty());
    assert_eq!(fixture.report()["failure"], "authenticate");
}

#[tokio::test]
async fn process_failures_never_publish_success_or_expose_captured_errors() {
    for stage in [
        "download",
        "install_directory",
        "install_file",
        "caller_version",
        "root_version",
    ] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, &format!("{stage}-fail"));
        let mut native = Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            signals(),
        );
        let error = execute::install(&fixture.context, &mut native)
            .await
            .unwrap_err();
        assert_eq!(error.code, 42);
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
    async fn run(&mut self, mut command: Command) -> Result<Vec<u8>> {
        command.timeout = Duration::from_secs(2);
        command.grace = Duration::from_millis(30);
        self.0.run(command).await
    }
    async fn ready(&mut self) -> Result<()> {
        self.0.ready().await
    }
}
async fn ready(path: &std::path::Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn interrupt_and_timeout_cleanup_preserve_truth_after_uncertain_privileged_install() {
    for (code, stage) in [(124, "download"), (130, "download"), (143, "install_file")] {
        let mut fixture = Fixture::new();
        executable(&mut fixture, &format!("{stage}-hang"));
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Short(Native::new(
            &fixture.context.root,
            &fixture.context.environment,
            Signals::injected(receiver),
        ));
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(execute::install(&fixture.context, &mut native), cancel);
        assert_eq!(result.unwrap_err().code, code);
        assert!(fixture.staging().is_empty());
        let report = fixture.report();
        assert_eq!(report["exitCode"], code);
        assert_eq!(report["cleanupPassed"], true);
        assert_eq!(report["passed"], false);
        if stage == "install_file" {
            assert!(fixture.root.join("installed-fixture").exists());
            assert_eq!(report["installationAttempted"], true);
            assert_eq!(report["installationCompleted"], false);
        }
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

#[tokio::test]
async fn already_cancelled_installer_spawns_nothing_and_creates_no_owned_resources() {
    let mut fixture = Fixture::new();
    executable(&mut fixture, "");
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    sender.send(143).unwrap();
    let mut native = Native::new(
        &fixture.context.root,
        &fixture.context.environment,
        Signals::injected(receiver),
    );
    assert_eq!(
        execute::install(&fixture.context, &mut native)
            .await
            .unwrap_err()
            .code,
        143
    );
    assert!(!fixture.root.join("events").exists());
    assert!(fixture.staging().is_empty());
}
