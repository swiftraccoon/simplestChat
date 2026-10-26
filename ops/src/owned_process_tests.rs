//! Original pure Darwin group proof cases, alongside their actual shared implementation.

#[test]
fn darwin_group_exception_requires_successful_bounded_zombie_inventory_with_the_reserved_leader() {
    let pid = rustix::process::Pid::from_raw(111).unwrap();
    for rows in ["111 111 Z\n", " 111 111 Z+\n112 111 ZNs\n"] {
        assert!(super::cleanup::proof(rows.as_bytes(), b"", true, pid));
    }
    for rows in [
        "",
        "112 111 Z\n",
        "111 111 R\n",
        "111 111 Z\n112 111 S\n",
        "111 999 Z\n",
        "111 111 Z?\n",
        "0111 111 Z\n",
        "111 0111 Z\n",
        "111 111 Z\n111 111 Z\n",
        "111 111 Z PRIVATE\n",
        "2147483648 111 Z\n",
        "-111 111 Z\n",
        "111 111 Z\n\n",
    ] {
        assert!(
            !super::cleanup::proof(rows.as_bytes(), b"", true, pid),
            "{rows:?}"
        );
    }
    assert!(!super::cleanup::proof(
        b"111 111 Z\n",
        b"PRIVATE",
        true,
        pid
    ));
    assert!(!super::cleanup::proof(b"111 111 Z\n", b"", false, pid));
    assert!(!super::cleanup::proof(b"\xff", b"", true, pid));
    assert!(!super::cleanup::proof(&vec![b' '; 65537], b"", true, pid));
}

#[tokio::test]
async fn expired_cleanup_still_signals_the_original_group_and_keeps_the_result_negative() {
    use std::{os::unix::process::CommandExt, process::Command, time::Duration};

    let child = Command::new("/bin/sh")
        .args(["-c", "sleep 10"])
        .process_group(0)
        .spawn()
        .unwrap();
    let mut group = super::Group::new(child);
    let pid = group.pid();
    assert!(super::reserved(pid));
    assert!(!super::finish_before(pid, std::time::Instant::now() - Duration::from_secs(1)).await);
    tokio::time::timeout(Duration::from_secs(2), super::exited(pid))
        .await
        .unwrap()
        .unwrap();
    assert!(super::reserved(pid));
    let status = group.reap().await.unwrap();
    use std::os::unix::process::ExitStatusExt;
    assert_eq!(status.signal(), Some(9));
}

#[test]
fn public_owners_preserve_send_and_sync_contracts() {
    fn assert_send_sync<T: Send + Sync>() {}

    assert_send_sync::<super::Group>();
    assert_send_sync::<super::Signals>();
    assert_send_sync::<super::CompletionSignals>();
}

#[test]
fn completion_signals_survive_runtime_teardown_in_actual_owned_children() {
    let Ok(number) = std::env::var("SIMPLESTCHAT_COMPLETION_SIGNAL") else {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(completion_signal_children());
        return;
    };
    assert!(matches!(number.as_str(), "INT" | "TERM"));
    let mut owner = super::CompletionSignals::new().unwrap();
    assert_eq!(owner.status(), None);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        tokio::task::yield_now().await;
    });
    drop(runtime);
    let (signal, code) = if number == "INT" {
        (rustix::process::Signal::INT, 130)
    } else {
        (rustix::process::Signal::TERM, 143)
    };
    rustix::process::kill_process(rustix::process::getpid(), signal).unwrap();
    let end = std::time::Instant::now() + std::time::Duration::from_secs(2);
    while owner.status().is_none() && std::time::Instant::now() < end {
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
    let observed = owner.status();
    let repeated = owner.status();
    let finished = owner.finish();
    assert_eq!(observed, Some(code));
    assert_eq!(repeated, Some(code));
    assert_eq!(finished.unwrap(), Some(code));
    // The final observation is the same retained atomic, not another registration.
    assert_eq!(owner.status(), Some(code));
}

async fn completion_signal_children() {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let mut facts = Vec::new();
    for signal in ["INT", "TERM"] {
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                concat!(
                    "owned_process::tests::",
                    "completion_signals_survive_runtime_teardown_in_actual_owned_children"
                ),
                "--nocapture",
            ])
            .env("SIMPLESTCHAT_COMPLETION_SIGNAL", signal)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut group = super::Group::new(child);
        let end = Instant::now() + Duration::from_secs(10);
        let terminal = tokio::time::timeout_at(end.into(), super::exited(group.pid())).await;
        let retired = super::finish_before(group.pid(), end).await;
        let reaped = group.reap().await;
        facts.push((terminal, retired, reaped));
    }
    for (terminal, retired, reaped) in facts {
        assert!(matches!(terminal, Ok(Ok(()))) && retired);
        assert!(reaped.unwrap().success());
    }
}

#[test]
fn fixed_owner_errors_use_lowercase_privacy_safe_display() {
    for (error, expected) in [
        (super::Error::Supervision, "owned child supervision failed"),
        (super::Error::Wait, "owned child wait failed"),
        (super::Error::Reap, "owned child reap failed"),
    ] {
        assert_eq!(error.to_string(), expected);
    }
}

#[test]
fn disarmed_group_debug_reads_only_stored_identity_and_owner_state() {
    let group = super::Group {
        child: None,
        pid: rustix::process::Pid::from_raw(123).unwrap(),
    };
    // This synthetic state owns no process. Formatting cannot wait, signal or
    // inspect a live/recycled PID merely to learn whether its slot is empty.
    assert_eq!(
        format!("{group:?}"),
        "Group { pid: 123, reap_pending: false }"
    );
    assert!(group.child.is_none());
    assert_eq!(group.pid.as_raw_nonzero().get(), 123);
}

#[tokio::test]
async fn signals_debug_exposes_only_registered_kinds_without_polling_events() {
    let signals = super::Signals::new().unwrap();
    let expected = "Signals { interrupt_registered: true, terminate_registered: true }";
    // Construction registers owned receivers; this test sends and polls no
    // signals. Debug takes only &self and never queries either receiver.
    assert_eq!(format!("{signals:?}"), expected);
    assert_eq!(format!("{signals:?}"), expected);
}
