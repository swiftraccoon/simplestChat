use super::{
    config::Config,
    controller::{self, Services},
    fixture::{self, Fake},
    io::Frame,
};
use serde_json::{Value, json};
use std::path::Path;

async fn execute(options: &Value) -> (controller::Outcome, std::rc::Rc<Fake>) {
    let fake = Fake::new(options);
    let mut signals = fixture::Signals(fake.clone());
    let wire = fixture::Transport(fake.clone());
    let cfg = Config::read(
        &[
            "--origin".into(),
            "https://chat.example.com".into(),
            "--output".into(),
            "/tmp/public-smoke-unused".into(),
        ],
        Path::new("/tmp"),
        options["tlsDisabled"].as_bool() == Some(true),
        fixture::ID,
    )
    .unwrap();
    let mut services = Services {
        wire: &wire,
        clock: fake.as_ref(),
        signals: &mut signals,
    };
    let result = controller::run(
        &cfg,
        &mut services,
        fixture::Output(fake.clone()),
        fixture::ID.into(),
    )
    .await;
    (result, fake)
}
pub(super) async fn oracle_case(options: &Value) -> Value {
    let (result, fake) = execute(options).await;
    let mut summary = result.summary;
    summary["runId"] = json!("RUN");
    summary["startedAt"] = json!("TIME");
    summary["durationMs"] = json!(0);
    json!({"status":result.status,"summary":summary,"trace":fake.trace()})
}
#[tokio::test]
async fn complete_controller_queues_exactly_one_chat_and_joins_every_acquired_socket() {
    let (outcome, fake) = execute(&json!({})).await;
    assert_eq!(outcome.status, 0);
    assert_eq!(outcome.summary["passed"], true);
    assert_eq!(fake.finished.get(), 2);
    assert!(fake.artifact_closed.get());
    assert_eq!(
        fake.trace()["sent"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["type"] == "chatMessage")
            .count(),
        1
    );
    assert!(
        !serde_json::to_string(&outcome.summary)
            .unwrap()
            .contains("guest-0")
    );
}
#[tokio::test]
async fn acquired_socket_survives_late_cancel_and_deadline_and_artifacts_always_close() {
    for (options, status) in [
        (json!({"connectCancel":true}), 130),
        (json!({"connectLate":true}), 124),
        (json!({"requestCancel":true}), 143),
    ] {
        let (outcome, fake) = execute(&options).await;
        assert_eq!(outcome.status, status, "{options}");
        assert_eq!(
            fake.finished.get(),
            fake.trace()["connects"].as_array().unwrap().len()
        );
        assert!(fake.artifact_closed.get());
    }
}
#[tokio::test]
async fn publication_is_prefinal_and_cli_fences_preserve_first_known_failure() {
    for (options, status) in [
        (json!({"publishSignal":130}), 130),
        (json!({"publishSignal":143}), 143),
        (json!({"publishDeadline":true}), 124),
        (json!({"artifactCloseFailure":true}), 1),
        (json!({"publishFailure":true}), 1),
    ] {
        let (outcome, fake) = execute(&options).await;
        assert_eq!(outcome.status, status);
        assert_eq!(outcome.summary["passed"], true);
        assert!(fake.artifact_closed.get());
        assert_eq!(fake.finished.get(), 2);
    }
    let (outcome, _) = execute(&json!({"httpStatus":503,"publishSignal":143})).await;
    assert_eq!(outcome.status, 1);
}
#[tokio::test]
async fn close_owners_are_concurrent_and_still_joined_after_observed_cancellation() {
    tokio::time::timeout(std::time::Duration::from_secs(2),async {
    for status in [143,124] {
    let fake = Fake::new(&json!({"holdClose":true}));
    let wire = fixture::Transport(fake.clone());
    let mut signals = fixture::Signals(fake.clone());
    let cfg = Config::read(
        &["--origin".into(), "https://chat.example.com".into()],
        Path::new("/tmp"),
        false,
        fixture::ID,
    )
    .unwrap();
    let mut services = Services {
        wire: &wire,
        clock: fake.as_ref(),
        signals: &mut signals,
    };
    let future = controller::run(
        &cfg,
        &mut services,
        fixture::Output(fake.clone()),
        fixture::ID.into(),
    );
    tokio::pin!(future);
    loop {
        tokio::select! {
            result=&mut future => panic!("returned before retained close owners: {}",result.status),
            _=tokio::task::yield_now() => {},
        }
        if fake.close_started.get() == 2 {
            break;
        }
    }
    if status==143 {
        fake.signal.set(Some(143));
        fake.signal_wake.notify_waiters();
    } else { fake.clock.set(5.0); }
    tokio::select! {
        result=&mut future => panic!("cancel dropped a retained close owner: {}",result.status),
        _=tokio::task::yield_now() => {},
    }
    assert_eq!(fake.finished.get(), 0);
    assert!(!fake.artifact_closed.get());
    fake.release_close.notify_waiters();
    let result = future.await;
    assert_eq!(result.status, status);
    assert_eq!(fake.finished.get(), 2);
    assert!(fake.artifact_closed.get());
    }
    }).await.expect("held fake finalizers did not reach/settle their handshake");
}
#[tokio::test]
async fn accepted_chat_is_counted_once_even_when_its_flush_fails() {
    let (outcome, fake) = execute(&json!({"chatFlushFailure":true})).await;
    assert_eq!(outcome.status, 1);
    assert_eq!(outcome.summary["publicMessagesSent"], 1);
    assert_eq!(
        fake.trace()["sent"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["type"] == "chatMessage")
            .count(),
        1
    );
    assert_eq!(fake.finished.get(), 2);
}
#[tokio::test]
async fn exact_work_and_cleanup_boundaries_refuse_late_positive_observations() {
    for options in [
        json!({"lateResponse":true}),
        json!({"lateClose":true}),
        json!({"closeAtDeadline":true}),
    ] {
        let (outcome, fake) = execute(&options).await;
        assert_eq!(outcome.status, 124, "{options}");
        assert_eq!(outcome.summary["passed"], false);
        assert_eq!(outcome.summary["checks"]["webSocketCleanup"], false);
        assert!(fake.artifact_closed.get());
    }
}
#[tokio::test]
async fn already_observed_invalid_cleanup_clocks_precede_later_close_deadlines() {
    let (_, baseline) = execute(&json!({})).await;
    let start = baseline.first_close_clock_read.get().unwrap();
    // The first close sees the just-sampled cleanup start; the next sample is
    // cleanup end. No wall-clock delay or implementation call-count is assumed.
    for read in [start, start + 1] {
        for invalid in [Value::Null, json!(-1.0)] {
            let (outcome, fake) =
                execute(&json!({"invalidAtRead":read,"invalidClock":invalid})).await;
            assert_eq!(outcome.status, 1, "read{read}/{invalid}");
            assert_eq!(outcome.summary["failure"]["code"], "invalid_clock");
            assert_eq!(fake.finished.get(), 2);
            assert!(fake.artifact_closed.get());
        }
    }
    let (outcome, fake) = execute(&json!({"closeMissing":true,"invalidAtRead":start+1})).await;
    assert_eq!(outcome.status, 124);
    assert_eq!(outcome.summary["failure"]["code"], "cleanup_deadline");
    assert_eq!(fake.finished.get(), 2);
}
#[test]
fn raw_selected_json_matches_last_wins_and_ignores_private_extreme_numbers() {
    let frame =
        Frame::decode(br#"{"type":"roomClosed","type":"unrelated","PRIVATE":1e999}"#).unwrap();
    assert_eq!(frame.kind, "unrelated");
    assert!(!frame.rejected());
    for bytes in [
        b"null".as_slice(),
        b"[]",
        br#"{"type":null}"#,
        br#"{"type":"\ud800"}"#,
    ] {
        assert!(Frame::decode(bytes).is_err());
    }
}
#[test]
fn origin_room_output_are_admitted_before_any_owner_or_mutation() {
    for origin in [
        "http://example.com",
        "https://user:secret@example.com",
        "https://x/path",
        "https://x?x",
        "https://x#x",
        "https://x\\evil",
        "https://x\u{0085}",
    ] {
        assert!(
            Config::read(
                &["--origin".into(), origin.into()],
                Path::new("/tmp"),
                false,
                "id"
            )
            .is_err()
        );
    }
    assert!(
        Config::read(
            &["--origin".into(), "https://[::1]:8443/".into()],
            Path::new("/tmp"),
            false,
            "id"
        )
        .is_ok()
    );
}
