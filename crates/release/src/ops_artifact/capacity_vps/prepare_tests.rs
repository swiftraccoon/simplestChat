//! Actual owned pipe/group checks with inert executables, never SSH or Ansible.

use crate::ops_artifact::capacity_vps::tests::PreparationFixture;
use simplestchat_ci::process::{Executor, Native};
use simplestchat_toolkit::owned_process::Signals;
use std::time::Duration;

/// The lease reads exactly its request frame, answers the ready frame, then
/// requires exactly `release\n` followed by the end of its input. `dd bs=1`
/// reads one byte per call, so it never consumes a byte past its count.
const LEASE: &str = r#"
received=$(/bin/dd bs=1 count="$REQUEST" 2>/dev/null | /usr/bin/wc -c) || exit 1
[ "$((received))" -eq "$REQUEST" ] || exit 1
printf '%s' "$((received))" >"$ROOT/frame-count"
printf "$READY"
release=$(/bin/dd bs=1 count=8 2>/dev/null | /usr/bin/od -An -v -t x1)
set -- $release
[ "$*" = '72 65 6c 65 61 73 65 0a' ] || exit 1
remaining=$(/bin/dd bs=1 count=1 2>/dev/null | /usr/bin/wc -c)
[ "$((remaining))" -eq 0 ] || exit 1
[ -e "$ROOT/ansible-finished" ] || exit 1
printf released >"$ROOT/released"
"#;

#[tokio::test(flavor = "current_thread")]
async fn completed_preparation_closes_both_groups_and_streams_before_release() {
    const MAX_FUTURE_BYTES: usize = 32 * 1024;
    let signals = Signals::new().unwrap();
    let constructing = PreparationFixture::new(
        LEASE,
        r#"
printf 'private ansible output\n'
printf 'private ansible error\n' >&2
printf done >"$ROOT/ansible-finished"
"#,
        240,
    );
    let construction_compact = std::mem::size_of_val(&constructing) < MAX_FUTURE_BYTES;
    tokio::task::yield_now().await;
    let mut fixture = constructing.await;
    let invocation = fixture.take();
    let mut native = Native::new(&fixture.caller, &fixture.environment, signals);
    // Keep the actual production owner unboxed on the ordinary test stack.
    let preparing = native.vps_prepare(invocation, fixture.end);
    let preparation_compact = std::mem::size_of_val(&preparing) < MAX_FUTURE_BYTES;
    tokio::task::yield_now().await;
    let prepared = preparing.await;
    let cleanup_passed = native.cleanup_passed();
    let frame_count = std::fs::read_to_string(fixture.marker("frame-count"));
    let frame_bytes = fixture.frame_bytes;
    let released = fixture.marker("released").exists();
    let captures = fixture.captures();
    let ready_bytes = fixture.ready.len() as u64;
    let exits = fixture.exits();
    drop(native);
    let closed = fixture.finish();
    prepared.unwrap();
    closed.unwrap();
    assert!(construction_compact && preparation_compact && cleanup_passed);
    assert_eq!(frame_count.unwrap(), frame_bytes.to_string());
    assert!(released);
    assert_eq!(captures[0].as_ref().unwrap().stdout.bytes, ready_bytes);
    assert!(captures[1].as_ref().unwrap().stdout.bytes > 0);
    assert!(captures[1].as_ref().unwrap().stderr.bytes > 0);
    assert!(exits.iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status == Some(0) && v.settled)
    }));
}

#[tokio::test]
async fn late_lease_stdout_after_release_cannot_pass_the_completed_capture_fence() {
    let lease = format!("{LEASE}\nprintf 'late extra record\\n'\n");
    let mut fixture =
        PreparationFixture::new(&lease, "printf done >\"$ROOT/ansible-finished\"\n", 240).await;
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::new().unwrap(),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(fixture.marker("released").exists());
    assert!(native.cleanup_passed());
    assert!(fixture.captures()[0].as_ref().unwrap().stdout.bytes > fixture.ready.len() as u64);
    drop(native);
    fixture.finish().unwrap();
}

#[tokio::test]
async fn stderr_eof_cancels_quiet_ansible_while_the_lease_leader_and_stdout_remain() {
    let release = "release=$(";
    assert_eq!(LEASE.matches(release).count(), 1);
    let lease = LEASE.replace(
        release,
        r#"
while [ ! -e "$ROOT/ansible-started" ]; do /bin/sleep 0.005; done
exec 2>&-
while :; do /bin/sleep 1; done
release=$("#,
    );
    let mut fixture = PreparationFixture::new(
        &lease,
        r#"
printf started >"$ROOT/ansible-started"
/bin/sleep 30
printf unexpected >"$ROOT/ansible-finished"
"#,
        240,
    )
    .await;
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::new().unwrap(),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    assert_eq!(error.code, 1);
    assert!(fixture.marker("ansible-started").exists());
    assert!(!fixture.marker("ansible-finished").exists());
    assert!(native.cleanup_passed());
    assert!(fixture.exits().iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status.is_some() && v.settled)
    }));
    drop(native);
    fixture.finish().unwrap();
}

#[tokio::test]
async fn first_interrupt_survives_a_later_signal_during_joined_settlement() {
    let mut fixture = PreparationFixture::new(
        LEASE,
        r#"
trap 'printf seen >"$ROOT/ansible-interrupted"' INT TERM
printf started >"$ROOT/ansible-started"
while :; do /bin/sleep 1; done
"#,
        240,
    )
    .await;
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let started = fixture.marker("ansible-started");
    let interrupted = fixture.marker("ansible-interrupted");
    let injected = tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !started.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            sender.send(130).unwrap();
            while !interrupted.exists() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            sender.send(143).unwrap();
        })
        .await
        .unwrap();
    });
    let invocation = fixture.take();
    let mut native = Native::new(
        &fixture.caller,
        &fixture.environment,
        Signals::injected(receiver),
    );
    let error = native
        .vps_prepare(invocation, fixture.end)
        .await
        .unwrap_err();
    injected.await.unwrap();
    assert_eq!(error.code, 130);
    assert_eq!(native.ready().await.unwrap_err().code, 130);
    assert!(native.cleanup_passed());
    assert!(fixture.exits().iter().all(|value| {
        value
            .as_ref()
            .is_some_and(|v| v.status.is_some() && v.settled)
    }));
    drop(native);
    fixture.finish().unwrap();
}
