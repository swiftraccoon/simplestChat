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
