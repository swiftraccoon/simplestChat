//! Fixed-plan and owned path fixtures; no database/container/browser work.

use super::*;

struct Fake {
    commands: Vec<Command>,
    output: Vec<u8>,
    fail: Option<(usize, u8)>,
    clean: bool,
}
impl Executor for Fake {
    async fn ready(&mut self) -> tooling::Result<()> {
        Ok(())
    }
    async fn run(&mut self, command: Command) -> tooling::Result<Vec<u8>> {
        self.commands.push(command);
        if let Some((index, code)) = self.fail
            && self.commands.len() == index
        {
            return Err(Failure::new(code, "Owned fixture failure."));
        }
        Ok(self.output.clone())
    }
    fn cleanup_passed(&self) -> bool {
        self.clean
    }
}

#[tokio::test]
async fn every_check_has_exact_python_pattern_and_restore_pull_precedes_discovery() {
    let root =
        std::env::temp_dir().join(format!("database check $(literal) {}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let environment = Environment::from([
        (OsString::from("PATH"), OsString::from("/usr/bin:/bin")),
        (OsString::from("RESTORE_CONTAINER_E2E"), OsString::from("1")),
    ]);
    for (check, pattern) in [
        (Check::Monitoring, "test_monitoring_database.py"),
        (Check::Restore, "test_restore_database.py"),
        (Check::Migration, "test_migrate_public.py"),
    ] {
        assert_eq!(
            check.command(),
            [
                "python",
                "-m",
                "unittest",
                "discover",
                "-s",
                "ops/ansible/tests",
                "-p",
                pattern,
                "-v"
            ]
        );
        let mut executor = Fake {
            commands: vec![],
            output: format!("{}\n", root.display()).into_bytes(),
            fail: None,
            clean: true,
        };
        let result = prepare(check, &environment, &mut executor).await.unwrap();
        assert_eq!(
            result.get(OsStr::new("RESTORE_CONTAINER_E2E")),
            Some(&OsString::from("1"))
        );
        assert_eq!(
            executor.commands.len(),
            if check == Check::Restore { 2 } else { 1 }
        );
        if check == Check::Restore {
            assert_eq!(executor.commands[0].program, Program::Docker);
            assert_eq!(
                executor.commands[0].arguments,
                ["pull", "docker.io/library/postgres:18.6-bookworm@sha256:3725f4e2499eef5134592b3b4ab79a543ed7f8e533b05b5b637af926630f6650"].map(OsString::from)
            );
            assert!(executor.commands[0].capture.is_none());
            assert_eq!(executor.commands[0].timeout, Duration::from_secs(600));
        }
        let command = executor.commands.last().unwrap();
        assert_eq!(command.program, Program::PgConfig);
        assert_eq!(command.arguments, [OsString::from("--bindir")]);
        assert_eq!(command.capture, Some(4096));
        assert_eq!(command.timeout, Duration::from_secs(30));
        assert_eq!(
            std::env::split_paths(result.get(OsStr::new("PATH")).unwrap()).next(),
            Some(root.clone())
        );
    }
    std::fs::remove_dir(root).unwrap();
}

#[tokio::test]
async fn preparation_failures_preserve_status_and_never_continue() {
    for code in [23, 124, 130, 143, 127] {
        let mut executor = Fake {
            commands: vec![],
            output: vec![],
            fail: Some((1, code)),
            clean: true,
        };
        assert_eq!(
            prepare(Check::Restore, &Environment::new(), &mut executor)
                .await
                .unwrap_err()
                .code,
            code
        );
        assert_eq!(executor.commands.len(), 1);
    }
}

#[test]
fn path_admission_and_closed_selection_reject_ambiguous_inputs() {
    for args in [
        vec![],
        vec!["other".into()],
        vec!["restore".into(), "extra".into()],
    ] {
        assert!(Check::parse(&args).is_none());
    }
    for output in [
        b"".as_slice(),
        b"relative",
        b"/tmp\nsecond",
        b"/tmp\r\n",
        b"/tmp\0",
        b"\xff",
    ] {
        assert!(postgres_path(output, &Environment::new()).is_err());
    }
}

struct Owned(PathBuf);
impl Owned {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "pg config native {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let program = directory.join("pg_config");
        std::fs::write(&program, format!("#!/usr/bin/python3\n{script}")).unwrap();
        std::fs::set_permissions(program, std::fs::Permissions::from_mode(0o700)).unwrap();
        Self(directory)
    }
    fn environment(&self) -> Environment {
        Environment::from([("PATH".into(), self.0.clone().into_os_string())])
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[tokio::test]
async fn actual_owned_pg_config_preserves_paths_status_and_same_signal_registration() {
    let fixture = Owned::new(
        "import json,sys;from pathlib import Path;root=Path(__file__).parent;(root/'args.json').write_text(json.dumps(sys.argv[1:]));print(root)",
    );
    let environment = fixture.environment();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    let result = prepare(Check::Monitoring, &environment, &mut native)
        .await
        .unwrap();
    assert_eq!(
        std::env::split_paths(result.get(OsStr::new("PATH")).unwrap()).next(),
        Some(fixture.0.clone())
    );
    assert_eq!(
        std::fs::read_to_string(fixture.0.join("args.json")).unwrap(),
        "[\"--bindir\"]"
    );
    assert!(native.cleanup_passed());
    // The original receiver survives transfer; no second listener is installed.
    let mut signals = native.into_signals().await.unwrap();
    sender.send(130).unwrap();
    assert_eq!(signals.pending().await, Some(130));

    let fixture = Owned::new("raise SystemExit(37)");
    let environment = fixture.environment();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert_eq!(
        prepare(Check::Monitoring, &environment, &mut native)
            .await
            .unwrap_err()
            .code,
        37
    );
    assert!(native.cleanup_passed());
    std::fs::remove_file(fixture.0.join("pg_config")).unwrap();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert_eq!(
        prepare(Check::Monitoring, &environment, &mut native)
            .await
            .unwrap_err()
            .code,
        127
    );
    assert!(native.cleanup_passed());
}

#[tokio::test]
async fn actual_discovery_overflow_or_cancellation_never_transfers_clean_success() {
    let fixture = Owned::new("print('x'*4097)");
    let environment = fixture.environment();
    let (_sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    assert!(
        prepare(Check::Migration, &environment, &mut native)
            .await
            .is_err()
    );

    let fixture = Owned::new(
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
    let (result, ()) = tokio::join!(
        prepare(Check::Monitoring, &environment, &mut native),
        cancel
    );
    assert_eq!(result.unwrap_err().code, 143);
    assert!(fixture.0.join("term").is_file());
    assert!(native.cleanup_passed());
    assert_eq!(native.into_signals().await.err().unwrap().code, 143);
}

#[tokio::test]
async fn unproven_cleanup_and_late_cancellation_fence_the_harness_handoff() {
    let fixture = Owned::new("from pathlib import Path;print(Path(__file__).parent)");
    let environment = fixture.environment();
    let mut fake = Fake {
        commands: vec![],
        output: fixture.0.to_string_lossy().as_bytes().to_vec(),
        fail: None,
        clean: false,
    };
    assert!(
        prepare(Check::Monitoring, &environment, &mut fake)
            .await
            .unwrap_err()
            .message
            .contains("cleanup is unproven")
    );
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = Native::new(&fixture.0, &environment, Signals::injected(receiver));
    prepare(Check::Monitoring, &environment, &mut native)
        .await
        .unwrap();
    sender.send(143).unwrap();
    assert_eq!(native.into_signals().await.err().unwrap().code, 143);
}

#[tokio::test]
async fn failed_restore_discovery_does_not_hide_its_status_after_successful_pull() {
    let mut executor = Fake {
        commands: vec![],
        output: vec![],
        fail: Some((2, 37)),
        clean: true,
    };
    assert_eq!(
        prepare(Check::Restore, &Environment::new(), &mut executor)
            .await
            .unwrap_err()
            .code,
        37
    );
    assert_eq!(executor.commands.len(), 2);
    assert_eq!(executor.commands[0].program, Program::Docker);
    assert_eq!(executor.commands[1].program, Program::PgConfig);
}
