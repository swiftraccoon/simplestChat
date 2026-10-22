//! Real fixture commands establish lifetime, stream and cleanup behavior without
//! a server, listener, container engine or benchmark workload.

use super::{Cancellation, Limits, Request, Runner};
use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("bench child {}", uuid::Uuid::new_v4()));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
    fn runner(&self) -> Runner {
        Runner {
            directory: self.0.clone(),
            environment: [("PATH".into(), "/bin:/usr/bin".into())].into(),
            cancellation: Cancellation::default(),
        }
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

async fn terminal(child: &mut super::child::Child) -> super::child::Exit {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if let Some(exit) = child.poll().await.unwrap() {
                return exit;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn early_exit_keeps_literal_arguments_stdin_and_exact_code_until_reap() {
    let directory = Directory::new();
    let literal = "literal spaces ; $(touch forbidden)";
    let mut child = directory
        .runner()
        .spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "test -z \"${HOME+x}\" || exit 9; cat; printf '%s' \"$1\"; printf err >&2; exit 7"
                    .into(),
                "fixture".into(),
                literal.into(),
            ],
            Request {
                input: b"input".to_vec(),
                ..Request::default()
            },
        )
        .unwrap();
    let exit = terminal(&mut child).await;
    assert_eq!(exit.code, Some(7));
    assert_eq!(exit.signal, None);
    assert!(child.pid() > 1);
    let stopped = child.stop(Duration::from_millis(20)).await.unwrap();
    assert_eq!(stopped.code, Some(7));
    assert_eq!(child.output, format!("input{literal}").as_bytes());
    assert_eq!(child.errors, b"err");
    assert!(!directory.0.join("forbidden").exists());
    assert_eq!(child.stop(Duration::ZERO).await.unwrap().code, Some(7));
    assert!(
        directory
            .runner()
            .spawn(
                Path::new("/fixture missing executable"),
                &[],
                Request::default()
            )
            .is_err()
    );
}

#[tokio::test]
async fn successful_leader_never_releases_a_live_descendant_or_resignals_after_reap() {
    let directory = Directory::new();
    let mut child = directory
        .runner()
        .spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "(sleep 0.3; printf leaked > leaked) </dev/null >/dev/null 2>&1 & exit 0".into(),
            ],
            Request::default(),
        )
        .unwrap();
    assert_eq!(terminal(&mut child).await.code, Some(0));
    assert_eq!(
        child.stop(Duration::from_millis(20)).await.unwrap().code,
        Some(0)
    );
    assert_eq!(child.stop(Duration::ZERO).await.unwrap().code, Some(0));
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!directory.0.join("leaked").exists());
}

#[tokio::test]
async fn ignored_term_uses_bounded_grace_and_retains_the_actual_kill_signal() {
    let directory = Directory::new();
    let mut child = directory
        .runner()
        .spawn(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "trap '' TERM; printf ready > ready; while :; do sleep 1; done".into(),
            ],
            Request::default(),
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !directory.0.join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let begin = Instant::now();
    let exit = child.stop(Duration::from_millis(25)).await.unwrap();
    assert_eq!(exit.code, None);
    assert_eq!(exit.signal.as_deref(), Some("SIGKILL"));
    assert!(begin.elapsed() < Duration::from_secs(3));
}

#[tokio::test]
async fn streaming_has_a_sticky_byte_limit_without_delivering_the_overflow_byte() {
    let directory = Directory::new();
    let mut child = directory
        .runner()
        .spawn_stream(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "printf 123456789; printf private-error >&2".into(),
            ],
            Request {
                limits: Limits {
                    stdout: 8,
                    ..Limits::default()
                },
                ..Request::default()
            },
        )
        .unwrap();
    let mut stream = child.take_stdout().unwrap();
    assert!(child.take_stdout().is_none());
    let mut buffer = [0; 64];
    let mut result = Vec::new();
    while result.len() < 8 {
        let count = stream.read(&mut buffer).await.unwrap();
        result.extend_from_slice(&buffer[..count]);
    }
    assert_eq!(result, b"12345678");
    assert!(
        stream
            .read(&mut buffer)
            .await
            .unwrap_err()
            .to_string()
            .contains("limit_exceeded")
    );
    assert!(
        stream
            .read(&mut buffer)
            .await
            .unwrap_err()
            .to_string()
            .contains("limit_exceeded")
    );
    assert_eq!(
        child.stop(Duration::from_millis(20)).await.unwrap().code,
        Some(0)
    );
    assert_eq!(child.errors, b"private-error");
}

#[tokio::test]
async fn capture_overflow_remains_failure_while_cleanup_still_reclaims_the_group() {
    let directory = Directory::new();
    let mut child = directory
        .runner()
        .spawn(
            Path::new("/bin/sh"),
            &["-c".into(), "while :; do printf 1234567890; done".into()],
            Request {
                limits: Limits {
                    stdout: 8,
                    ..Limits::default()
                },
                ..Request::default()
            },
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if child.poll().await.is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(child.poll().await.is_err());
    assert!(child.stop(Duration::from_millis(20)).await.is_err());
    assert!(
        child.stop(Duration::ZERO).await.is_err(),
        "a retry must not erase the failed sink"
    );
}

#[tokio::test]
async fn aborting_the_owner_future_kills_before_the_group_identity_can_be_reused() {
    let directory = Directory::new();
    let runner = directory.runner();
    let task = tokio::spawn(async move {
        let _child = runner
            .spawn(
                Path::new("/bin/sh"),
                &[
                    "-c".into(),
                    "printf ready > ready; (sleep 0.3; printf leaked > leaked) & wait".into(),
                ],
                Request::default(),
            )
            .unwrap();
        std::future::pending::<()>().await;
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !directory.0.join("ready").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!directory.0.join("leaked").exists());
}
