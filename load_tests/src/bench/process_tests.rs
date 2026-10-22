//! Only owned short fixture commands. These tests start no application workload.

use super::process::{Command, NativeHost, Sink, run};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn command(directory: &Path) -> Command {
    Command {
        executable: "/bin/sh".into(),
        arguments: vec![
            "-c".into(),
            "printf '%s' \"$$\" > owned.pid; trap 'exit 0' TERM; while :; do sleep 1; done".into(),
        ],
        directory: directory.to_owned(),
        environment: BTreeMap::new(),
        timeout: Duration::from_secs(2),
        stdout_limit: 4096,
        stderr_limit: 4096,
        input: Vec::new(),
        stdout: Sink::Capture,
        stderr: Sink::Capture,
        cleanup: false,
    }
}

#[tokio::test]
async fn cancellation_closes_and_reaps_the_owned_short_command_before_returning() {
    let directory = std::env::temp_dir().join(format!("benchmark-cancel-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let host = Arc::new(NativeHost::default());
    let owner = host.clone();
    let request = command(&directory);
    let task = tokio::spawn(async move { run(owner.as_ref(), request).await });
    let mut pid = None;
    for _ in 0..200 {
        if let Ok(text) = std::fs::read_to_string(directory.join("owned.pid")) {
            pid = text.parse::<i32>().ok();
            if pid.is_some() {
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let pid = pid.expect("The fixture must reach its owned ready marker");
    host.cancel(143);
    let result = tokio::time::timeout(Duration::from_secs(7), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.err().unwrap().to_string(), "Benchmark interrupted");
    assert_eq!(host.code(), 143);
    assert_eq!(
        nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None),
        Err(nix::errno::Errno::ESRCH),
        "The exact fixture leader must be reaped"
    );
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn cancellation_refuses_new_work_but_allows_explicit_owned_cleanup() {
    let directory =
        std::env::temp_dir().join(format!("benchmark-cleanup-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let host = NativeHost::default();
    host.cancel(130);
    assert!(run(&host, command(&directory)).await.is_err());
    assert!(!directory.join("owned.pid").exists());
    let mut cleanup = command(&directory);
    cleanup.executable = "/usr/bin/printf".into();
    cleanup.arguments = vec![
        "%s".into(),
        "literal $(touch unowned) ; private fixture".into(),
    ];
    cleanup.cleanup = true;
    let output = run(&host, cleanup).await.unwrap();
    assert!(output.exit.success());
    assert_eq!(output.stdout, b"literal $(touch unowned) ; private fixture");
    assert!(!directory.join("unowned").exists());
    std::fs::remove_dir_all(directory).unwrap();
}
