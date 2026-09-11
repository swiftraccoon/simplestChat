//! Bounded readiness probes sharing the process admission/drain signal.

use crate::shutdown::DrainSignal;
use sqlx::PgPool;
use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;

pub(super) const PROBE_TIMEOUT: Duration = Duration::from_secs(1);
const MAX_CONCURRENT_PROBES: usize = 4;

/// Clones share probe capacity and process drain intent. HTTP, connection, and
/// room consumers observe that intent; the coordinator owns cleanup deadlines.
#[derive(Clone)]
pub(super) struct ReadinessState {
    draining: DrainSignal,
    probes: Arc<Semaphore>,
}

impl Default for ReadinessState {
    fn default() -> Self {
        Self::new(DrainSignal::default())
    }
}

impl ReadinessState {
    pub(super) fn new(draining: DrainSignal) -> Self {
        Self {
            draining,
            probes: Arc::new(Semaphore::new(MAX_CONCURRENT_PROBES)),
        }
    }

    pub(super) fn begin_draining(&self) {
        self.draining.begin_draining();
    }

    pub(super) async fn check(&self, probe: impl Future<Output = bool>) -> bool {
        if self.draining.is_draining() {
            return false;
        }
        let Ok(_permit) = self.probes.try_acquire() else {
            return false;
        };
        // Includes worker-state locks, pool acquisition, and the SQL round trip.
        let ready = tokio::time::timeout(PROBE_TIMEOUT, probe)
            .await
            .unwrap_or(false);
        ready && !self.draining.is_draining()
    }
}

pub(super) async fn database_ready(pool: Option<&PgPool>) -> bool {
    match pool {
        None => true,
        Some(pool) => sqlx::query_scalar::<_, i32>("SELECT 1")
            .fetch_one(pool)
            .await
            .is_ok_and(|value| value == 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};

    #[tokio::test]
    async fn readiness_accepts_guest_only_mode_and_rejects_unavailable_dependencies() {
        let state = ReadinessState::default();
        assert!(state.check(database_ready(None)).await);
        assert!(!state.check(async { false }).await);
        assert!(state.check(async { true }).await);
    }

    #[tokio::test]
    async fn draining_is_shared_one_way_and_does_not_start_new_probes() {
        let state = ReadinessState::default();
        let other = state.clone();
        other.begin_draining();
        state.begin_draining();
        assert!(
            !state
                .check(async { panic!("draining must not poll dependencies") })
                .await
        );
        assert!(!other.check(async { true }).await);
    }

    #[tokio::test]
    async fn draining_during_a_successful_probe_still_returns_unready() {
        let state = ReadinessState::default();
        assert!(
            !state
                .check(async {
                    state.clone().begin_draining();
                    true
                })
                .await
        );
    }

    #[tokio::test]
    async fn probe_capacity_is_bounded_and_released() {
        let state = ReadinessState::default();
        let permits: Vec<_> = (0..MAX_CONCURRENT_PROBES)
            .map(|_| state.probes.try_acquire().unwrap())
            .collect();
        assert!(
            !state
                .check(async { panic!("busy probe must not poll dependencies") })
                .await
        );
        drop(permits);
        assert!(state.check(async { true }).await);
        assert_eq!(state.probes.available_permits(), MAX_CONCURRENT_PROBES);
    }

    #[tokio::test]
    async fn cancelled_probe_releases_its_permit() {
        let state = ReadinessState::default();
        let running = state.clone();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            running
                .check(async {
                    let _ = entered_tx.send(());
                    std::future::pending::<bool>().await
                })
                .await
        });
        let entered = tokio::time::timeout(PROBE_TIMEOUT * 3, entered_rx).await;
        task.abort();
        let _ = task.await;
        entered
            .expect("probe did not start")
            .expect("probe did not enter dependency check");
        assert_eq!(state.probes.available_permits(), MAX_CONCURRENT_PROBES);
    }

    #[tokio::test]
    async fn closed_configured_database_is_unready() {
        let pool = PgPoolOptions::new().connect_lazy_with(
            PgConnectOptions::new()
                .host("127.0.0.1")
                .port(9)
                .username("readiness_test")
                .password("")
                .database("readiness_test"),
        );
        // Close before querying: this case cannot contact any database.
        pool.close().await;
        assert!(
            !ReadinessState::default()
                .check(database_ready(Some(&pool)))
                .await
        );
    }

    #[tokio::test]
    async fn stalled_configured_database_times_out_and_releases_probe_capacity() {
        // An owned ephemeral loopback listener accepts TCP but never speaks
        // PostgreSQL. No existing database or service is contacted or stopped.
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let socket_task = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            let _ = accepted_tx.send(());
            std::future::pending::<()>().await;
            drop(connection);
        });
        let pool = PgPoolOptions::new().max_connections(1).connect_lazy_with(
            PgConnectOptions::new()
                .host("127.0.0.1")
                .port(port)
                .username("readiness_test")
                .password("")
                .database("readiness_test")
                .ssl_mode(PgSslMode::Disable),
        );
        let state = ReadinessState::default();
        let started = std::time::Instant::now();
        let result =
            tokio::time::timeout(PROBE_TIMEOUT * 3, state.check(database_ready(Some(&pool)))).await;
        socket_task.abort();
        let _ = socket_task.await;
        let close = tokio::time::timeout(PROBE_TIMEOUT * 3, pool.close()).await;
        assert!(!result.expect("probe exceeded its overall timeout"));
        accepted_rx
            .await
            .expect("database probe did not use the owned listener");
        assert!(
            started.elapsed() >= PROBE_TIMEOUT,
            "must exercise the timeout, not an immediate connection error"
        );
        close.expect("owned probe pool did not close");
        assert_eq!(state.probes.available_permits(), MAX_CONCURRENT_PROBES);
    }
}
