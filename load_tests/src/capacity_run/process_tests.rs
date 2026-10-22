//! Real owned fake subprocesses exercise bounded I/O and process-group cleanup.

use super::{
    fixtures::Directory,
    process::{Cancellation, Limits, Request, Runner},
};
use std::path::Path;
use std::time::{Duration, Instant};

fn runner(directory: &Directory, cancellation: Cancellation) -> Runner {
    Runner {
        directory: directory.0.clone(),
        environment: [("PATH".into(), "/bin:/usr/bin".into())].into(),
        cancellation,
    }
}
#[tokio::test]
async fn streams_exit_status_and_stdin_are_retained_without_an_engine() {
    let directory = Directory::new();
    let runner = runner(&directory, Cancellation::default());
    let output = runner
        .run(
            Path::new("/bin/sh"),
            &["-c".into(), "cat; printf err >&2; exit 7".into()],
            Request {
                input: b"input".to_vec(),
                ..Request::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(output.status, 7);
    assert_eq!(output.stdout, b"input");
    assert_eq!(output.stderr, b"err");
}
#[tokio::test]
async fn byte_ceiling_and_deadline_reclaim_owned_descendants() {
    let directory = Directory::new();
    let runner = runner(&directory, Cancellation::default());
    let begin = Instant::now();
    let error = runner
        .run(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                "while :; do printf 1234567890; printf abcdefghij >&2; done".into(),
            ],
            Request {
                limits: Limits {
                    timeout: Duration::from_secs(2),
                    stdout: 1024,
                    stderr: 1024,
                },
                ..Request::default()
            },
        )
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("limit_exceeded"));
    assert!(begin.elapsed() < Duration::from_secs(10));
    let error = runner
        .run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & wait".into()],
            Request {
                limits: Limits {
                    timeout: Duration::from_millis(100),
                    ..Limits::default()
                },
                ..Request::default()
            },
        )
        .await
        .err()
        .unwrap();
    assert_eq!(error.to_string(), "command_timed_out");
}
#[tokio::test]
async fn a_successful_leader_cannot_leave_a_detached_pipe_closing_descendant() {
    let directory = Directory::new();
    let runner = runner(&directory, Cancellation::default());
    let output = runner
        .run(
            Path::new("/bin/sh"),
            &[
                "-c".into(),
                concat!(
                    "(while [ ! -f release ]; do sleep 0.01; done; ",
                    "printf leaked > leaked) </dev/null >/dev/null 2>&1 & exit 0"
                )
                .into(),
            ],
            Request::default(),
        )
        .await
        .unwrap();
    assert_eq!(output.status, 0);
    std::fs::write(directory.0.join("release"), b"release").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!directory.0.join("leaked").exists());
}
#[tokio::test]
async fn cancellation_stops_the_owned_group_and_cleanup_can_still_run() {
    let directory = Directory::new();
    let cancellation = Cancellation::default();
    let runner = runner(&directory, cancellation.clone());
    let owned = cancellation.clone();
    let cancel = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        owned.cancel(143);
    });
    let error = runner
        .run(
            Path::new("/bin/sh"),
            &["-c".into(), "sleep 30 & wait".into()],
            Request::default(),
        )
        .await
        .err()
        .unwrap();
    cancel.await.unwrap();
    assert_eq!(error.to_string(), "capacity interrupted");
    assert!(
        runner
            .run(
                Path::new("/bin/sh"),
                &["-c".into(), "exit 0".into()],
                Request::default()
            )
            .await
            .is_err()
    );
    assert_eq!(
        runner
            .run(
                Path::new("/bin/sh"),
                &["-c".into(), "exit 0".into()],
                Request {
                    cleanup: true,
                    ..Request::default()
                }
            )
            .await
            .unwrap()
            .status,
        0
    );
}
#[test]
fn darwin_zombie_exception_requires_exact_unreaped_group_members() {
    let pid = nix::unistd::Pid::from_raw(1234);
    assert!(crate::owned_process::zombie_rows(
        b"1234 1234 Z\n1235 1234 Z+\n",
        pid
    ));
    for bytes in [
        b"".as_slice(),
        b"1235 1234 Z\n",
        b"1234 999 Z\n",
        b"1234 1234 S\n",
        b"1234 1234 Z\n1234 1234 Z\n",
        b"01234 1234 Z\n",
    ] {
        assert!(!crate::owned_process::zombie_rows(bytes, pid));
    }
}

#[tokio::test]
async fn aborting_the_future_drops_guard_kills_group_and_preserves_unrelated_child() {
    use std::process::Stdio;
    let directory = Directory::new();
    let owned = runner(&directory, Cancellation::default());
    let mut unrelated = tokio::process::Command::new("/bin/sleep")
        .arg("30")
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let task = tokio::spawn(async move {
        owned
            .run(
                Path::new("/bin/sh"),
                &[
                    "-c".into(),
                    concat!(
                        "(printf ready > ready; ",
                        "while [ ! -f release ]; do sleep 0.01; done; ",
                        "printf leaked > leaked) & sleep 30; wait"
                    )
                    .into(),
                ],
                Request::default(),
            )
            .await
    });
    let deadline = Instant::now() + Duration::from_secs(5);
    while !directory.0.join("ready").exists() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // The descendant cannot perform its observable write before abort/reap.
    // A scheduler pause must not turn this ownership test into a timer race.
    std::fs::write(directory.0.join("release"), b"release").unwrap();
    tokio::time::sleep(Duration::from_millis(400)).await;
    assert!(!directory.0.join("leaked").exists());
    assert!(unrelated.try_wait().unwrap().is_none());
    unrelated.kill().await.unwrap();
    unrelated.wait().await.unwrap();
}
