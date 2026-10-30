//! Publication remains within the original deadline and signal-owner lifetime.

use super::*;
use crate::ci::{command::Command, process::Executor};
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Default)]
struct Fake {
    replay_failure: Option<u8>,
    ready_failure: Option<(usize, u8)>,
    cleanup_failure: bool,
    late: bool,
    ready_calls: usize,
    replayed: Vec<(Vec<u8>, bool, Instant)>,
}
impl Executor for Fake {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("publication must use the existing replay owner");
    }
    async fn ready(&mut self) -> Result<()> {
        self.ready_calls += 1;
        match self.ready_failure {
            Some((at, code)) if self.ready_calls >= at => Err(failure(code)),
            _ => Ok(()),
        }
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, end: Instant) -> Result<()> {
        self.replayed.push((bytes.into(), stderr, end));
        if self.late {
            tokio::time::sleep_until(
                tokio::time::Instant::from_std(end) + Duration::from_millis(1),
            )
            .await;
        }
        match self.replay_failure {
            Some(code) => Err(failure(code)),
            None => Ok(()),
        }
    }
    fn cleanup_passed(&self) -> bool {
        !self.cleanup_failure || self.replayed.is_empty()
    }
}
fn completed(status: u8) -> Completed {
    Completed {
        summary: json!({"candidatePassed":status == 0,"deployed":true,
            "failureClass":if status == 0 { None } else { Some("original cause") }}),
        end: Instant::now() + Duration::from_secs(60),
        status,
    }
}

#[tokio::test]
async fn one_stdout_publication_uses_the_original_deadline_and_candidate_status() {
    let mut complete = completed(0);
    let end = complete.end;
    let mut fake = Fake::default();
    super::super::execute::publish(&mut complete, &mut fake).await;
    assert_eq!(complete.status, 0);
    assert_eq!(fake.ready_calls, 2);
    assert_eq!(fake.replayed.len(), 1);
    let (bytes, stderr, deadline) = &fake.replayed[0];
    assert!(!stderr);
    assert_eq!(*deadline, end);
    assert!(bytes.ends_with(b"\n"));
    let payload: Value = serde_json::from_slice(bytes).unwrap();
    assert_eq!(payload["passed"], true);
    assert_eq!(complete.summary["passed"], true);
}

#[tokio::test]
async fn replay_failure_cannot_replace_an_earlier_body_status() {
    for status in [0, 23] {
        for replay_failure in [1, 124, 130, 143] {
            let mut complete = completed(status);
            let mut fake = Fake {
                replay_failure: Some(replay_failure),
                ..Fake::default()
            };
            super::super::execute::publish(&mut complete, &mut fake).await;
            assert_eq!(
                complete.status,
                if status == 0 { replay_failure } else { status }
            );
            assert_eq!(fake.replayed.len(), 1);
            let payload: Value = serde_json::from_slice(&fake.replayed[0].0).unwrap();
            assert_eq!(payload["passed"], status == 0);
            assert_eq!(complete.summary["passed"], false);
            if status != 0 {
                assert_eq!(complete.summary["failureClass"], "original cause");
            }
        }
    }
}

#[tokio::test]
async fn replay_success_still_requires_postawait_signal_cleanup_and_original_time_fences() {
    for status in [0, 23] {
        for case in 0..3 {
            let mut complete = completed(status);
            let mut fake = Fake::default();
            match case {
                0 => fake.ready_failure = Some((2, 130)),
                1 => fake.cleanup_failure = true,
                _ => {
                    complete.end = Instant::now() + Duration::from_millis(100);
                    fake.late = true;
                }
            }
            super::super::execute::publish(&mut complete, &mut fake).await;
            assert_eq!(
                complete.status,
                if status == 0 {
                    [130, 1, 124][case]
                } else {
                    status
                }
            );
            assert_eq!(fake.replayed.len(), 1);
            assert_eq!(complete.summary["passed"], false);
        }
    }
}

#[tokio::test]
async fn known_interruption_or_expiry_cannot_start_publication() {
    for status in [0, 23] {
        for interrupted in [false, true] {
            let mut complete = completed(status);
            let mut fake = Fake::default();
            if interrupted {
                fake.ready_failure = Some((1, 143));
            } else {
                complete.end = Instant::now();
            }
            super::super::execute::publish(&mut complete, &mut fake).await;
            assert_eq!(
                complete.status,
                if status != 0 {
                    status
                } else if interrupted {
                    143
                } else {
                    124
                }
            );
            assert!(fake.replayed.is_empty());
            assert_eq!(complete.summary["passed"], false);
        }
    }
}

#[test]
fn native_publication_environment_has_no_captured_credentials_or_configuration() {
    let environment = super::super::execute::replay_environment();
    for name in [
        "GH_TOKEN",
        "GITHUB_TOKEN",
        "GH_CONFIG_DIR",
        "XDG_CONFIG_HOME",
        "ANSIBLE_CONFIG",
        "SSH_AUTH_SOCK",
        "HTTPS_PROXY",
        "PYTHONPATH",
    ] {
        assert!(!environment.contains_key(OsStr::new(name)), "{name}");
    }
    assert_eq!(environment.len(), 5);
}

struct BlockedReplay {
    signals: crate::owned_process::Signals,
    reader: Option<std::fs::File>,
    writer: Option<std::fs::File>,
    marker: std::path::PathBuf,
    replayed: bool,
}
impl Executor for BlockedReplay {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("publication cannot dispatch another operation");
    }
    async fn ready(&mut self) -> Result<()> {
        match self.signals.pending().await {
            Some(code) => Err(failure(code)),
            None => Ok(()),
        }
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, end: Instant) -> Result<()> {
        assert!(!stderr);
        self.replayed = true;
        let mut command = std::process::Command::new("/bin/sh");
        command
            .args(["-c", "printf ready > \"$1\"; exec /bin/cat", "owned-replay"])
            .arg(&self.marker)
            .env_clear()
            .stdout(std::process::Stdio::from(self.writer.take().unwrap()))
            .stderr(std::process::Stdio::null());
        let replay =
            crate::ci::release_controller::replay::run(command, bytes, &mut self.signals, end)
                .await;
        let closed = nix::unistd::close(self.reader.take().unwrap()).map_err(|_| failure(1));
        replay.and(closed)
    }
}

#[test]
fn isolated_original_owner_covers_interrupted_blocked_publication() {
    const CHILD: &str = "SIMPLESTCHAT_FRONTEND_PUBLICATION_CHILD";
    if let Ok(case) = std::env::var(CHILD) {
        use std::{fs, os::unix::fs::PermissionsExt};
        let root =
            std::env::temp_dir().join(format!("routine-publication-{}-{case}", std::process::id()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        let marker = root.join("replay-ready");
        let start = Instant::now();
        let owner = Arc::new(CompletionSignals::new().unwrap());
        let result = scope(
            start,
            0,
            owner,
            |runtime, flags, _| {
                let (reader, writer) = nix::unistd::pipe().unwrap();
                let mut executor = BlockedReplay {
                    signals: crate::owned_process::Signals::from_completion(flags),
                    reader: Some(reader.into()),
                    writer: Some(writer.into()),
                    marker: marker.clone(),
                    replayed: false,
                };
                let watched = marker.clone();
                let signal = if case == "term" {
                    nix::sys::signal::Signal::SIGTERM
                } else {
                    nix::sys::signal::Signal::SIGINT
                };
                let notifier = std::thread::spawn(move || {
                    let end = Instant::now() + Duration::from_secs(5);
                    while !watched.is_file() && Instant::now() < end {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    assert!(watched.is_file(), "the owned replay reached publication");
                    std::thread::sleep(Duration::from_millis(25));
                    nix::sys::signal::kill(nix::unistd::getpid(), signal).unwrap();
                });
                let mut complete = completed(if case == "first" { 23 } else { 0 });
                complete.summary["padding"] = json!("x".repeat(2 * 1024 * 1024));
                runtime.block_on(super::super::execute::publish(&mut complete, &mut executor));
                notifier.join().unwrap();
                assert!(executor.replayed);
                assert!(executor.reader.is_none() && executor.writer.is_none());
                Ok(complete)
            },
            || Ok(()),
        )
        .unwrap();
        assert_eq!(
            result.status,
            match case.as_str() {
                "int" => 130,
                "term" => 143,
                "first" => 23,
                _ => panic!("case"),
            }
        );
        assert_eq!(result.summary["passed"], false);
        fs::remove_dir_all(root).unwrap();
        return;
    }
    for case in ["int", "term", "first"] {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "ci::ops_artifact::deploy_frontend::lifecycle::publish_tests::\
                isolated_original_owner_covers_interrupted_blocked_publication",
                "--nocapture",
            ])
            .env(CHILD, case)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
