//! Explicitly enabled, authenticated, bounded native media snapshots. This is
//! an on-demand diagnostic path, never a packet callback or a background poller.

use super::{SignalingServer, constant_time_eq};
use crate::media::{TransportManager, diagnostics::SnapshotContext};
use crate::shutdown::DrainSignal;
use axum::{
    Json,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, SemaphorePermit};

const MIN_START_INTERVAL: Duration = Duration::from_millis(250);

/// Clones of the signaling server share one namespace, one in-flight request,
/// and a minimum start interval. Failed authentication never consumes capacity.
pub(super) struct MediaDiagnostics {
    context: SnapshotContext,
    active: Semaphore,
    last_started: Mutex<Option<Instant>>,
}

impl MediaDiagnostics {
    /// Disabled configuration does not create a namespace or schedule work.
    pub(super) fn configure(
        enabled: bool,
        metrics_token: Option<&str>,
    ) -> anyhow::Result<Option<Arc<Self>>> {
        if !enabled {
            return Ok(None);
        }
        anyhow::ensure!(
            metrics_token.is_some_and(|token| token.len() >= 32),
            "MEDIA_DIAGNOSTICS_ENABLED requires METRICS_TOKEN of at least 32 bytes"
        );
        Ok(Some(Arc::new(Self {
            context: SnapshotContext::new(),
            active: Semaphore::new(1),
            last_started: Mutex::new(None),
        })))
    }

    fn try_acquire_at(&self, now: Instant) -> Result<SemaphorePermit<'_>, StatusCode> {
        let permit = self
            .active
            .try_acquire()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        let mut last_started = self
            .last_started
            .try_lock()
            .map_err(|_| StatusCode::TOO_MANY_REQUESTS)?;
        if last_started.is_some_and(|last| now.saturating_duration_since(last) < MIN_START_INTERVAL)
        {
            return Err(StatusCode::TOO_MANY_REQUESTS);
        }
        *last_started = Some(now);
        Ok(permit)
    }
}

fn authorize(expected: &str, headers: &HeaderMap) -> bool {
    // Reject ambiguous duplicate authorization headers instead of selecting one.
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let provided = values
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    values.next().is_none()
        && provided
            .is_some_and(|provided| constant_time_eq(provided.as_bytes(), expected.as_bytes()))
}

fn uncached_status(status: StatusCode) -> Response {
    (status, [(header::CACHE_CONTROL, "no-store")]).into_response()
}

async fn capture_response(
    diagnostics: Option<&MediaDiagnostics>,
    expected: Option<&str>,
    headers: &HeaderMap,
    draining: &DrainSignal,
    manager: &TransportManager,
) -> Response {
    let (Some(diagnostics), Some(expected)) = (diagnostics, expected) else {
        return uncached_status(StatusCode::NOT_FOUND);
    };
    if !authorize(expected, headers) {
        return uncached_status(StatusCode::UNAUTHORIZED);
    }
    if draining.is_draining() {
        return uncached_status(StatusCode::SERVICE_UNAVAILABLE);
    }
    let _permit = match diagnostics.try_acquire_at(Instant::now()) {
        Ok(permit) => permit,
        Err(status) => return uncached_status(status),
    };
    // The collector bounds native calls and entity counts. Keep capacity until
    // serialization completes; cancelling the HTTP future releases it too.
    let snapshot = manager.diagnostic_snapshot(&diagnostics.context).await;
    if draining.is_draining() {
        return uncached_status(StatusCode::SERVICE_UNAVAILABLE);
    }
    ([(header::CACHE_CONTROL, "no-store")], Json(snapshot)).into_response()
}

pub(super) async fn handler(State(server): State<SignalingServer>, headers: HeaderMap) -> Response {
    capture_response(
        server.media_diagnostics.as_deref(),
        server.metrics_token.as_deref(),
        &headers,
        &server.room_manager.drain_signal(),
        &server.room_manager.media_server().transport_manager(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const TOKEN: &str = "owned-local-diagnostic-test-token-32-bytes";

    fn headers() -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_str(&format!("Bearer {TOKEN}")).unwrap(),
        );
        headers
    }

    #[test]
    fn configuration_is_explicit_and_requires_a_strong_token() {
        assert!(MediaDiagnostics::configure(false, None).unwrap().is_none());
        assert!(MediaDiagnostics::configure(true, None).is_err());
        assert!(MediaDiagnostics::configure(true, Some("short")).is_err());
        assert!(
            MediaDiagnostics::configure(true, Some(TOKEN))
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn endpoint_gates_are_uncached_and_do_not_start_collection() {
        let diagnostics = MediaDiagnostics::configure(true, Some(TOKEN))
            .unwrap()
            .unwrap();
        let manager = TransportManager::new();
        let drain = DrainSignal::default();
        for (enabled, token, request, expected) in [
            (None, Some(TOKEN), headers(), StatusCode::NOT_FOUND),
            (
                Some(diagnostics.as_ref()),
                None,
                headers(),
                StatusCode::NOT_FOUND,
            ),
            (
                Some(diagnostics.as_ref()),
                Some(TOKEN),
                HeaderMap::new(),
                StatusCode::UNAUTHORIZED,
            ),
            (
                Some(diagnostics.as_ref()),
                Some("incorrect"),
                headers(),
                StatusCode::UNAUTHORIZED,
            ),
        ] {
            let response = capture_response(enabled, token, &request, &drain, &manager).await;
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        }
        let mut duplicate = headers();
        duplicate.append(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer extra"),
        );
        assert_eq!(
            capture_response(
                Some(&diagnostics),
                Some(TOKEN),
                &duplicate,
                &drain,
                &manager
            )
            .await
            .status(),
            StatusCode::UNAUTHORIZED,
        );
        drain.begin_draining();
        assert_eq!(
            capture_response(
                Some(&diagnostics),
                Some(TOKEN),
                &headers(),
                &drain,
                &manager
            )
            .await
            .status(),
            StatusCode::SERVICE_UNAVAILABLE,
        );
        assert!(diagnostics.last_started.lock().unwrap().is_none());
        assert_eq!(diagnostics.active.available_permits(), 1);
    }

    #[tokio::test]
    async fn authenticated_empty_snapshot_is_not_cached_or_confused_with_missing_data() {
        let diagnostics = MediaDiagnostics::configure(true, Some(TOKEN))
            .unwrap()
            .unwrap();
        let manager = TransportManager::new();
        let response = capture_response(
            Some(&diagnostics),
            Some(TOKEN),
            &headers(),
            &DrainSignal::default(),
            &manager,
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let snapshot: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(snapshot["schemaVersion"], 1);
        assert_eq!(snapshot["coverage"]["complete"], true);
        assert_eq!(snapshot["entities"], serde_json::json!([]));
        assert!(!String::from_utf8(body.to_vec()).unwrap().contains(TOKEN));
        assert_eq!(diagnostics.active.available_permits(), 1);
    }

    #[test]
    fn request_capacity_and_spacing_are_bounded_without_waiting() {
        let diagnostics = MediaDiagnostics::configure(true, Some(TOKEN))
            .unwrap()
            .unwrap();
        let start = Instant::now();
        let permit = diagnostics.try_acquire_at(start).unwrap();
        assert_eq!(
            diagnostics
                .try_acquire_at(start + Duration::from_secs(1))
                .unwrap_err(),
            StatusCode::TOO_MANY_REQUESTS
        );
        drop(permit);
        assert_eq!(
            diagnostics
                .try_acquire_at(start + MIN_START_INTERVAL / 2)
                .unwrap_err(),
            StatusCode::TOO_MANY_REQUESTS
        );
        assert!(
            diagnostics
                .try_acquire_at(start + MIN_START_INTERVAL)
                .is_ok()
        );
        assert_eq!(diagnostics.active.available_permits(), 1);
    }

    #[tokio::test]
    async fn cancelling_a_request_releases_its_capacity() {
        let diagnostics = MediaDiagnostics::configure(true, Some(TOKEN))
            .unwrap()
            .unwrap();
        let owned = diagnostics.clone();
        let (entered, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _permit = owned.try_acquire_at(Instant::now()).unwrap();
            let _ = entered.send(());
            std::future::pending::<()>().await;
        });
        tokio::time::timeout(Duration::from_secs(1), ready)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(diagnostics.active.available_permits(), 0);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(diagnostics.active.available_permits(), 1);
    }
}
