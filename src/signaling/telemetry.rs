//! Bounded, aggregate telemetry. Every label comes from a closed vocabulary;
//! request paths, credentials, identities and arbitrary browser text never enter
//! this store. Client reports are untrusted observations, never admission input.
use super::SignalingServer;
use axum::{Json, extract::State, http::StatusCode};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU64, Ordering};
use std::{collections::BTreeMap, fmt::Write, sync::Mutex, time::Duration};

const MAX_CLIENT_SERIES: usize = 512;
const MAX_HTTP_SERIES: usize = 256;
const DURATION_BOUNDS_MS: [u64; 10] = [1, 5, 10, 50, 100, 500, 1_000, 5_000, 30_000, 120_000];

macro_rules! vocabulary {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
        #[serde(rename_all = "snake_case")]
        pub(crate) enum $name { $($variant),+ }
        impl $name {
            fn label(self) -> String {
                serde_json::to_value(self).expect("fixed enum serializes").as_str().expect("enum string").to_owned()
            }
        }
    };
}
vocabulary!(Browser {
    Firefox,
    Chromium,
    Safari,
    Other
});
vocabulary!(ClientName {
    AuthRestore,
    PasswordLogin,
    PasswordRegister,
    PasskeyLoginStart,
    PasskeyLoginCeremony,
    PasskeyLoginFinish,
    PasskeyRegisterStart,
    PasskeyRegisterCeremony,
    PasskeyRegisterFinish,
    RoomJoin,
    RoomAdmission,
    ChatSend,
    Connection,
    Reconnect,
    JsError,
    UnhandledRejection,
    MediaSample,
    MediaFirstVideoFrame,
    MediaVideoProgress,
    MediaVideoFreeze,
    MediaAudioConcealment,
    MediaPacketLoss,
    MediaRtt,
});
vocabulary!(ClientOutcome {
    Started,
    Waiting,
    Ok,
    Error,
    Timeout,
    CancelledOrTimeout,
    Unavailable,
    Superseded,
    Denied,
    Unauthenticated,
    NormalClose,
    GoingAway,
    AbnormalClose,
    PolicyClose,
    ServerClose,
    OtherClose,
    CleanClose,
    UncleanClose,
    Unknown,
});

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub(crate) struct ClientEvent {
    name: ClientName,
    outcome: ClientOutcome,
    duration_ms: Option<u64>,
    value: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ClientBatch {
    version: u8,
    browser: Browser,
    events: Vec<ClientEvent>,
}

impl ClientBatch {
    fn valid(&self) -> bool {
        self.version == 1
            && !self.events.is_empty()
            && self.events.len() <= 16
            && self.events.iter().all(|event| {
                event.duration_ms.is_none_or(|duration| duration <= 120_000)
                    && event.value.is_none_or(|value| match event.name {
                        ClientName::MediaAudioConcealment | ClientName::MediaPacketLoss => {
                            value <= 10_000
                        }
                        ClientName::MediaVideoProgress
                        | ClientName::MediaVideoFreeze
                        | ClientName::MediaRtt => value <= 1_000_000,
                        _ => false,
                    })
            })
    }
}

pub(super) async fn ingest(
    State(server): State<SignalingServer>,
    Json(batch): Json<ClientBatch>,
) -> StatusCode {
    if !batch.valid() {
        return StatusCode::BAD_REQUEST;
    }
    let Some(_permit) = server.telemetry_guard.try_acquire_concurrency() else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    server.metrics.telemetry().record_client(batch);
    StatusCode::NO_CONTENT
}

#[derive(Default)]
struct Observation {
    count: u64,
    durations: [u64; 11],
    duration_sum_ms: u64,
    duration_count: u64,
    value_count: u64,
    value_sum: u64,
}

impl Observation {
    fn observe(&mut self, duration: Option<Duration>, value: Option<u64>) {
        self.count = self.count.saturating_add(1);
        if let Some(duration) = duration {
            self.duration_count = self.duration_count.saturating_add(1);
            let millis = u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
            let bucket = DURATION_BOUNDS_MS.partition_point(|bound| *bound < millis);
            self.durations[bucket] = self.durations[bucket].saturating_add(1);
            self.duration_sum_ms = self.duration_sum_ms.saturating_add(millis);
        }
        if let Some(value) = value {
            self.value_count = self.value_count.saturating_add(1);
            self.value_sum = self.value_sum.saturating_add(value);
        }
    }

    fn render(&self, prefix: &str, labels: &str, out: &mut String) {
        let _ = writeln!(out, "{prefix}_total{{{labels}}} {}", self.count);
        if self.duration_count > 0 {
            let mut count = 0;
            for (index, bucket) in self.durations.iter().enumerate() {
                count += bucket;
                let bound = DURATION_BOUNDS_MS.get(index).map_or_else(
                    || "+Inf".to_owned(),
                    |ms| (*ms as f64 / 1_000.0).to_string(),
                );
                let _ = writeln!(
                    out,
                    "{prefix}_duration_seconds_bucket{{{labels},le=\"{bound}\"}} {count}"
                );
            }
            let _ = writeln!(
                out,
                "{prefix}_duration_seconds_sum{{{labels}}} {}",
                self.duration_sum_ms as f64 / 1_000.0
            );
            let _ = writeln!(out, "{prefix}_duration_seconds_count{{{labels}}} {count}");
        }
        if self.value_count > 0 {
            let _ = writeln!(
                out,
                "{prefix}_value_sum_total{{{labels}}} {}",
                self.value_sum
            );
            let _ = writeln!(
                out,
                "{prefix}_value_observations_total{{{labels}}} {}",
                self.value_count
            );
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum WarningFamily {
    Handshake,
    FrameRate,
    ChatRate,
}

impl WarningFamily {
    fn index(self) -> usize {
        match self {
            Self::Handshake => 0,
            Self::FrameRate => 1,
            Self::ChatRate => 2,
        }
    }
}

#[derive(Default)]
struct WarningWindow {
    started: Option<std::time::Instant>,
    events: u64,
    suppressed: u64,
}

impl WarningWindow {
    fn observe(&mut self, now: std::time::Instant) -> bool {
        self.events = self.events.saturating_add(1);
        if self
            .started
            .is_none_or(|started| now.duration_since(started) >= Duration::from_secs(60))
        {
            self.started = Some(now);
            true
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            false
        }
    }
}

/// Stores only finite route/enum combinations. Lock-held work is bounded by the
/// static vocabularies; ingestion adds at most sixteen observations per request.
#[derive(Default)]
pub(crate) struct Telemetry {
    http: Mutex<BTreeMap<(&'static str, &'static str, &'static str), Observation>>,
    signaling: Mutex<BTreeMap<(&'static str, &'static str), Observation>>,
    client: Mutex<BTreeMap<(ClientName, ClientOutcome, Browser), Observation>>,
    client_series_dropped: AtomicU64,
    http_series_dropped: AtomicU64,
    warnings: Mutex<[WarningWindow; 3]>,
}

impl Telemetry {
    /// Only named repetitive admission warnings are coalesced. Errors and
    /// native media warnings retain their existing logging behavior.
    pub(crate) fn warning_should_log(&self, family: WarningFamily) -> bool {
        self.warnings
            .lock()
            .unwrap_or_else(|error| error.into_inner())[family.index()]
        .observe(std::time::Instant::now())
    }

    pub(crate) fn record_http(
        &self,
        route: Option<&str>,
        method: &str,
        status: u16,
        duration: Duration,
    ) {
        let route = fixed_route(route);
        let method = match method {
            "GET" => "GET",
            "POST" => "POST",
            "PATCH" => "PATCH",
            "DELETE" => "DELETE",
            "HEAD" => "HEAD",
            "OPTIONS" => "OPTIONS",
            _ => "other",
        };
        let status = match status {
            408 => "408",
            429 => "429",
            503 => "503",
            100..=199 => "1xx",
            200..=299 => "2xx",
            300..=399 => "3xx",
            400..=499 => "4xx",
            _ => "5xx",
        };
        let mut records = self.http.lock().unwrap_or_else(|error| error.into_inner());
        let key = (route, method, status);
        if records.len() >= MAX_HTTP_SERIES && !records.contains_key(&key) {
            self.http_series_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        records
            .entry(key)
            .or_default()
            .observe(Some(duration), None);
    }

    pub(crate) fn record_signaling(
        &self,
        operation: crate::diagnostics::OperationKind,
        accepted: bool,
        duration: Duration,
    ) {
        let operation = operation_label(operation);
        self.signaling
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .entry((operation, if accepted { "completed" } else { "rejected" }))
            .or_default()
            .observe(Some(duration), None);
    }

    fn record_client(&self, batch: ClientBatch) {
        let mut records = self
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for event in batch.events {
            if records.len() >= MAX_CLIENT_SERIES
                && !records.contains_key(&(event.name, event.outcome, batch.browser))
            {
                self.client_series_dropped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
            records
                .entry((event.name, event.outcome, batch.browser))
                .or_default()
                .observe(event.duration_ms.map(Duration::from_millis), event.value);
        }
    }

    pub(crate) fn render(&self, out: &mut String) {
        let _ = writeln!(
            out,
            "# HELP simplestchat_client_series_dropped_total Browser observations discarded because the finite series capacity was reached\n# TYPE simplestchat_client_series_dropped_total counter\nsimplestchat_client_series_dropped_total {}",
            self.client_series_dropped.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP simplestchat_http_series_dropped_total HTTP observations discarded because the finite route-series capacity was reached\n# TYPE simplestchat_http_series_dropped_total counter\nsimplestchat_http_series_dropped_total {}",
            self.http_series_dropped.load(Ordering::Relaxed)
        );
        let _ = writeln!(
            out,
            "# HELP simplestchat_warning_events_total Occurrences of explicitly coalesced admission-warning families\n# TYPE simplestchat_warning_events_total counter\n# HELP simplestchat_warning_logs_suppressed_total Repetitive admission warnings omitted after the first exemplar per family per minute\n# TYPE simplestchat_warning_logs_suppressed_total counter"
        );
        let warnings = self
            .warnings
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        for (family, window) in [
            "handshake_rate_limit",
            "frame_rate_limit",
            "chat_rate_limit",
        ]
        .iter()
        .zip(warnings.iter())
        {
            let _ = writeln!(
                out,
                "simplestchat_warning_events_total{{family=\"{family}\"}} {}\nsimplestchat_warning_logs_suppressed_total{{family=\"{family}\"}} {}",
                window.events, window.suppressed
            );
        }
        drop(warnings);
        for (prefix, help) in [
            (
                "simplestchat_http_requests",
                "HTTP response headers produced, including deadline and guard rejections; duration excludes body delivery",
            ),
            (
                "simplestchat_signaling_operations",
                "Dispatched commands; completed indicates handler returned Ok and may include a domain rejection response",
            ),
            (
                "simplestchat_client_events",
                "Untrusted opt-in browser-reported events; missing reports do not mean success",
            ),
        ] {
            let _ = writeln!(
                out,
                "# HELP {prefix}_total {help}\n# TYPE {prefix}_total counter\n# HELP {prefix}_duration_seconds Recorded operation durations; client durations are optional and untrusted\n# TYPE {prefix}_duration_seconds histogram"
            );
        }
        let _ = writeln!(
            out,
            "# HELP simplestchat_client_events_value_sum_total Sum of untrusted browser measurements; units are fixed by event name\n# TYPE simplestchat_client_events_value_sum_total counter\n# HELP simplestchat_client_events_value_observations_total Browser measurements received with a value\n# TYPE simplestchat_client_events_value_observations_total counter"
        );
        for ((route, method, status), observation) in self
            .http
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
        {
            observation.render(
                "simplestchat_http_requests",
                &format!("route=\"{route}\",method=\"{method}\",status=\"{status}\""),
                out,
            );
        }
        for ((operation, outcome), observation) in self
            .signaling
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
        {
            observation.render(
                "simplestchat_signaling_operations",
                &format!("operation=\"{operation}\",outcome=\"{outcome}\""),
                out,
            );
        }
        for ((name, outcome, browser), observation) in self
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
        {
            observation.render(
                "simplestchat_client_events",
                &format!(
                    "name=\"{}\",outcome=\"{}\",browser=\"{}\"",
                    name.label(),
                    outcome.label(),
                    browser.label()
                ),
                out,
            );
        }
    }
}

fn operation_label(operation: crate::diagnostics::OperationKind) -> &'static str {
    use crate::diagnostics::OperationKind;
    match operation {
        OperationKind::JoinRoom => "join_room",
        OperationKind::LeaveRoom => "leave_room",
        OperationKind::Reconnect => "reconnect",
        OperationKind::RouterCapabilities => "router_capabilities",
        OperationKind::CreateSendTransport => "create_send_transport",
        OperationKind::CreateRecvTransport => "create_recv_transport",
        OperationKind::ConnectTransport => "connect_transport",
        OperationKind::Produce => "produce",
        OperationKind::Consume => "consume",
        OperationKind::ResumeConsumer => "resume_consumer",
        OperationKind::PauseConsumer => "pause_consumer",
        OperationKind::CloseConsumer => "close_consumer",
        OperationKind::PauseProducer => "pause_producer",
        OperationKind::ResumeProducer => "resume_producer",
        OperationKind::CloseProducer => "close_producer",
        OperationKind::RestartIce => "restart_ice",
        OperationKind::SetPreferredLayers => "set_preferred_layers",
        OperationKind::Chat => "chat",
        OperationKind::PrivateMessage => "private_message",
        OperationKind::RoomAction => "room_action",
        OperationKind::SocketWrite => "socket_write",
        OperationKind::ShutdownNotification => "shutdown_notification",
    }
}

const HTTP_ROUTES: &[&str] = &[
    "/ws",
    "/health",
    "/ready",
    "/metrics",
    "/diagnostics/media",
    "/api/telemetry",
    "/api/auth/register",
    "/api/auth/login",
    "/api/auth/refresh",
    "/api/auth/logout",
    "/api/auth/profile",
    "/api/auth/profiles/{id}",
    "/api/auth/password",
    "/api/auth/recovery/key",
    "/api/auth/recovery/redeem",
    "/api/auth/passkey/register/start",
    "/api/auth/passkey/register/finish",
    "/api/auth/passkey/login/start",
    "/api/auth/passkey/login/finish",
    "/api/rooms",
    "/api/rooms/",
    "/api/rooms/mine",
    "/api/rooms/{id}/identity",
    "/api/rooms/{id}",
];

pub(super) fn fixed_route(route: Option<&str>) -> &'static str {
    HTTP_ROUTES
        .iter()
        .copied()
        .find(|known| Some(*known) == route)
        .unwrap_or("unmatched")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_metric_vocabulary_stays_within_the_deployed_scrape_budget() {
        // The operations contract test checks the deployed limit against this
        // budget without making Rust compilation depend on deployment files.
        const SCRAPE_SAMPLE_LIMIT: usize = 15_000;
        use crate::diagnostics::OperationKind;
        use crate::media::config::MAX_MEDIA_WORKERS;
        use crate::metrics::{SaturationSnapshot, ServerMetrics, WorkerCpuSnapshot};
        let metrics = ServerMetrics::new();
        let telemetry = metrics.telemetry();
        for route in HTTP_ROUTES
            .iter()
            .copied()
            .chain(["/unmatched-private-path"])
        {
            for method in [
                "GET",
                "POST",
                "PATCH",
                "DELETE",
                "HEAD",
                "OPTIONS",
                "nonstandard",
            ] {
                for status in [101, 200, 302, 400, 408, 429, 500, 503] {
                    telemetry.record_http(Some(route), method, status, Duration::from_millis(1));
                }
            }
        }
        assert_eq!(telemetry.http.lock().unwrap().len(), MAX_HTTP_SERIES);
        assert!(telemetry.http_series_dropped.load(Ordering::Relaxed) > 0);
        let dropped = telemetry.http_series_dropped.load(Ordering::Relaxed);
        telemetry.record_http(Some("/ws"), "GET", 101, Duration::from_millis(1));
        assert_eq!(
            telemetry.http.lock().unwrap()[&("/ws", "GET", "1xx")].count,
            2
        );
        assert_eq!(
            telemetry.http_series_dropped.load(Ordering::Relaxed),
            dropped,
            "existing series continue updating at capacity"
        );
        for browser in ["firefox", "chromium", "safari", "other"] {
            for name in [
                "media_audio_concealment",
                "media_packet_loss",
                "media_video_progress",
                "media_video_freeze",
                "media_rtt",
                "auth_restore",
                "room_join",
            ] {
                for outcome in [
                    "started",
                    "waiting",
                    "ok",
                    "error",
                    "timeout",
                    "cancelled_or_timeout",
                    "unavailable",
                    "superseded",
                    "denied",
                    "unauthenticated",
                    "normal_close",
                    "going_away",
                    "abnormal_close",
                    "policy_close",
                    "server_close",
                    "other_close",
                    "clean_close",
                    "unclean_close",
                    "unknown",
                ] {
                    let value = name.starts_with("media_").then_some(1);
                    let batch: ClientBatch = serde_json::from_value(serde_json::json!({"version":1,"browser":browser,"events":[{"name":name,"outcome":outcome,"durationMs":1,"value":value}]})).unwrap();
                    assert!(batch.valid());
                    telemetry.record_client(batch);
                }
            }
        }
        assert_eq!(telemetry.client.lock().unwrap().len(), MAX_CLIENT_SERIES);
        for operation in [
            OperationKind::JoinRoom,
            OperationKind::LeaveRoom,
            OperationKind::Reconnect,
            OperationKind::RouterCapabilities,
            OperationKind::CreateSendTransport,
            OperationKind::CreateRecvTransport,
            OperationKind::ConnectTransport,
            OperationKind::Produce,
            OperationKind::Consume,
            OperationKind::ResumeConsumer,
            OperationKind::PauseConsumer,
            OperationKind::CloseConsumer,
            OperationKind::PauseProducer,
            OperationKind::ResumeProducer,
            OperationKind::CloseProducer,
            OperationKind::RestartIce,
            OperationKind::SetPreferredLayers,
            OperationKind::Chat,
            OperationKind::PrivateMessage,
            OperationKind::RoomAction,
            OperationKind::SocketWrite,
            OperationKind::ShutdownNotification,
        ] {
            for accepted in [true, false] {
                telemetry.record_signaling(operation, accepted, Duration::from_millis(1));
            }
        }
        metrics.set_saturation(SaturationSnapshot {
            throttling_available: true,
            pressure_available: true,
            saturated: false,
            throttled_fraction: 0.0,
            pressure_avg10: 0.0,
        });
        metrics.set_worker_cpu(
            (0..MAX_MEDIA_WORKERS)
                .map(|index| WorkerCpuSnapshot {
                    index,
                    utilization: Some(0.0),
                    saturated: false,
                })
                .collect(),
        );
        metrics.set_quality_sample(crate::media::quality::QualitySample {
            completed_at: Some(std::time::Instant::now()),
            completed_wall_time: Some(std::time::SystemTime::now()),
            ..Default::default()
        });
        let snapshot = metrics.render_prometheus(0, 0, MAX_MEDIA_WORKERS);
        let samples = snapshot
            .lines()
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .count();
        // Even the conservative product (all client series carry a value) is
        // bounded: 8192 client + 3584 HTTP + 616 signaling samples. Reserve 2000
        // further samples for base gauges and handler-appended pool/build data.
        assert!(
            samples <= MAX_CLIENT_SERIES * 16 + MAX_HTTP_SERIES * 14 + 44 * 14 + 2000,
            "{samples}"
        );
        const {
            assert!(
                MAX_CLIENT_SERIES * 16 + MAX_HTTP_SERIES * 14 + 44 * 14 + 2000
                    < SCRAPE_SAMPLE_LIMIT
            );
        }
        assert!(
            samples < SCRAPE_SAMPLE_LIMIT,
            "sample budget exceeded: {samples}"
        );
        println!(
            "Full valid vocabulary and 64-worker snapshot: {samples} samples; conservative family bound plus 2000-base allowance: 14392 < 15000"
        );
        assert!(!snapshot.contains("unmatched-private-path"));
    }

    #[test]
    fn coalescing_retains_an_exemplar_and_counts_every_omission() {
        let mut window = WarningWindow::default();
        let now = std::time::Instant::now();
        assert!(window.observe(now));
        for _ in 0..99 {
            assert!(!window.observe(now));
        }
        assert_eq!(window.events, 100);
        assert_eq!(window.suppressed, 99);
        assert!(window.observe(now + Duration::from_secs(60)));
        assert_eq!(window.events, 101);
        assert_eq!(window.suppressed, 99);
    }

    #[test]
    fn client_cross_product_has_a_hard_series_cap_and_no_empty_histograms() {
        let telemetry = Telemetry::default();
        for browser in ["firefox", "chromium", "safari", "other"] {
            for name in [
                "auth_restore",
                "password_login",
                "password_register",
                "room_join",
                "chat_send",
                "connection",
                "reconnect",
                "js_error",
                "unhandled_rejection",
                "media_sample",
            ] {
                for outcome in [
                    "started",
                    "ok",
                    "error",
                    "timeout",
                    "cancelled_or_timeout",
                    "unavailable",
                    "superseded",
                    "denied",
                    "unauthenticated",
                    "normal_close",
                    "going_away",
                    "abnormal_close",
                    "policy_close",
                    "server_close",
                    "other_close",
                    "clean_close",
                    "unclean_close",
                    "unknown",
                ] {
                    let batch = serde_json::from_value(serde_json::json!({"version":1,"browser":browser,"events":[{"name":name,"outcome":outcome}]})).unwrap();
                    telemetry.record_client(batch);
                }
            }
        }
        assert_eq!(telemetry.client.lock().unwrap().len(), MAX_CLIENT_SERIES);
        assert_eq!(
            telemetry.client_series_dropped.load(Ordering::Relaxed),
            720 - MAX_CLIENT_SERIES as u64
        );
        let mut snapshot = String::new();
        telemetry.render(&mut snapshot);
        assert!(!snapshot.contains("simplestchat_client_events_duration_seconds_bucket{"));
    }

    #[test]
    fn browser_contract_rejects_unknown_fields_values_and_oversized_batches() {
        for json in [
            r#"{"version":1,"browser":"firefox","events":[{"name":"room_join","outcome":"ok","userId":"private"}]}"#,
            r#"{"version":1,"browser":"other","events":[{"name":"arbitrary","outcome":"ok"}]}"#,
            r#"{"version":1,"browser":"other","events":[{"name":"room_join","outcome":"ok","durationMs":-1}]}"#,
        ] {
            assert!(serde_json::from_str::<ClientBatch>(json).is_err());
        }
        for (name, value, valid) in [
            ("room_join", 1, false),
            ("media_packet_loss", 10_001, false),
            ("media_packet_loss", 10_000, true),
            ("media_rtt", 1_000_001, false),
        ] {
            let batch: ClientBatch = serde_json::from_value(serde_json::json!({"version":1,"browser":"firefox","events":[{"name":name,"outcome":"ok","value":value}]})).unwrap();
            assert_eq!(batch.valid(), valid);
        }
        let batch: ClientBatch = serde_json::from_value(serde_json::json!({"version":1,"browser":"other","events":vec![serde_json::json!({"name":"room_join","outcome":"ok"});17]})).unwrap();
        assert!(!batch.valid());
    }

    #[test]
    fn request_labels_never_include_paths_or_unknown_methods() {
        let telemetry = Telemetry::default();
        telemetry.record_http(
            Some("/private-user?secret=abc"),
            "arbitrary",
            429,
            Duration::from_millis(20),
        );
        telemetry.record_http(
            Some("/api/auth/login"),
            "POST",
            408,
            Duration::from_secs(15),
        );
        let mut text = String::new();
        telemetry.render(&mut text);
        assert!(!text.contains("secret"));
        assert!(!text.contains("arbitrary"));
        assert!(text.contains("route=\"unmatched\",method=\"other\",status=\"429\""));
        assert!(text.contains("route=\"/api/auth/login\",method=\"POST\",status=\"408\""));
    }
}
