use super::metrics::{MetricsCollector, ReceiverStallTrigger};
use super::webrtc_client::WebRtcSession;
use anyhow::Result;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OwnedSemaphorePermit, Semaphore};

const CAPTURE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_CAPTURES: usize = 8;
const MAX_CONCURRENT_CAPTURES: usize = 2;

/// One budget shared by all clients and churn attempts in a diagnostic run.
/// Failed or canceled admitted captures consume their slot; there are no retries.
#[derive(Debug)]
pub struct ReceiverStallBudget {
    remaining: AtomicUsize,
    concurrent: Arc<Semaphore>,
}

impl Default for ReceiverStallBudget {
    fn default() -> Self {
        Self {
            remaining: AtomicUsize::new(MAX_CAPTURES),
            concurrent: Arc::new(Semaphore::new(MAX_CONCURRENT_CAPTURES)),
        }
    }
}

impl ReceiverStallBudget {
    fn claim(&self) -> std::result::Result<OwnedSemaphorePermit, CaptureStatus> {
        // Never queue behind another receiver's native work.
        let permit = self
            .concurrent
            .clone()
            .try_acquire_owned()
            .map_err(|_| CaptureStatus::Busy)?;
        self.remaining
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |remaining| {
                remaining.checked_sub(1)
            })
            .map_err(|_| CaptureStatus::BudgetExhausted)?;
        Ok(permit)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
enum CaptureStatus {
    Captured,
    Busy,
    BudgetExhausted,
    TimedOut,
    AttemptEnded,
    Unavailable,
    SnapshotRejected,
    Cancelled,
}

/// Scope-owned completion record: cancellation cannot leave a trigger looking
/// like a successful capture, nor attribute the missing evidence to a new attempt.
struct CaptureRecord<'a> {
    metrics: &'a MetricsCollector,
    attempt: usize,
    triggered_elapsed_ms: u64,
    finished: bool,
}

impl CaptureRecord<'_> {
    fn finish(&mut self, status: CaptureStatus) {
        self.finished = true;
        self.metrics.diagnostic_event_for_attempt(
            self.attempt,
            "receiver-stall-capture",
            serde_json::json!({
                "triggerElapsedMs": self.triggered_elapsed_ms,
                "status": status,
            }),
        );
        if status != CaptureStatus::Captured {
            // Fixed status vocabulary; never include native errors or SDP.
            self.metrics.diagnostic_failure_for_attempt(
                self.attempt,
                &format!("Receiver-stall capture {status:?}; evidence is incomplete"),
            );
        }
    }
}

impl Drop for CaptureRecord<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.finish(CaptureStatus::Cancelled);
        }
    }
}

fn active(metrics: &MetricsCollector, attempt: usize, deadline: Instant) -> bool {
    Instant::now() < deadline
        && metrics.diagnostic_attempt() == attempt
        && metrics.attempt_accepts_work(attempt)
}

async fn capture(
    metrics: &MetricsCollector,
    budget: &ReceiverStallBudget,
    attempt: usize,
    deadline: Instant,
    trigger: ReceiverStallTrigger,
    snapshot: impl std::future::Future<Output = Result<serde_json::Value>>,
) -> CaptureStatus {
    let triggered_elapsed_ms = metrics.diagnostic_elapsed_ms();
    metrics.diagnostic_event_for_attempt(
        attempt,
        "receiver-stall-triggered",
        serde_json::json!({
            "triggerElapsedMs": triggered_elapsed_ms,
            "trigger": trigger,
        }),
    );
    let mut record = CaptureRecord {
        metrics,
        attempt,
        triggered_elapsed_ms,
        finished: false,
    };
    let status = if !active(metrics, attempt, deadline) {
        CaptureStatus::AttemptEnded
    } else {
        match budget.claim() {
            Err(status) => status,
            Ok(_permit) => {
                let timeout = (Instant::now() + CAPTURE_TIMEOUT).min(deadline);
                let result = tokio::time::timeout_at(timeout.into(), snapshot).await;
                // An old native result cannot become a later attempt's evidence.
                if !active(metrics, attempt, deadline) {
                    CaptureStatus::AttemptEnded
                } else {
                    match result {
                        Err(_) => CaptureStatus::TimedOut,
                        Ok(Err(_)) => CaptureStatus::Unavailable,
                        Ok(Ok(snapshot)) => {
                            if metrics.receiver_stall_snapshot_for_attempt(
                                attempt,
                                triggered_elapsed_ms,
                                trigger,
                                snapshot,
                            ) {
                                CaptureStatus::Captured
                            } else {
                                CaptureStatus::SnapshotRejected
                            }
                        }
                    }
                }
            }
        }
    };
    record.finish(status);
    status
}

/// Wait for the existing session deadline, observing only already-collected
/// receive buckets once per completed shared-window second. No watcher task is
/// detached: dropping this future releases the snapshot future, lock and permit.
/// A trigger is one-shot even when capture admission or native collection fails.
pub async fn monitor_until_deadline(
    session: Arc<Mutex<WebRtcSession>>,
    metrics: Arc<MetricsCollector>,
    budget: Arc<ReceiverStallBudget>,
    attempt: usize,
    measurement_start: Instant,
    deadline: Instant,
) {
    if !metrics.diagnostics_enabled() {
        tokio::time::sleep_until(deadline.into()).await;
        return;
    }
    let elapsed = Instant::now().saturating_duration_since(measurement_start);
    let first_check = measurement_start + Duration::from_secs(elapsed.as_secs() + 1);
    let mut interval = tokio::time::interval_at(first_check.into(), Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            _ = tokio::time::sleep_until(deadline.into()) => return,
            _ = interval.tick() => {
                if !active(&metrics, attempt, deadline) {
                    break;
                }
                if let Some(trigger) = metrics.receiver_stall(attempt) {
                    capture(&metrics, &budget, attempt, deadline, trigger, async {
                        let receiver = session.lock().await;
                        // The two-second allowance includes session lock acquisition.
                        anyhow::ensure!(active(&metrics, attempt, deadline), "Attempt ended before receiver capture");
                        receiver.receive_diagnostic_snapshot().await
                    }).await;
                    break;
                }
            }
        }
    }
    // Capture never changes session duration, departure, or the pre-close path.
    tokio::time::sleep_until(deadline.into()).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metrics() -> Arc<MetricsCollector> {
        let metrics = Arc::new(MetricsCollector::new("owned-stall-test".into()));
        metrics.begin_connection_attempt();
        metrics.enable_diagnostics();
        metrics
    }

    fn trigger() -> ReceiverStallTrigger {
        ReceiverStallTrigger {
            consumer_ordinal: 1,
            ssrc: 42,
            is_audio: Some(false),
            begin_bucket: 3,
            end_bucket: 6,
        }
    }

    #[test]
    fn global_admission_is_non_waiting_bounded_and_never_refunds_attempts() {
        let budget = ReceiverStallBudget::default();
        let first = budget.claim().unwrap();
        let second = budget.claim().unwrap();
        assert!(matches!(budget.claim(), Err(CaptureStatus::Busy)));
        assert_eq!(budget.remaining.load(Ordering::Relaxed), 6);
        drop(first);
        drop(second);
        for _ in 0..6 {
            drop(budget.claim().unwrap());
        }
        assert!(matches!(
            budget.claim(),
            Err(CaptureStatus::BudgetExhausted)
        ));
        assert_eq!(budget.concurrent.available_permits(), 2);
    }

    #[tokio::test]
    async fn successful_capture_has_fixed_attempt_and_separate_pre_close_storage() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async { Ok(serde_json::json!({"transports": [{"direction": "receive"}]})) },
        )
        .await;
        assert_eq!(status, CaptureStatus::Captured);
        metrics.diagnostic_snapshot(serde_json::json!({"preclose": true}));
        let report = metrics.generate_report().diagnostics.unwrap();
        assert!(report.failures.is_empty());
        assert_eq!(report.snapshots.len(), 1);
        assert_eq!(report.snapshots[0].kind, "pre-close");
        assert_eq!(report.receiver_stalls.len(), 1);
        let stored = &report.receiver_stalls[0];
        assert_eq!(stored.attempt, 1);
        assert_eq!(stored.details["trigger"]["ssrc"], 42);
        assert!(stored.details["triggerElapsedMs"].as_u64().unwrap() <= stored.elapsed_ms);
        assert_eq!(report.events.len(), 2);
        assert_eq!(report.events[1].details["status"], "captured");
        assert_eq!(budget.concurrent.available_permits(), 2);
    }

    #[tokio::test]
    async fn denied_capture_does_not_poll_native_work_or_wait_for_capacity() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let _first = budget.claim().unwrap();
        let _second = budget.claim().unwrap();
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async {
                panic!("Denied capture must not poll native work");
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::Busy);
        let report = metrics.generate_report().diagnostics.unwrap();
        assert!(report.receiver_stalls.is_empty());
        assert_eq!(report.events[1].details["status"], "busy");
        assert_eq!(report.failures.len(), 1);
    }

    #[tokio::test]
    async fn capture_timeout_releases_session_lock_and_global_permit() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let lock = Mutex::new(());
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async {
                let _guard = lock.lock().await;
                std::future::pending::<Result<serde_json::Value>>().await
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::TimedOut);
        assert!(lock.try_lock().is_ok());
        assert_eq!(budget.concurrent.available_permits(), 2);
        assert_eq!(budget.remaining.load(Ordering::Relaxed), 7);
        assert!(
            metrics
                .generate_report()
                .diagnostics
                .unwrap()
                .receiver_stalls
                .is_empty()
        );
    }

    #[tokio::test]
    async fn capture_deadline_includes_lock_wait_and_does_not_delay_departure() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let lock = Mutex::new(());
        let _held = lock.lock().await;
        let started = Instant::now();
        let status = capture(
            &metrics,
            &budget,
            1,
            started + Duration::from_millis(10),
            trigger(),
            async {
                let _guard = lock.lock().await;
                panic!("Session lock must not become available in this fixture");
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::AttemptEnded);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(budget.concurrent.available_permits(), 2);
    }

    #[tokio::test]
    async fn cancellation_records_missing_evidence_on_the_original_attempt() {
        let metrics = metrics();
        let budget = Arc::new(ReceiverStallBudget::default());
        let lock = Arc::new(Mutex::new(()));
        let (started, ready) = tokio::sync::oneshot::channel();
        let mut tasks = tokio::task::JoinSet::new();
        tasks.spawn({
            let metrics = metrics.clone();
            let budget = budget.clone();
            let lock = lock.clone();
            async move {
                capture(
                    &metrics,
                    &budget,
                    1,
                    Instant::now() + Duration::from_secs(5),
                    trigger(),
                    async {
                        let _guard = lock.lock().await;
                        let _ = started.send(());
                        std::future::pending::<Result<serde_json::Value>>().await
                    },
                )
                .await
            }
        });
        tokio::time::timeout(Duration::from_secs(1), ready)
            .await
            .unwrap()
            .unwrap();
        metrics.begin_connection_attempt();
        tasks.shutdown().await;
        assert!(lock.try_lock().is_ok());
        assert_eq!(budget.concurrent.available_permits(), 2);
        let report = metrics.generate_report().diagnostics.unwrap();
        assert!(report.receiver_stalls.is_empty());
        assert_eq!(report.events[1].details["status"], "cancelled");
        assert!(report.events.iter().all(|event| event.attempt == 1));
        assert!(report.failures[0].starts_with("Attempt 1:"));
    }

    #[tokio::test]
    async fn late_results_and_native_errors_do_not_create_misleading_snapshots() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async {
                metrics.begin_connection_attempt();
                Ok(serde_json::json!({"stale": true}))
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::AttemptEnded);
        let status = capture(
            &metrics,
            &budget,
            2,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async {
                anyhow::bail!("PRIVATE native error credential/sdp detail");
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::Unavailable);
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async {
                panic!("A stale attempt must not query native stats");
            },
        )
        .await;
        assert_eq!(status, CaptureStatus::AttemptEnded);
        let report = metrics.generate_report().diagnostics.unwrap();
        assert!(report.receiver_stalls.is_empty());
        assert!(!serde_json::to_string(&report).unwrap().contains("PRIVATE"));
    }

    #[tokio::test]
    async fn oversized_snapshot_is_visible_failure_without_retaining_payload() {
        let metrics = metrics();
        let budget = ReceiverStallBudget::default();
        let status = capture(
            &metrics,
            &budget,
            1,
            Instant::now() + Duration::from_secs(5),
            trigger(),
            async { Ok(serde_json::json!({"oversized": "x".repeat(256 * 1024)})) },
        )
        .await;
        assert_eq!(status, CaptureStatus::SnapshotRejected);
        let report = metrics.generate_report().diagnostics.unwrap();
        assert!(report.receiver_stalls.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(
            !serde_json::to_string(&report)
                .unwrap()
                .contains("oversized")
        );
    }

    #[tokio::test]
    async fn disabled_monitor_has_no_capture_side_effects_or_session_lock_wait() {
        let metrics = Arc::new(MetricsCollector::new("disabled-monitor".into()));
        metrics.begin_connection_attempt();
        let budget = Arc::new(ReceiverStallBudget::default());
        let session = Arc::new(Mutex::new(WebRtcSession::new(
            "disabled-monitor".into(),
            metrics.clone(),
        )));
        let _held = session.lock().await;
        let now = Instant::now();
        monitor_until_deadline(
            session.clone(),
            metrics.clone(),
            budget.clone(),
            1,
            now,
            now + Duration::from_millis(10),
        )
        .await;
        assert_eq!(budget.remaining.load(Ordering::Relaxed), MAX_CAPTURES);
        assert!(metrics.generate_report().diagnostics.is_none());
    }
}
