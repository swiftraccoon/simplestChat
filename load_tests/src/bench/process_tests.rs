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

#[tokio::test]
async fn inherited_unix_stdin_is_duplex_and_capture_remains_separate_until_retirement() {
    use super::process::Host;
    let directory = std::env::temp_dir().join(format!("benchmark-duplex-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let node = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|path| path.join("node"))
        .find(|path| path.is_file())
        .expect("Pinned Node is required by helper fixtures");
    let host = NativeHost::default();
    let mut request = command(&directory);
    request.executable = node;
    request.arguments = vec![
        "-e".into(),
        concat!(
            "const net=require('node:net');",
            "const s=new net.Socket({fd:0,readable:true,writable:true});",
            "let data='';s.setEncoding('utf8');s.on('data',chunk=>{data+=chunk;",
            "if(data==='request\\n'){s.write('ack\\n');s.end(()=>s.destroy());",
            "process.stdout.write('{\"receipt\":true}');}});"
        )
        .into(),
    ];
    let (mut child, mut control) = host.spawn_controlled(request).unwrap();
    assert!(!child.owned_streams_closed());
    // A cancelled waiting read has consumed no bytes; the same retained owner
    // can subsequently deliver every fragment without restarting a frame.
    let mut waiting = [0; 2];
    assert!(
        tokio::time::timeout(Duration::from_millis(10), control.read(&mut waiting))
            .await
            .is_err()
    );
    control
        .write_bounded(b"req", Duration::from_secs(1))
        .await
        .unwrap();
    control.write(b"uest\n").await.unwrap();
    let mut bytes = Vec::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        while bytes.len() < 4 {
            let mut buffer = [0; 2];
            let count = control.read(&mut buffer).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&buffer[..count]);
        }
    })
    .await
    .unwrap();
    assert_eq!(bytes, b"ack\n");
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if child.poll().await.unwrap().is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        child
            .stop(Duration::from_millis(20))
            .await
            .unwrap()
            .success()
    );
    assert_eq!(child.output(), b"{\"receipt\":true}");
    assert!(child.owned_streams_closed());
    assert!(
        !child.retirement().unwrap().streams_closed,
        "owned pipe completion cannot prove the caller's control endpoint closed"
    );
    control.finish().unwrap();
    assert!(child.owned_streams_closed());
    assert!(!child.retirement().unwrap().streams_closed);
    std::fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn controlled_mode_refuses_byte_input_and_cancelled_dispatch_before_spawning() {
    use super::process::Host;
    let directory =
        std::env::temp_dir().join(format!("benchmark-duplex-refusal-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let host = NativeHost::default();
    let mut input = command(&directory);
    input.input = b"must not dispatch".to_vec();
    assert!(host.spawn_controlled(input).is_err());
    host.cancel(143);
    let error = host.spawn_controlled(command(&directory)).err().unwrap();
    assert_eq!(super::process::failure_status(&error), Some(143));
    assert!(!directory.join("owned.pid").exists());
    // Spawn failure still settles the parent's admitted Unix socket endpoint.
    let host = NativeHost::default();
    let mut missing = command(&directory);
    missing.executable = directory.join("missing");
    assert!(host.spawn_controlled(missing).is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
