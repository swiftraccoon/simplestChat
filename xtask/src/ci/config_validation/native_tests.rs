//! The actual metadata adapter with owned inert CLI groups and pipes, never Docker.

use super::*;
use crate::{ci::process::Native, owned_process::Signals};
use std::{os::unix::process::CommandExt, path::Path};

fn executable(fixture: &Fixture, body: &str) -> Environment {
    let program = fixture.root.join("docker");
    fs::write(
        &program,
        format!(
            concat!(
                "#!/usr/bin/python3\nimport os,signal,sys,time\nfrom pathlib import Path\n",
                "root=Path(os.environ['FIXTURE_ROOT'])\n{}\n"
            ),
            body
        ),
    )
    .unwrap();
    fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
    let mut environment = fixture.context.environment.clone();
    environment.insert("PATH".into(), fixture.root.clone().into());
    environment.insert("FIXTURE_ROOT".into(), fixture.root.clone().into());
    environment
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
fn command(maximum: u64) -> Command {
    let mut request = Command::capture(Program::Docker, &["literal $(argument)"]);
    request.capture = Some(maximum);
    request.timeout = Duration::from_secs(3);
    request.grace = Duration::from_millis(20);
    request
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

#[tokio::test]
async fn metadata_readback_follows_normal_exit_descendant_retirement_and_pipe_close() {
    let fixture = Fixture::new(Kind::Caddy);
    let environment = executable(
        &fixture,
        r#"assert sys.argv[1:]==['literal $(argument)']
child=os.fork()
if child==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'worker.pending').write_text(str(os.getpid()))
    (root/'worker.pending').rename(root/'worker')
    while True:time.sleep(.01)
while not (root/'worker').is_file():time.sleep(.005)
os.write(1,b'{"metadata":"closed"}\n')
"#,
    );
    let artifacts = files::Artifacts::new(&fixture.context).unwrap();
    let mut unrelated = std::process::Command::new("/bin/sleep");
    unrelated.arg("30").process_group(0);
    let unrelated = crate::owned_process::Group::new(unrelated.spawn().unwrap());
    let mut native = Native::new(&fixture.root, &environment, signals());
    let bytes = capture::Capture {
        inner: &mut native,
        artifacts: &artifacts,
        started: Instant::now(),
    }
    .run(command(64 * 1024))
    .await
    .unwrap();
    assert_eq!(bytes, b"{\"metadata\":\"closed\"}\n");
    assert!(native.cleanup_passed());
    assert!(!crate::owned_process::exited_now(unrelated.pid()).unwrap());
    let worker = fs::read_to_string(fixture.root.join("worker")).unwrap();
    let observed = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", worker.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8(observed.stdout).unwrap();
    assert!(state.trim().is_empty() || state.trim_start().starts_with('Z'));
    artifacts.finish().unwrap();
    drop(unrelated);
}

const IDLE: &str = r#"signal.signal(signal.SIGTERM,signal.SIG_IGN)
signal.signal(signal.SIGINT,signal.SIG_IGN)
os.write(1,b'PRIVATE_METADATA\n')
(root/'ready.pending').write_text(str(os.getpid()))
(root/'ready.pending').rename(root/'ready')
while True:time.sleep(.01)
"#;

#[tokio::test]
async fn metadata_timeout_and_signals_preserve_primary_after_owned_group_and_log_settlement() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new(Kind::Caddy);
        let environment = executable(&fixture, IDLE);
        let artifacts = files::Artifacts::new(&fixture.context).unwrap();
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Native::new(&fixture.root, &environment, Signals::injected(receiver));
        let mut capture = capture::Capture {
            inner: &mut native,
            artifacts: &artifacts,
            started: Instant::now(),
        };
        let mut request = command(64 * 1024);
        if code == 124 {
            request.timeout = Duration::from_secs(1);
        }
        let cancel = async {
            ready(&fixture.root.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(capture.run(request), cancel);
        let error = result.unwrap_err();
        assert_eq!(error.code, code);
        assert!(!error.message.contains("PRIVATE_METADATA"));
        assert!(native.cleanup_passed());
        assert_eq!(
            fs::read(fixture.artifacts().join("command-000.log")).unwrap(),
            b"PRIVATE_METADATA\n"
        );
        artifacts.finish().unwrap();
    }
}

#[tokio::test]
async fn metadata_overflow_and_spawn_failure_never_produce_a_partial_successful_response() {
    for overflow in [false, true] {
        let fixture = Fixture::new(Kind::Caddy);
        let mut environment = executable(
            &fixture,
            "os.write(1,b'PRIVATE_METADATA'*8192)\nwhile True:time.sleep(.01)",
        );
        if !overflow {
            environment.insert("PATH".into(), fixture.root.join("absent").into());
        }
        let artifacts = files::Artifacts::new(&fixture.context).unwrap();
        let mut native = Native::new(&fixture.root, &environment, signals());
        let error = capture::Capture {
            inner: &mut native,
            artifacts: &artifacts,
            started: Instant::now(),
        }
        .run(command(64 * 1024))
        .await
        .unwrap_err();
        assert_eq!(error.code, if overflow { 1 } else { 127 });
        assert!(!error.message.contains("PRIVATE_METADATA"));
        assert!(native.cleanup_passed());
        assert_eq!(
            fs::metadata(fixture.artifacts().join("command-000.log"))
                .unwrap()
                .len(),
            if overflow { 64 * 1024 } else { 0 }
        );
        artifacts.finish().unwrap();
    }
}
