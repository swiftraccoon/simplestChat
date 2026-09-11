//! One-way drain coordination. Admission guards cover only synchronous commits;
//! never hold one across an await. The process coordinator bounds each cleanup
//! stage and reports incomplete work before tearing down its runtime.

use std::future::Future;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::watch;

/// Shared, persistent drain notification, including for late subscribers.
#[derive(Clone)]
pub struct DrainSignal {
    state: watch::Sender<bool>,
    admission: Arc<Mutex<()>>,
}

impl Default for DrainSignal {
    fn default() -> Self {
        Self {
            state: watch::channel(false).0,
            admission: Arc::new(Mutex::new(())),
        }
    }
}

impl DrainSignal {
    /// Atomically closes admission and wakes all current and future waiters.
    pub fn begin_draining(&self) {
        let _guard = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        self.state.send_replace(true);
    }

    /// Whether shutdown has begun; drain cannot be reversed.
    pub fn is_draining(&self) -> bool {
        *self.state.borrow()
    }

    /// Serialize a short in-memory admission commit with drain initiation.
    pub(crate) fn admit(&self) -> anyhow::Result<MutexGuard<'_, ()>> {
        let guard = self
            .admission
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        anyhow::ensure!(!self.is_draining(), "Server shutting down");
        Ok(guard)
    }

    /// Completes immediately for late subscribers after shutdown has begun.
    pub async fn wait(&self) {
        let mut receiver = self.state.subscribe();
        while !*receiver.borrow_and_update() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Run a cleanup stage within its budget. Failure is logged and returned so the
/// caller can continue later stages without reporting a false successful exit.
pub async fn run_stage<E: std::fmt::Display>(
    stage: &'static str,
    budget: Duration,
    work: impl Future<Output = Result<(), E>>,
) -> anyhow::Result<()> {
    match tokio::time::timeout(budget, work).await {
        Ok(Ok(())) => {
            tracing::info!(stage, "Shutdown stage complete");
            Ok(())
        }
        Ok(Err(error)) => {
            tracing::error!(stage, %error, "Shutdown stage failed; continuing remaining cleanup");
            anyhow::bail!("Shutdown stage failed: {stage}")
        }
        Err(_) => {
            tracing::error!(
                stage,
                budget_ms = budget.as_millis(),
                "Shutdown stage timed out; unfinished work will be cancelled"
            );
            anyhow::bail!("Shutdown stage timed out: {stage}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn drain_wakes_existing_and_late_waiters_and_closes_admission() {
        let signal = DrainSignal::default();
        assert!(signal.admit().is_ok());
        let waiting = signal.clone();
        let task = tokio::spawn(async move { waiting.wait().await });
        tokio::task::yield_now().await;
        assert!(!task.is_finished());
        signal.begin_draining();
        signal.begin_draining();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), signal.wait())
            .await
            .unwrap();
        assert!(signal.admit().is_err());
    }

    #[tokio::test]
    async fn shutdown_stages_report_failure_and_timeout_without_skipping_later_work() {
        assert!(
            run_stage("failed test", Duration::from_secs(1), async {
                Err::<(), _>("test failure")
            })
            .await
            .is_err()
        );
        assert!(
            run_stage(
                "stalled test",
                Duration::from_millis(5),
                std::future::pending::<Result<(), String>>()
            )
            .await
            .is_err()
        );
        assert!(
            run_stage("following test", Duration::from_secs(1), async {
                Ok::<(), String>(())
            })
            .await
            .is_ok()
        );
    }
}
