//! Hook byte oracles and inert owned Git stand-ins, never external repositories.

use super::*;
use std::{
    fs,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::PathBuf,
    process::{Command as Process, Stdio},
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "commit subject $(literal) {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self(root)
    }
    fn environment(&self) -> Environment {
        Environment::from([
            ("PATH".into(), self.0.clone().into_os_string()),
            ("COMMIT_REVISION".into(), "a".repeat(40).into()),
        ])
    }
    fn git(&self, body: &str) {
        let file = self.0.join("git");
        fs::write(&file, format!("#!/usr/bin/python3\n{body}")).unwrap();
        fs::set_permissions(file, fs::Permissions::from_mode(0o700)).unwrap();
    }
    fn hook(&self, bytes: &[u8]) -> bool {
        let file = self.0.join("message");
        fs::write(&file, bytes).unwrap();
        let hook = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .join(".githooks/commit-msg");
        let status = Process::new(hook)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        assert!(matches!(status.code(), Some(0 | 1)));
        status.success()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_byte_policy_matches_the_local_hook_without_unicode_reinterpretation() {
    let fixture = Fixture::new();
    let mut cases: Vec<Vec<u8>> = vec![
        b"fix(web): prevent overlapping controls\n\nbody".to_vec(),
        b"feat(api)!: require the new protocol\n".to_vec(),
        b"ci: check the release commit subject\n".to_vec(),
        b"ops(a/b-c_0.z): ready\r\n".to_vec(),
        b"Fix: uppercase\n".to_vec(),
        b"fix(): missing scope\n".to_vec(),
        b"fix(web):missing space\n".to_vec(),
        b"fix(web):  extra space\n".to_vec(),
        b"fix(web):\nfix: later line\n".to_vec(),
        b"fix\0(web): body\n".to_vec(),
        b"fix: \xc2\xa0\n".to_vec(),
        b"fix: \xff\xfe\n".to_vec(),
    ];
    for byte in 0..=255 {
        let mut message = b"fix: ".to_vec();
        message.extend([byte, b'x', b'\n']);
        cases.push(message);
    }
    for scope in [
        "a",
        "a0",
        "a/b-c_d.e",
        "",
        "A",
        "_a",
        ".a",
        "/a",
        "a b",
        "é",
    ] {
        for bang in ["", "!", "!!"] {
            cases.push(format!("feat({scope}){bang}: body\n").into_bytes());
        }
    }
    for message in cases {
        assert_eq!(
            valid(&message).unwrap(),
            fixture.hook(&message),
            "{}",
            message
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }
}

#[test]
fn bounded_ci_domain_and_first_line_behavior_are_explicit() {
    let mut line = b"fix: ".to_vec();
    line.resize(MAX_SUBJECT, b'x');
    assert!(valid(&line).unwrap());
    line.push(b'x');
    assert!(valid(&line).is_err());
    let mut output = b"fix: valid\n".to_vec();
    output.resize(MAX_OUTPUT, b'x');
    assert!(valid(&output).unwrap());
    output.push(b'x');
    assert!(valid(&output).is_err());
    assert!(!valid(b"\nfix: later").unwrap());
    assert!(!valid(b"fix: \0\n").unwrap());
    assert!(valid(b"fi\0x: x\t\r\nignored").unwrap());
}

struct Fake {
    commands: Vec<Command>,
    bytes: Vec<u8>,
    failure: Option<u8>,
    clean: bool,
    ready_calls: usize,
    cancel_on: Option<usize>,
}
impl Executor for Fake {
    async fn ready(&mut self) -> tooling::Result<()> {
        self.ready_calls += 1;
        if self.cancel_on == Some(self.ready_calls) {
            Err(Failure::new(143, "Owned interruption."))
        } else {
            Ok(())
        }
    }
    async fn run(&mut self, command: Command) -> tooling::Result<Vec<u8>> {
        self.commands.push(command);
        if let Some(code) = self.failure {
            Err(Failure::new(code, "Owned Git failure."))
        } else {
            Ok(self.bytes.clone())
        }
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
}
fn fake() -> Fake {
    Fake {
        commands: vec![],
        bytes: b"fix: valid\n".to_vec(),
        failure: None,
        clean: true,
        ready_calls: 0,
        cancel_on: None,
    }
}

#[tokio::test]
async fn closed_revision_fixed_argv_status_and_late_cancellation_are_binding() {
    for value in [
        "",
        "HEAD",
        "--all",
        "abc",
        &"a".repeat(39),
        &"g".repeat(40),
        &format!("{}\n", "a".repeat(40)),
    ] {
        assert!(
            Revision::read(&Environment::from([(
                "COMMIT_REVISION".into(),
                value.into()
            )]))
            .is_err()
        );
    }
    let revision = Revision::read(&Environment::from([(
        "COMMIT_REVISION".into(),
        "B".repeat(40).into(),
    )]))
    .unwrap();
    let mut executor = fake();
    verify(&revision, &mut executor).await.unwrap();
    let command = &executor.commands[0];
    assert_eq!(command.program, Program::Git);
    assert_eq!(
        command.arguments,
        ["log", "-1", "--format=%B", &"B".repeat(40)].map(std::ffi::OsString::from)
    );
    assert_eq!(command.timeout, Duration::from_secs(30));
    assert_eq!(command.capture, Some(1024 * 1024));
    for code in [23, 128, 130, 143, 124, 126, 127] {
        let mut executor = fake();
        executor.failure = Some(code);
        assert_eq!(
            verify(&revision, &mut executor).await.unwrap_err().code,
            code
        );
        assert_eq!(executor.commands.len(), 1);
    }
    for call in [1, 2, 3] {
        let mut executor = fake();
        executor.cancel_on = Some(call);
        assert_eq!(
            verify(&revision, &mut executor).await.unwrap_err().code,
            143
        );
        assert_eq!(executor.commands.len(), usize::from(call != 1));
    }
    let mut executor = fake();
    executor.clean = false;
    assert!(verify(&revision, &mut executor).await.is_err());
    let mut executor = fake();
    executor.bytes = b"Invalid private subject".to_vec();
    assert_eq!(
        verify(&revision, &mut executor).await.unwrap_err().message,
        INVALID
    );
}

#[tokio::test]
async fn actual_fake_git_uses_captured_path_and_keeps_failure_or_interrupt_status() {
    let fixture = Fixture::new();
    fixture.git("import json,sys;from pathlib import Path;root=Path(__file__).parent;(root/'args').write_text(json.dumps(sys.argv[1:]));print('fix: owned')");
    let environment = fixture.environment();
    let revision = Revision::read(&environment).unwrap();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    verify(&revision, &mut native).await.unwrap();
    assert_eq!(
        fs::read_to_string(fixture.0.join("args")).unwrap(),
        format!("[\"log\", \"-1\", \"--format=%B\", \"{}\"]", "a".repeat(40))
    );
    for body in ["raise SystemExit(37)", "print('x'*(1024*1024+1))"] {
        fixture.git(body);
        let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
        let error = verify(&revision, &mut native).await.unwrap_err();
        assert_eq!(error.code, if body.starts_with("raise") { 37 } else { 1 });
        assert!(native.cleanup_passed());
    }
    fs::remove_file(fixture.0.join("git")).unwrap();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert_eq!(verify(&revision, &mut native).await.unwrap_err().code, 127);
}

#[tokio::test]
async fn cancellation_retires_only_the_owned_fake_git_group() {
    use std::os::unix::process::CommandExt;
    let mut command = Process::new("/usr/bin/python3");
    command
        .args(["-c", "import time;time.sleep(30)"])
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let unrelated = crate::owned_process::Group::new(command.spawn().unwrap());
    drop(command);
    let fixture = Fixture::new();
    fixture.git(
        r#"import signal,time
from pathlib import Path
root=Path(__file__).parent
def stop(*_):
 (root/'term').write_text('retired');raise SystemExit(0)
signal.signal(signal.SIGTERM,stop)
pending=root/'ready.pending';pending.write_text('ready');pending.replace(root/'ready')
while True:time.sleep(.01)
"#,
    );
    let environment = fixture.environment();
    let revision = Revision::read(&environment).unwrap();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    let cancel = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !fixture.0.join("ready").is_file() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        sender.send(143).unwrap();
    };
    let (result, ()) = tokio::join!(verify(&revision, &mut native), cancel);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(native.cleanup_passed());
    assert!(fixture.0.join("term").is_file());
    assert!(!crate::owned_process::exited_now(unrelated.pid()).unwrap());
}
