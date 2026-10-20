//! Tests of `src/diagnostics.rs`.

use super::*;

#[test]
fn operation_kinds_serialize_as_their_labels() {
    for kind in [OperationKind::JoinRoom, OperationKind::ShutdownNotification] {
        assert_eq!(serde_json::to_value(kind).unwrap(), kind.label());
    }
    assert_eq!(
        OperationKind::SetPreferredLayers.label(),
        "set_preferred_layers"
    );
}
use std::sync::{Barrier, Mutex, mpsc as sync_mpsc};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn records(&self) -> Vec<serde_json::Value> {
        let bytes = self.0.lock().unwrap();
        std::str::from_utf8(&bytes)
            .unwrap()
            .lines()
            .map(|line| {
                assert!(line.len() <= 1024);
                serde_json::from_str(line).unwrap()
            })
            .collect()
    }
}

#[tokio::test]
async fn disabled_records_nothing_and_preserves_future_results() {
    let diagnostics = Diagnostics::default();
    assert!(diagnostics.inner.is_none());
    let operation = diagnostics.operation(OperationKind::Chat, None);
    assert_eq!(
        operation
            .scope(measure(Stage::Dispatch, async { 42 }))
            .await,
        42
    );
    operation.finish(Outcome::Ok);
    assert!(diagnostics.shutdown().await);
}

#[tokio::test]
async fn scopes_correlate_without_capturing_values_and_preserve_errors() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let id = Some(7);
    let operation = diagnostics.operation(OperationKind::JoinRoom, id);
    let result = operation
        .scope(measure_result(Stage::RoomPolicyLookup, async {
            Err::<(), _>("PRIVATE_MESSAGE_TOKEN_SDP_NAME")
        }))
        .await;
    assert!(result.is_err());
    operation.finish(Outcome::Error);
    assert!(diagnostics.shutdown().await);
    let records = output.records();
    assert_eq!(records.len(), 3);
    assert_eq!(records[0]["stage"], "room_policy_lookup");
    assert_eq!(records[0]["outcome"], "error");
    assert_eq!(records[0]["operationId"], records[1]["operationId"]);
    assert_eq!(records[1]["connectionId"], 7);
    assert!(records[1]["stage"].is_null());
    assert_eq!(records[2]["written"], 2);
    assert!(!serde_json::to_string(&records).unwrap().contains("PRIVATE"));
}

#[tokio::test]
async fn dropped_pending_scope_records_stage_and_operation_cancellation() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let operation = diagnostics.operation(OperationKind::Consume, None);
    {
        let mut future =
            Box::pin(operation.scope(measure(Stage::MediaConsume, std::future::pending::<()>())));
        assert!(futures_util::poll!(&mut future).is_pending());
    }
    drop(operation);
    assert!(diagnostics.shutdown().await);
    let records = output.records();
    assert_eq!(records[0]["outcome"], "cancelled");
    assert_eq!(records[1]["outcome"], "cancelled");
}

#[tokio::test]
async fn spawned_tasks_do_not_inherit_an_unrelated_operation() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let operation = diagnostics.operation(OperationKind::JoinRoom, None);
    operation
        .scope(async {
            tokio::spawn(measure(Stage::Dispatch, async {}))
                .await
                .unwrap();
        })
        .await;
    operation.finish(Outcome::Ok);
    assert!(diagnostics.shutdown().await);
    assert_eq!(output.records().len(), 2);
}

#[test]
fn record_limit_and_full_queue_are_nonblocking_and_counted() {
    let limits = Limits {
        max_records: 2,
        ..Limits::default()
    };
    let (diagnostics, mut receiver) = Diagnostics::channel(limits);
    for _ in 0..3 {
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
    }
    let inner = diagnostics.inner.as_ref().unwrap();
    assert_eq!(inner.shared.accepted.load(Relaxed), 2);
    assert_eq!(inner.shared.dropped.load(Relaxed), 1);
    assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).count(), 2);

    let (diagnostics, _receiver) = Diagnostics::channel(Limits::default());
    for _ in 0..QUEUE_CAPACITY + 1 {
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
    }
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64);
    assert_eq!(shared.dropped.load(Relaxed), 1);
}

#[test]
fn concurrent_producers_below_queue_capacity_do_not_lose_records() {
    const PRODUCERS: usize = 8;
    const PER_PRODUCER: usize = 16;
    let (diagnostics, mut receiver) = Diagnostics::channel(Limits::default());
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    // A registered producer does not prevent other producers from entering.
    let held = shared.enter().unwrap();
    let barrier = Barrier::new(PRODUCERS);
    std::thread::scope(|threads| {
        for _ in 0..PRODUCERS {
            threads.spawn(|| {
                barrier.wait();
                for _ in 0..PER_PRODUCER {
                    diagnostics
                        .operation(OperationKind::Chat, None)
                        .finish(Outcome::Ok);
                }
            });
        }
    });
    drop(held);
    let records: Vec<_> = std::iter::from_fn(|| receiver.try_recv().ok()).collect();
    let ids: std::collections::HashSet<_> =
        records.iter().map(|record| record.operation_id).collect();
    let expected = (PRODUCERS * PER_PRODUCER) as u64;
    assert_eq!(records.len() as u64, expected);
    assert_eq!(ids.len() as u64, expected);
    assert_eq!(shared.accepted.load(Relaxed), expected);
    assert_eq!(shared.reserved_records.load(Relaxed), expected);
    assert_eq!(shared.started_records.load(Relaxed), expected);
    assert_eq!(shared.finalized_records.load(Relaxed), expected);
    assert_eq!(shared.dropped.load(Relaxed), 0);
    assert_eq!(shared.producers.load(Relaxed), 0);
}

#[test]
fn concurrent_record_reservations_enforce_the_limit() {
    let limits = Limits {
        max_records: 64,
        ..Limits::default()
    };
    let (diagnostics, mut receiver) = Diagnostics::channel(limits);
    let barrier = Barrier::new(8);
    std::thread::scope(|threads| {
        for _ in 0..8 {
            threads.spawn(|| {
                barrier.wait();
                for _ in 0..16 {
                    diagnostics
                        .operation(OperationKind::Chat, None)
                        .finish(Outcome::Ok);
                }
            });
        }
    });
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    assert_eq!(std::iter::from_fn(|| receiver.try_recv().ok()).count(), 64);
    assert_eq!(shared.accepted.load(Relaxed), 64);
    assert_eq!(shared.reserved_records.load(Relaxed), 64);
    assert_eq!(shared.dropped.load(Relaxed), 64);
    assert_eq!(
        shared.started_records.load(Relaxed),
        shared.finalized_records.load(Relaxed)
    );
}

#[test]
fn failed_enqueue_refunds_its_record_reservation() {
    let limits = Limits {
        max_records: QUEUE_CAPACITY as u64 + 1,
        ..Limits::default()
    };
    let (diagnostics, mut receiver) = Diagnostics::channel(limits);
    for _ in 0..QUEUE_CAPACITY + 1 {
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
    }
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    assert_eq!(shared.dropped.load(Relaxed), 1);
    assert_eq!(shared.reserved_records.load(Relaxed), QUEUE_CAPACITY as u64);
    receiver.try_recv().unwrap();
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64 + 1);
    assert_eq!(
        shared.reserved_records.load(Relaxed),
        QUEUE_CAPACITY as u64 + 1
    );
    assert_eq!(shared.dropped.load(Relaxed), 1);
}

#[test]
fn duration_limit_and_disconnected_writer_are_observable() {
    let (diagnostics, mut receiver) = Diagnostics::channel(Limits {
        duration: Duration::ZERO,
        ..Limits::default()
    });
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    assert_eq!(
        diagnostics
            .inner
            .as_ref()
            .unwrap()
            .shared
            .expired
            .load(Relaxed),
        1
    );
    assert!(receiver.try_recv().is_err());
    let (diagnostics, receiver) = Diagnostics::channel(Limits::default());
    drop(receiver);
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    assert_eq!(
        diagnostics
            .inner
            .as_ref()
            .unwrap()
            .shared
            .dropped
            .load(Relaxed),
        1
    );
}

#[tokio::test]
async fn output_failure_never_changes_the_operation_result() {
    struct Broken;
    impl Write for Broken {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("failed fixture"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Err(io::Error::other("failed fixture"))
        }
    }
    let diagnostics = Diagnostics::with_writer(Broken, Limits::default()).unwrap();
    let operation = diagnostics.operation(OperationKind::Chat, None);
    assert_eq!(operation.scope(async { 42 }).await, 42);
    operation.finish(Outcome::Ok);
    assert!(!diagnostics.shutdown().await);
    assert_eq!(
        diagnostics
            .inner
            .as_ref()
            .unwrap()
            .shared
            .written
            .load(Relaxed),
        0
    );
}

#[tokio::test]
async fn blocked_writer_cannot_extend_shutdown_past_its_budget() {
    struct Blocked {
        started: sync_mpsc::Sender<()>,
        release: sync_mpsc::Receiver<()>,
    }
    impl Write for Blocked {
        fn write(&mut self, data: &[u8]) -> io::Result<usize> {
            self.started.send(()).ok();
            self.release.recv_timeout(Duration::from_secs(2)).ok();
            Ok(data.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (started_tx, started_rx) = sync_mpsc::channel();
    let (release_tx, release_rx) = sync_mpsc::channel();
    let diagnostics = Diagnostics::with_writer(
        Blocked {
            started: started_tx,
            release: release_rx,
        },
        Limits::default(),
    )
    .unwrap();
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
    for _ in 0..QUEUE_CAPACITY + 1 {
        diagnostics
            .operation(OperationKind::Chat, None)
            .finish(Outcome::Ok);
    }
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    assert_eq!(shared.accepted.load(Relaxed), QUEUE_CAPACITY as u64 + 1);
    assert_eq!(shared.dropped.load(Relaxed), 1);
    assert!(!diagnostics.shutdown().await);
    release_tx.send(()).unwrap();
    drop(release_tx);
    assert!(diagnostics.shutdown().await);
}

#[tokio::test]
async fn shutdown_reports_unfinished_timers_and_rejects_late_records() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let operation = diagnostics.operation(OperationKind::Consume, None);
    let mut future =
        Box::pin(operation.scope(measure(Stage::MediaConsume, std::future::pending::<()>())));
    assert!(futures_util::poll!(&mut future).is_pending());
    assert!(diagnostics.shutdown().await);
    let records = output.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["unfinished"], 2);
    drop(future);
    drop(operation);
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    assert!(diagnostics.shutdown().await);
    assert_eq!(output.records(), records);
}

#[tokio::test]
async fn close_racing_producers_accounts_for_every_admitted_timer() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let barrier = Arc::new(Barrier::new(9));
    let admitted = Arc::new(AtomicU64::new(0));
    let mut threads = Vec::new();
    for _ in 0..8 {
        let diagnostics = diagnostics.clone();
        let barrier = barrier.clone();
        let admitted = admitted.clone();
        threads.push(std::thread::spawn(move || {
            let first = diagnostics.operation(OperationKind::Chat, None);
            admitted.fetch_add(u64::from(first.active.is_some()), Relaxed);
            barrier.wait();
            first.finish(Outcome::Ok);
            for _ in 0..15 {
                let operation = diagnostics.operation(OperationKind::Chat, None);
                admitted.fetch_add(u64::from(operation.active.is_some()), Relaxed);
                operation.finish(Outcome::Ok);
            }
        }));
    }
    barrier.wait();
    let completed = diagnostics.shutdown().await;
    for thread in threads {
        thread.join().unwrap();
    }
    assert!(completed);
    let records = output.records();
    let summary = records.last().unwrap();
    let accepted = summary["accepted"].as_u64().unwrap();
    let unfinished = summary["unfinished"].as_u64().unwrap();
    assert_eq!(accepted + unfinished, admitted.load(Relaxed));
    assert_eq!(accepted as usize, records.len() - 1);
    assert_eq!(summary["written"], accepted);
    assert_eq!(summary["dropped"], 0);
    assert_eq!(summary["expired"], 0);
    assert!(
        diagnostics
            .operation(OperationKind::Chat, None)
            .active
            .is_none()
    );
    assert_eq!(output.records(), records);
}

#[tokio::test]
async fn writer_waits_for_registered_sections_before_its_terminal_snapshot() {
    let output = Buffer::default();
    let diagnostics = Diagnostics::with_writer(output.clone(), Limits::default()).unwrap();
    let shared = &diagnostics.inner.as_ref().unwrap().shared;
    let section = shared.enter().unwrap();
    let mut shutdown = Box::pin(diagnostics.shutdown());
    assert!(futures_util::poll!(&mut shutdown).is_pending());
    assert_eq!(shared.producers.load(Relaxed), CLOSED + 1);
    assert!(shared.enter().is_none());
    assert!(!shared.finished.load(Acquire));
    // Model an already-admitted timer start finishing its short section.
    shared.started_records.fetch_add(1, Relaxed);
    drop(section);
    assert!(shutdown.await);
    let records = output.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["unfinished"], 1);
}

#[tokio::test]
async fn concurrent_shutdown_callers_observe_the_same_writer_completion() {
    let diagnostics = Diagnostics::with_writer(Buffer::default(), Limits::default()).unwrap();
    diagnostics
        .operation(OperationKind::Chat, None)
        .finish(Outcome::Ok);
    let (first, second) = tokio::join!(diagnostics.shutdown(), diagnostics.shutdown());
    assert!(first && second);
}

#[tokio::test]
async fn output_is_private_create_new_and_limits_are_validated() {
    let directory =
        std::env::temp_dir().join(format!("simplestchat-diagnostics-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let path = directory.join("events.jsonl");
    let diagnostics = Diagnostics::open(&path, Limits::default()).unwrap();
    assert!(Diagnostics::open(&path, Limits::default()).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let link = directory.join("link");
        symlink(&path, &link).unwrap();
        assert!(Diagnostics::open(&link, Limits::default()).is_err());
        std::fs::remove_file(link).unwrap();
    }
    assert!(diagnostics.shutdown().await);
    assert!(Diagnostics::open(Path::new("relative"), Limits::default()).is_err());
    assert!(
        Limits {
            max_records: 100_001,
            ..Limits::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Limits {
            max_records: 0,
            ..Limits::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Limits {
            duration: Duration::ZERO,
            ..Limits::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        Limits {
            duration: Duration::from_secs(3601),
            ..Limits::default()
        }
        .validate()
        .is_err()
    );
    std::fs::remove_file(path).unwrap();
    std::fs::remove_dir(directory).unwrap();
}
