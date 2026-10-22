//! Actual inert child fixtures for the shared Native combined-log mode.

use super::*;
use crate::ci::{bounded_log::Log, command::Program};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    path: PathBuf,
    environment: Environment,
}
impl Fixture {
    fn new(body: &str) -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "native-log-fixture-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        let program = path.join("docker");
        fs::write(
            &program,
            format!(
                "#!/usr/bin/python3\nimport os,signal,sys,time\nfrom pathlib import Path\nroot=Path(os.environ['FIXTURE_ROOT'])\n{body}\n"
            ),
        )
        .unwrap();
        fs::set_permissions(program, fs::Permissions::from_mode(0o700)).unwrap();
        let environment = Environment::from([
            ("PATH".into(), path.clone().into()),
            ("FIXTURE_ROOT".into(), path.clone().into()),
        ]);
        Self { path, environment }
    }
    fn log(&self, maximum: u64) -> Log {
        let output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(self.path.join("output.log"))
            .unwrap();
        Log::start(output, maximum).unwrap()
    }
    fn request(&self, maximum: u64) -> Command {
        let mut command = Command::capture(Program::Docker, &["literal-argument"]);
        command.capture = None;
        command.combined_log = Some(Arc::new(self.log(maximum)));
        command.timeout = Duration::from_secs(3);
        command.grace = Duration::from_millis(20);
        command
    }
    fn native(&self, signals: Signals) -> Native<'_> {
        Native::new(&self.path, &self.environment, signals)
    }
    fn bytes(&self) -> Vec<u8> {
        fs::read(self.path.join("output.log")).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}
fn signals() -> Signals {
    let (_, receiver) = tokio::sync::mpsc::unbounded_channel();
    Signals::injected(receiver)
}
async fn ready(path: &Path) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}
fn close(file: File) {
    nix::unistd::close(file).unwrap();
}

#[tokio::test]
async fn combined_mode_keeps_ordered_streams_private_and_closes_before_success() {
    let fixture = Fixture::new(
        "assert sys.argv[1:]==['literal-argument']\nos.write(1,b'output\\n')\nos.write(2,b'private-error\\n')",
    );
    let mut native = fixture.native(signals());
    assert!(native.run(fixture.request(64)).await.unwrap().is_empty());
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"output\nprivate-error\n");
}

#[tokio::test]
async fn normal_leader_exit_retires_pipe_holding_descendant_before_eof() {
    use std::os::unix::process::CommandExt;
    let mut unrelated_command = std::process::Command::new("/bin/sleep");
    unrelated_command.arg("30").process_group(0);
    let unrelated = Group::new(unrelated_command.spawn().unwrap());
    let fixture = Fixture::new(
        r#"child=os.fork()
if child==0:
    signal.signal(signal.SIGTERM,signal.SIG_IGN)
    (root/'worker').write_text(str(os.getpid()))
    while True:time.sleep(.01)
while not (root/'worker').exists():time.sleep(.005)
os.write(1,b'leader completed\n')
"#,
    );
    let mut native = fixture.native(signals());
    native.run(fixture.request(128)).await.unwrap();
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"leader completed\n");
    assert!(!crate::owned_process::exited_now(unrelated.pid()).unwrap());
    // Darwin can retain dead orphan zombies; live worker states are refused.
    let worker = fs::read_to_string(fixture.path.join("worker")).unwrap();
    let observed = std::process::Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", worker.trim()])
        .output()
        .unwrap();
    let state = String::from_utf8(observed.stdout).unwrap();
    assert!(state.trim().is_empty() || state.trim_start().starts_with('Z'));
    drop(unrelated);
}

#[tokio::test]
async fn overflow_caps_real_file_and_retires_the_child_without_echoing_output() {
    let fixture = Fixture::new("os.write(1,b'PRIVATE-SENTINEL'*4096)\nwhile True:time.sleep(.01)");
    let mut native = fixture.native(signals());
    let error = native.run(fixture.request(32)).await.unwrap_err();
    assert_eq!(error.code, 1);
    assert!(!error.message.contains("PRIVATE-SENTINEL"));
    assert_eq!(fixture.bytes().len(), 32);
    assert!(native.cleanup_passed());
}

const IDLE: &str = r#"signal.signal(signal.SIGTERM,signal.SIG_IGN)
signal.signal(signal.SIGINT,signal.SIG_IGN)
os.write(1,b'private-output\n')
(root/'ready').write_text(str(os.getpid()))
while True:time.sleep(.01)
"#;

#[tokio::test]
async fn combined_timeout_and_interrupt_keep_status_after_closed_log_and_group_cleanup() {
    for code in [124, 130, 143] {
        let fixture = Fixture::new(IDLE);
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut native = fixture.native(Signals::injected(receiver));
        let mut command = fixture.request(128);
        if code == 124 {
            command.timeout = Duration::from_millis(300);
        }
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(native.run(command), cancel);
        assert_eq!(result.unwrap_err().code, code);
        assert_eq!(fixture.bytes(), b"private-output\n");
        assert!(native.cleanup_passed());
    }
}

struct Unproven;
impl Finalizer for Unproven {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        // Actually reclaim the inert fixture before injecting an unproven
        // terminal/reap result; the caller must not erase its known primary.
        let _ = Finalize.finish(group).await;
        (
            false,
            Err(Failure::new(1, "Injected terminal/reap failure.")),
        )
    }
}
#[tokio::test]
async fn known_timeout_and_interrupt_survive_unproven_terminal_or_reap() {
    for combined in [false, true] {
        for code in [124, 130, 143] {
            let fixture = Fixture::new(IDLE);
            let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
            let mut native = fixture.native(Signals::injected(receiver));
            let mut command = if combined {
                fixture.request(128)
            } else {
                Command::capture(Program::Docker, &[])
            };
            command.grace = Duration::from_millis(20);
            if code == 124 {
                command.timeout = Duration::from_millis(300);
            }
            let cancel = async {
                ready(&fixture.path.join("ready")).await;
                if code != 124 {
                    sender.send(code).unwrap();
                }
            };
            let (result, ()) = tokio::join!(native.execute_with(command, &Unproven), cancel);
            assert_eq!(result.unwrap_err().code, code);
            assert!(!native.cleanup_passed());
        }
    }
}

struct UnprovenCleanup;
impl Finalizer for UnprovenCleanup {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let (_, reaped) = Finalize.finish(group).await;
        (false, reaped)
    }
}
#[tokio::test]
async fn reaped_nonzero_status_survives_unproven_cleanup_while_zero_is_refused() {
    for combined in [false, true] {
        for code in [0, 42, 130, 143] {
            let fixture = Fixture::new(&format!("raise SystemExit({code})"));
            let mut native = fixture.native(signals());
            let command = if combined {
                fixture.request(128)
            } else {
                Command::capture(Program::Docker, &[])
            };
            let error = native
                .execute_with(command, &UnprovenCleanup)
                .await
                .unwrap_err();
            assert_eq!(error.code, if code == 0 { 1 } else { code });
            assert!(!native.cleanup_passed());
        }
    }
}

#[tokio::test]
async fn setup_spawn_and_prior_cancel_failures_finish_every_admitted_log_descriptor() {
    use std::os::fd::AsRawFd;
    for failure in ["mode", "role", "missing", "permission", "cancel"] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(fixture.path.join("output.log"))
            .unwrap();
        let output_fd = output.as_raw_fd();
        let log = Log::start(output, 128).unwrap();
        let pipe_fds = log.pipe_fds();
        let mut command = Command::capture(Program::Docker, &[]);
        command.capture = None;
        command.combined_log = Some(Arc::new(log));
        let expected = match failure {
            "mode" => {
                command.capture = Some(8);
                1
            }
            "role" => {
                command.program = Program::Node;
                1
            }
            "missing" => {
                fs::remove_file(fixture.path.join("docker")).unwrap();
                127
            }
            "permission" => {
                fs::set_permissions(
                    fixture.path.join("docker"),
                    fs::Permissions::from_mode(0o600),
                )
                .unwrap();
                126
            }
            _ => 143,
        };
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if failure == "cancel" {
            sender.send(143).unwrap();
        }
        let mut native = fixture.native(Signals::injected(receiver));
        assert_eq!(native.run(command).await.unwrap_err().code, expected);
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        for fd in [output_fd, pipe_fds[0], pipe_fds[1]] {
            assert!(
                !Path::new(&format!("/dev/fd/{fd}")).exists(),
                "{failure}: {fd}"
            );
        }
        assert!(fixture.bytes().is_empty());
    }
}

struct Finalized(Arc<AtomicBool>);
impl Finalizer for Finalized {
    async fn finish(&self, group: &mut Group) -> (bool, Result<std::process::ExitStatus>) {
        let result = Finalize.finish(group).await;
        self.0.store(true, Ordering::SeqCst);
        result
    }
}
#[tokio::test]
async fn interruption_during_final_log_completion_cannot_return_zero() {
    let fixture = Fixture::new("os.write(1,b'closed child\\n')");
    let log = fixture.log(128);
    let writer = log.writer().unwrap();
    let mut command = Command::capture(Program::Docker, &[]);
    command.capture = None;
    command.combined_log = Some(Arc::new(log));
    let finalized = Arc::new(AtomicBool::new(false));
    let finalizer = Finalized(finalized.clone());
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let mut native = fixture.native(Signals::injected(receiver));
    let cancel = async {
        while !finalized.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        sender.send(143).unwrap();
        close(writer);
    };
    let (result, ()) = tokio::join!(native.execute_with(command, &finalizer), cancel);
    assert_eq!(result.unwrap_err().code, 143);
    assert!(native.cleanup_passed());
    assert_eq!(fixture.bytes(), b"closed child\n");
}

#[tokio::test]
async fn combined_mode_refuses_ambiguous_modes_and_borrowed_owners_before_spawn() {
    for borrowed in [false, true] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let log = Arc::new(fixture.log(128));
        let mut command = Command::capture(Program::Docker, &[]);
        command.combined_log = Some(log.clone());
        if borrowed {
            command.capture = None;
        }
        let mut native = fixture.native(signals());
        assert!(native.run(command).await.is_err());
        assert!(!fixture.path.join("dispatched").exists());
        assert!(native.cleanup_passed());
        Arc::try_unwrap(log).unwrap().finish().await.unwrap();
    }
}

#[tokio::test]
async fn replay_known_interrupt_and_timeout_survive_unproven_finalization() {
    use std::process::Stdio;
    for code in [124, 130, 143] {
        let fixture = Fixture::new(IDLE);
        let mut command = std::process::Command::new(fixture.path.join("docker"));
        command
            .env_clear()
            .envs(&fixture.environment)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let mut signals = Signals::injected(receiver);
        let deadline =
            std::time::Instant::now() + Duration::from_secs(if code == 124 { 21 } else { 30 });
        let cancel = async {
            ready(&fixture.path.join("ready")).await;
            if code != 124 {
                sender.send(code).unwrap();
            }
        };
        let (result, ()) = tokio::join!(
            crate::ci::release_controller::replay::run_with_finalizer(
                command,
                b"owned bytes",
                &mut signals,
                deadline,
                &Unproven
            ),
            cancel
        );
        assert_eq!(result.unwrap_err().code, code);
    }
}

#[tokio::test]
async fn replay_unfit_deadline_cannot_dispatch_and_pending_interrupt_has_precedence() {
    use std::process::Stdio;
    for code in [124, 130, 143] {
        let fixture = Fixture::new("(root/'dispatched').write_text('unexpected')");
        let mut command = std::process::Command::new(fixture.path.join("docker"));
        command
            .env_clear()
            .envs(&fixture.environment)
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        if code != 124 {
            sender.send(code).unwrap();
        }
        let mut signals = Signals::injected(receiver);
        let result = crate::ci::release_controller::replay::run(
            command,
            b"owned bytes",
            &mut signals,
            std::time::Instant::now(),
        )
        .await;
        assert_eq!(result.unwrap_err().code, code);
        assert!(!fixture.path.join("dispatched").exists());
    }
}
