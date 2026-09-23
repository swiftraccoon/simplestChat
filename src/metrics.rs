#![forbid(unsafe_code)]

// Server metrics — lock-free AtomicU64 counters and Prometheus-compatible histogram.

use crate::diagnostics::Diagnostics;
use std::fmt::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::time::Duration;

/// Fixed histogram bucket boundaries (in microseconds for internal storage).
const BUCKET_BOUNDS_US: [u64; 10] = [
    1_000,     // 1ms
    5_000,     // 5ms
    10_000,    // 10ms
    25_000,    // 25ms
    50_000,    // 50ms
    100_000,   // 100ms
    250_000,   // 250ms
    500_000,   // 500ms
    1_000_000, // 1s
    5_000_000, // 5s
];

/// Prometheus-compatible histogram with disjoint storage and cumulative output.
/// Each observation updates one bucket. A scrape computes all cumulative counts
/// from one sampled vector, so concurrent observations cannot produce decreasing
/// bucket boundaries or a different `+Inf` and `_count`. Sum/bucket sampling is
/// weakly consistent, not a transactionally exact instant across all atomics.
pub struct Histogram {
    /// One interval per finite bound plus an overflow interval above five seconds.
    buckets: [AtomicU64; 11],
    /// Sum of all observations in microseconds
    sum_us: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_us: AtomicU64::new(0),
        }
    }

    /// Record a duration observation.
    pub fn observe(&self, duration: Duration) {
        let us = duration.as_micros() as u64;
        let bucket = BUCKET_BOUNDS_US.partition_point(|bound| *bound < us);
        self.buckets[bucket].fetch_add(1, Relaxed);
        self.sum_us.fetch_add(us, Relaxed);
    }

    /// Prefix summation preserves monotonicity even when individual interval
    /// samples straddle an observation. Keeping reads injectable also permits
    /// deterministic interleaving tests without timing-dependent stress tests.
    fn cumulative_buckets(mut read: impl FnMut(usize) -> u64) -> [u64; 11] {
        let mut count = 0;
        std::array::from_fn(|index| {
            count += read(index);
            count
        })
    }

    /// Render in Prometheus text exposition format.
    fn render(&self, name: &str, help: &str, out: &mut String) {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} histogram");

        let labels = [
            "0.001", "0.005", "0.01", "0.025", "0.05", "0.1", "0.25", "0.5", "1", "5",
        ];
        let buckets = Self::cumulative_buckets(|index| self.buckets[index].load(Relaxed));
        for (i, label) in labels.iter().enumerate() {
            let val = buckets[i];
            let _ = writeln!(out, "{name}_bucket{{le=\"{label}\"}} {val}");
        }
        let count = buckets[BUCKET_BOUNDS_US.len()];
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {count}");
        let sum_us = self.sum_us.load(Relaxed);
        // Convert microseconds to seconds with 6 decimal places
        let _ = writeln!(
            out,
            "{name}_sum {}.{:06}",
            sum_us / 1_000_000,
            sum_us % 1_000_000
        );
        let _ = writeln!(out, "{name}_count {count}");
    }
}

/// Server-wide metrics using lock-free atomics.
#[derive(Clone)]
pub struct ServerMetrics {
    inner: Arc<Inner>,
    diagnostics: Diagnostics,
}

struct Inner {
    telemetry: crate::signaling::telemetry::Telemetry,
    // Monotonic counters
    connections_total: AtomicU64,
    messages_received_total: AtomicU64,
    messages_sent_total: AtomicU64,
    message_send_failed_total: AtomicU64,
    outbound_queue_full_total: AtomicU64,
    outbound_queue_closed_total: AtomicU64,
    errors_total: AtomicU64,
    rooms_created_total: AtomicU64,
    joins_total: AtomicU64,
    lobby_admissions_total: AtomicU64,
    reconnects_total: AtomicU64,
    leaves_total: AtomicU64,
    producers_created_total: AtomicU64,
    consumers_created_total: AtomicU64,

    api_requests_rejected_total: AtomicU64,
    upgrades_rejected_total: AtomicU64,
    media_worker_deaths_total: AtomicU64,
    media_worker_recoveries: [AtomicU64; 2],
    next_connection_id: AtomicU64,
    password_work_rejected: AtomicU64,
    joins_refused_saturated_total: AtomicU64,
    consumer_layer_requests_total: AtomicU64,
    /// Latest CPU saturation reading, published by the monitor task.
    saturation: std::sync::RwLock<Option<SaturationSnapshot>>,
    worker_cpu: std::sync::RwLock<Vec<WorkerCpuSnapshot>>,
    /// Latest media quality sample, published by the sampler task.
    quality: std::sync::RwLock<Option<crate::media::quality::QualitySample>>,

    // Gauge
    connections_active: AtomicU64,

    // Histogram
    message_handling: Histogram,
}

impl Default for ServerMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerMetrics {
    /// Independent metrics with local operation diagnostics disabled by default.
    pub fn new() -> Self {
        Self::with_diagnostics(Diagnostics::default())
    }

    /// Attach the process-owned, bounded diagnostic recorder to these metrics.
    pub fn with_diagnostics(diagnostics: Diagnostics) -> Self {
        Self {
            inner: Arc::new(Inner {
                telemetry: crate::signaling::telemetry::Telemetry::default(),
                connections_total: AtomicU64::new(0),
                messages_received_total: AtomicU64::new(0),
                messages_sent_total: AtomicU64::new(0),
                message_send_failed_total: AtomicU64::new(0),
                outbound_queue_full_total: AtomicU64::new(0),
                outbound_queue_closed_total: AtomicU64::new(0),
                errors_total: AtomicU64::new(0),
                rooms_created_total: AtomicU64::new(0),
                joins_total: AtomicU64::new(0),
                lobby_admissions_total: AtomicU64::new(0),
                reconnects_total: AtomicU64::new(0),
                leaves_total: AtomicU64::new(0),
                producers_created_total: AtomicU64::new(0),
                consumers_created_total: AtomicU64::new(0),
                api_requests_rejected_total: AtomicU64::new(0),
                upgrades_rejected_total: AtomicU64::new(0),
                media_worker_deaths_total: AtomicU64::new(0),
                media_worker_recoveries: std::array::from_fn(|_| AtomicU64::new(0)),
                next_connection_id: AtomicU64::new(1),
                password_work_rejected: AtomicU64::new(0),
                joins_refused_saturated_total: AtomicU64::new(0),
                consumer_layer_requests_total: AtomicU64::new(0),
                saturation: std::sync::RwLock::new(None),
                worker_cpu: std::sync::RwLock::new(Vec::new()),
                quality: std::sync::RwLock::new(None),
                connections_active: AtomicU64::new(0),
                message_handling: Histogram::new(),
            }),
            diagnostics,
        }
    }

    /// The shared recorder attached at construction; disabled for [`Self::new`].
    pub fn diagnostics(&self) -> &Diagnostics {
        &self.diagnostics
    }

    pub(crate) fn telemetry(&self) -> &crate::signaling::telemetry::Telemetry {
        &self.inner.telemetry
    }

    // --- Counter increments ---

    pub fn inc_connections_total(&self) {
        self.inner.connections_total.fetch_add(1, Relaxed);
    }

    /// Counts an observed inbound frame, including control/rejected frames.
    pub fn inc_messages_received(&self) {
        self.inner.messages_received_total.fetch_add(1, Relaxed);
    }

    /// Counts a completed application text-frame socket write (including shutdown
    /// notices), not enqueue attempts
    /// or an acknowledgement that the peer processed the message.
    pub fn inc_messages_sent(&self) {
        self.inner.messages_sent_total.fetch_add(1, Relaxed);
    }

    /// Counts an attempted application text-frame write returning an error or
    /// timing out. Deliberate drain cancellation is not a failed send.
    pub fn inc_message_send_failed(&self) {
        self.inner.message_send_failed_total.fetch_add(1, Relaxed);
    }

    /// Essential-message enqueue attempt rejected by a full recipient queue.
    /// Do not count intentionally coalesced/dropped ephemeral media hints here.
    pub fn inc_outbound_queue_full(&self) {
        self.inner.outbound_queue_full_total.fetch_add(1, Relaxed);
    }

    /// Essential-message enqueue attempt rejected by a closed recipient queue.
    pub fn inc_outbound_queue_closed(&self) {
        self.inner.outbound_queue_closed_total.fetch_add(1, Relaxed);
    }

    /// An HTTP API request answered 429 or 503: rate limits, concurrency
    /// caps, the password lane, or a busy service. Without this series a
    /// login flood looks healthy on `/metrics`.
    pub fn inc_api_request_rejected(&self) {
        self.inner.api_requests_rejected_total.fetch_add(1, Relaxed);
    }

    /// A WebSocket upgrade refused by the handshake, connection, or per-IP
    /// limits before `connections_active` could observe it.
    pub fn inc_upgrade_rejected(&self) {
        self.inner.upgrades_rejected_total.fetch_add(1, Relaxed);
    }

    /// A media worker died. Replacement outcomes are counted separately.
    pub fn inc_media_worker_death(&self) {
        self.inner.media_worker_deaths_total.fetch_add(1, Relaxed);
    }

    pub(crate) fn inc_media_worker_recovery(&self, succeeded: bool) {
        self.inner.media_worker_recoveries[usize::from(succeeded)].fetch_add(1, Relaxed);
    }

    pub(crate) fn next_connection_id(&self) -> u64 {
        self.inner.next_connection_id.fetch_add(1, Relaxed)
    }

    pub(crate) fn inc_password_work_rejected(&self) {
        self.inner.password_work_rejected.fetch_add(1, Relaxed);
    }

    pub fn inc_errors(&self) {
        self.inner.errors_total.fetch_add(1, Relaxed);
    }

    /// A worker request changed a consumer's preferred layers, after the
    /// viewer ceiling and the bandwidth tier were merged server-side.
    pub fn inc_consumer_layer_request(&self) {
        self.inner
            .consumer_layer_requests_total
            .fetch_add(1, Relaxed);
    }

    /// A fresh join was refused because the process is CPU saturated.
    pub fn inc_join_refused_saturated(&self) {
        self.inner
            .joins_refused_saturated_total
            .fetch_add(1, Relaxed);
    }

    /// Publishes the latest per-worker CPU readings of the saturation monitor.
    pub fn set_worker_cpu(&self, workers: Vec<WorkerCpuSnapshot>) {
        *self
            .inner
            .worker_cpu
            .write()
            .unwrap_or_else(|e| e.into_inner()) = workers;
    }

    pub fn set_saturation(&self, snapshot: SaturationSnapshot) {
        *self
            .inner
            .saturation
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
    }

    pub fn set_quality_sample(&self, sample: crate::media::quality::QualitySample) {
        *self
            .inner
            .quality
            .write()
            .unwrap_or_else(|e| e.into_inner()) = Some(sample);
    }

    pub fn inc_rooms_created(&self) {
        self.inner.rooms_created_total.fetch_add(1, Relaxed);
    }

    pub fn inc_joins(&self) {
        self.inner.joins_total.fetch_add(1, Relaxed);
    }

    pub(crate) fn inc_lobby_admission(&self) {
        self.inner.lobby_admissions_total.fetch_add(1, Relaxed);
    }

    pub(crate) fn inc_reconnect(&self) {
        self.inner.reconnects_total.fetch_add(1, Relaxed);
    }

    pub fn inc_leaves(&self) {
        self.inner.leaves_total.fetch_add(1, Relaxed);
    }

    pub fn inc_producers_created(&self) {
        self.inner.producers_created_total.fetch_add(1, Relaxed);
    }

    pub fn inc_consumers_created(&self) {
        self.inner.consumers_created_total.fetch_add(1, Relaxed);
    }

    // --- Gauge ---

    /// Increments connections_active and returns an RAII guard that decrements on drop.
    /// This guarantees the gauge is decremented even if the caller panics.
    pub fn connection_active_guard(&self) -> ConnectionGuard {
        self.inner.connections_active.fetch_add(1, Relaxed);
        ConnectionGuard {
            inner: self.inner.clone(),
        }
    }

    // --- Histogram ---

    pub fn observe_message_handling(&self, duration: Duration) {
        self.inner.message_handling.observe(duration);
    }

    // --- Prometheus rendering ---

    /// Render complete caller-supplied snapshots. Call
    /// [`Self::render_prometheus_snapshot`] if any sampled count is unavailable.
    pub fn render_prometheus(
        &self,
        rooms_active: usize,
        participants_active: usize,
        live_workers: usize,
    ) -> String {
        self.render_prometheus_snapshot(rooms_active, Some(participants_active), Some(live_workers))
    }

    /// Render counters and available gauges. `None` omits the corresponding
    /// sample and emits its snapshot-complete gauge as zero; a known zero is
    /// emitted normally. This never substitutes stale/partial counts for a
    /// complete snapshot. Component snapshots are not globally atomic.
    pub fn render_prometheus_snapshot(
        &self,
        rooms_active: usize,
        participants_active: Option<usize>,
        live_workers: Option<usize>,
    ) -> String {
        let mut out = String::with_capacity(4096);

        let i = &self.inner;
        i.telemetry.render(&mut out);
        let _ = writeln!(
            out,
            "# HELP simplestchat_memberships_total Established or restored room memberships by path, not successful browser media setup\n# TYPE simplestchat_memberships_total counter"
        );
        for (path, counter) in [
            ("direct_join", &i.joins_total),
            ("lobby_admission", &i.lobby_admissions_total),
            ("reconnect", &i.reconnects_total),
        ] {
            let _ = writeln!(
                out,
                "simplestchat_memberships_total{{path=\"{path}\"}} {}",
                counter.load(Relaxed)
            );
        }
        let _ = writeln!(
            out,
            "# HELP simplestchat_media_worker_recoveries_total Attempted worker replacements by result\n# TYPE simplestchat_media_worker_recoveries_total counter"
        );
        for (index, outcome) in ["failed", "ok"].iter().enumerate() {
            let _ = writeln!(
                out,
                "simplestchat_media_worker_recoveries_total{{outcome=\"{outcome}\"}} {}",
                i.media_worker_recoveries[index].load(Relaxed)
            );
        }
        render_counter(
            &mut out,
            "simplestchat_auth_password_work_rejected_total",
            "Authentication password jobs refused because the bounded lane was full",
            i.password_work_rejected.load(Relaxed),
        );

        // Counters
        render_counter(
            &mut out,
            "simplestchat_connections_total",
            "WebSocket connection handlers started after upgrade",
            i.connections_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_messages_received_total",
            "Inbound WebSocket frames observed, including control and rejected frames",
            i.messages_received_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_messages_sent_total",
            "Completed application text-frame writes including shutdown notices, not peer acknowledgements",
            i.messages_sent_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_message_send_failed_total",
            "Application text-frame writes that failed or timed out, including shutdown notices",
            i.message_send_failed_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_outbound_queue_full_total",
            "Essential outbound enqueue attempts rejected by a full recipient queue",
            i.outbound_queue_full_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_outbound_queue_closed_total",
            "Essential outbound enqueue attempts rejected by a closed recipient queue",
            i.outbound_queue_closed_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_errors_total",
            "Invalid signaling messages and dispatched command rejections, not all server errors",
            i.errors_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_api_requests_rejected_total",
            "HTTP API requests answered 429 or 503 by rate limits, concurrency caps, the password lane or a busy service",
            i.api_requests_rejected_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_upgrades_rejected_total",
            "WebSocket upgrades refused by handshake, connection or per-IP limits",
            i.upgrades_rejected_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_media_worker_deaths_total",
            "Observed media worker deaths; does not imply successful replacement",
            i.media_worker_deaths_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_rooms_created_total",
            "Runtime room media setups completed, including setups later rolled back",
            i.rooms_created_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_joins_total",
            "Direct JoinRoom admissions completed by the dispatcher, excluding lobby admission and reconnect",
            i.joins_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_leaves_total",
            "Explicit room-leave operations completed by the dispatcher, excluding implicit or forced departures",
            i.leaves_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_producers_created_total",
            "Producer creation operations completed by the dispatcher",
            i.producers_created_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_consumers_created_total",
            "Consumer creation operations completed by the dispatcher",
            i.consumers_created_total.load(Relaxed),
        );

        // Gauges
        render_optional_gauge(
            &mut out,
            "simplestchat_media_workers_live",
            "Media workers with an open WebRTC listener, omitted when the snapshot is unavailable",
            live_workers,
        );
        render_gauge(
            &mut out,
            "simplestchat_media_workers_snapshot_complete",
            "Whether this scrape obtained the live-worker snapshot",
            u64::from(live_workers.is_some()),
        );
        render_gauge(
            &mut out,
            "simplestchat_connections_active",
            "Currently active WebSocket connections",
            i.connections_active.load(Relaxed),
        );
        render_gauge(
            &mut out,
            "simplestchat_rooms_active",
            "Currently active rooms",
            rooms_active as u64,
        );
        render_optional_gauge(
            &mut out,
            "simplestchat_participants_active",
            "Room memberships including disconnected grace sessions but excluding lobbies, omitted on incomplete snapshot",
            participants_active,
        );
        render_gauge(
            &mut out,
            "simplestchat_participants_snapshot_complete",
            "Whether every sampled room was readable for this scrape",
            u64::from(participants_active.is_some()),
        );

        // Histogram
        i.message_handling.render(
            "simplestchat_message_handling_seconds",
            "Dispatched signaling operation elapsed time including waits, excluding parsing, early rejections and reconnect",
            &mut out,
        );

        render_counter(
            &mut out,
            "simplestchat_joins_refused_saturated_total",
            "Fresh room joins refused because the process was CPU saturated",
            i.joins_refused_saturated_total.load(Relaxed),
        );
        render_counter(
            &mut out,
            "simplestchat_consumer_layer_requests_total",
            "Worker requests that changed a consumer's preferred layers (viewer ceiling and bandwidth tier merged server-side)",
            i.consumer_layer_requests_total.load(Relaxed),
        );
        if let Some(saturation) = i
            .saturation
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            render_gauge(
                &mut out,
                "simplestchat_cpu_saturated",
                "Whether the process is CPU saturated by cgroup throttling or pressure; readiness fails and fresh joins are refused while set",
                u64::from(saturation.saturated),
            );
            render_gauge(
                &mut out,
                "simplestchat_cpu_throttling_available",
                "Whether cgroup v2 throttling counters were readable for the last sample",
                u64::from(saturation.throttling_available),
            );
            render_gauge_f64(
                &mut out,
                "simplestchat_cpu_throttled_fraction",
                "Share of cgroup enforcement periods throttled over the sampling window",
                saturation.throttled_fraction,
            );
            render_gauge(
                &mut out,
                "simplestchat_cpu_pressure_available",
                "Whether cgroup v2 CPU pressure was readable for the last sample",
                u64::from(saturation.pressure_available),
            );
            render_gauge_f64(
                &mut out,
                "simplestchat_cpu_pressure_some_avg10",
                "cgroup CPU pressure, percent of time some task waited for CPU over ten seconds",
                saturation.pressure_avg10,
            );
        }
        render_worker_cpu(
            &mut out,
            &i.worker_cpu.read().unwrap_or_else(|e| e.into_inner()),
        );
        let quality = i.quality.read().unwrap_or_else(|e| e.into_inner());
        render_gauge(
            &mut out,
            "simplestchat_quality_sample_available",
            "One after the first media quality sample has been published",
            u64::from(quality.is_some()),
        );
        if let Some(quality) = quality.as_ref() {
            render_quality(&mut out, quality);
        }

        out
    }
}

/// What the CPU saturation monitor last observed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SaturationSnapshot {
    pub throttling_available: bool,
    pub pressure_available: bool,
    pub saturated: bool,
    pub throttled_fraction: f64,
    pub pressure_avg10: f64,
}

/// One media worker thread's CPU share over the saturation window, labelled
/// by its position in the pool.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkerCpuSnapshot {
    pub index: usize,
    /// Share of one core; `None` when the thread's time could not be read.
    pub utilization: Option<f64>,
    pub saturated: bool,
}

fn render_worker_cpu(out: &mut String, workers: &[WorkerCpuSnapshot]) {
    if workers.is_empty() {
        return;
    }
    let _ = writeln!(
        out,
        "# HELP simplestchat_media_worker_cpu Share of one core each media worker thread used over the saturation window (Linux, from schedstat)"
    );
    let _ = writeln!(out, "# TYPE simplestchat_media_worker_cpu gauge");
    for worker in workers {
        if let Some(utilization) = worker.utilization {
            let _ = writeln!(
                out,
                "simplestchat_media_worker_cpu{{worker=\"{}\"}} {utilization}",
                worker.index
            );
        }
    }
    let _ = writeln!(
        out,
        "# HELP simplestchat_media_worker_saturated Whether the worker is CPU saturated; its rooms refuse fresh joins and new rooms are placed elsewhere"
    );
    let _ = writeln!(out, "# TYPE simplestchat_media_worker_saturated gauge");
    for worker in workers {
        let _ = writeln!(
            out,
            "simplestchat_media_worker_saturated{{worker=\"{}\"}} {}",
            worker.index,
            u8::from(worker.saturated)
        );
    }
}

/// Media quality as the SFU sees it, from its own RTCP view and send path.
fn render_quality(out: &mut String, quality: &crate::media::quality::QualitySample) {
    use crate::media::quality::{BITRATE_BOUNDS, LOSS_BOUNDS};
    for (name, help, value) in [
        (
            "participants_available",
            "Participants present when sampling started",
            quality.participants_available,
        ),
        (
            "participants_sampled",
            "Participants whose media lock was available",
            quality.participants_sampled,
        ),
        (
            "transports_available",
            "Open receive transports found in inspected participants; incomplete when participant coverage is partial",
            quality.transports_available,
        ),
        (
            "transports_selected",
            "Receive transports selected by the per-sample cap",
            quality.transports_selected,
        ),
        (
            "transports_requested",
            "Receive transport requests started including failed and cancelled requests",
            quality.transports_requested,
        ),
        (
            "sample_complete",
            "One only when all participants and all discovered receive transports were successfully inspected",
            u64::from(
                quality.participants_available == quality.participants_sampled
                    && quality.transports_sampled == quality.transports_available
                    && !quality.budget_exhausted,
            ),
        ),
        (
            "sample_budget_exhausted",
            "One when the total sampling deadline expired",
            u64::from(quality.budget_exhausted),
        ),
    ] {
        render_gauge(out, &format!("simplestchat_quality_{name}"), help, value);
    }
    let _ = writeln!(
        out,
        "# HELP simplestchat_quality_sample_duration_seconds Wall time spent collecting the last sample\n# TYPE simplestchat_quality_sample_duration_seconds gauge\nsimplestchat_quality_sample_duration_seconds {}",
        quality.duration.as_secs_f64()
    );
    if let Some(timestamp) = quality
        .completed_wall_time
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
    {
        let _ = writeln!(
            out,
            "# HELP simplestchat_quality_sample_timestamp_seconds Unix time when the latest complete or partial sample finished\n# TYPE simplestchat_quality_sample_timestamp_seconds gauge\nsimplestchat_quality_sample_timestamp_seconds {}",
            timestamp.as_secs_f64()
        );
    }
    if let Some(at) = quality.completed_at {
        let _ = writeln!(
            out,
            "# HELP simplestchat_quality_sample_age_seconds Seconds since the latest completed or partial sample was published\n# TYPE simplestchat_quality_sample_age_seconds gauge\nsimplestchat_quality_sample_age_seconds {}",
            at.elapsed().as_secs_f64()
        );
    }

    render_gauge(
        out,
        "simplestchat_quality_consumers",
        "Consumers in the last media quality sample",
        quality.consumers,
    );
    render_gauge(
        out,
        "simplestchat_quality_consumers_paused",
        "Sampled consumers paused by the viewer or by the producer",
        quality.consumers_paused,
    );
    let score_labels: Vec<String> = (0..=10).map(|score| score.to_string()).collect();
    render_snapshot_buckets(
        out,
        "simplestchat_quality_consumer_score",
        "mediasoup consumer transmission score from 0 to 10 across sampled consumers",
        &score_labels,
        &quality.consumer_scores,
        quality.consumer_score_sum as f64,
    );
    let _ = writeln!(
        out,
        "# HELP simplestchat_quality_video_consumers_by_spatial_layer Video consumers in each current spatial layer; none includes paused or unknown layers\n# TYPE simplestchat_quality_video_consumers_by_spatial_layer gauge"
    );
    for (layer, count) in quality.video_consumers_by_spatial.iter().enumerate() {
        let label = if layer == quality.video_consumers_by_spatial.len() - 1 {
            "none".to_string()
        } else {
            layer.to_string()
        };
        let _ = writeln!(
            out,
            "simplestchat_quality_video_consumers_by_spatial_layer{{layer=\"{label}\"}} {count}"
        );
    }
    render_gauge(
        out,
        "simplestchat_quality_producers",
        "Producers with a stream score in the last media quality sample",
        quality.producers,
    );
    render_snapshot_buckets(
        out,
        "simplestchat_quality_producer_score",
        "mediasoup producer worst-stream score from 0 to 10 across sampled producers",
        &score_labels,
        &quality.producer_scores,
        quality.producer_score_sum as f64,
    );
    render_gauge(
        out,
        "simplestchat_quality_transports_sampled",
        "Receive transports with successful nonempty statistics in the last sample",
        quality.transports_sampled,
    );
    render_gauge(
        out,
        "simplestchat_quality_transport_stats_failed",
        "Receive transport statistics requests that failed or timed out in the last sample",
        quality.transport_stats_failed,
    );
    let loss_labels: Vec<String> = LOSS_BOUNDS.iter().map(|bound| bound.to_string()).collect();
    render_snapshot_buckets(
        out,
        "simplestchat_quality_downlink_loss",
        "Fraction of RTP packets the viewer reported lost on sampled receive transports (transport-cc feedback)",
        &loss_labels,
        &quality.loss_buckets,
        quality.loss_sum,
    );
    let bitrate_labels: Vec<String> = BITRATE_BOUNDS
        .iter()
        .map(|bound| bound.to_string())
        .collect();
    render_snapshot_buckets(
        out,
        "simplestchat_quality_available_outgoing_bitrate",
        "Bandwidth estimate in bit/s toward the viewer on sampled receive transports",
        &bitrate_labels,
        &quality.bitrate_buckets,
        quality.bitrate_sum as f64,
    );
}

/// Current distributions are gauges: a new sample replaces their observations.
/// Each interval is disjoint (not a cumulative Prometheus histogram).
fn render_snapshot_buckets(
    out: &mut String,
    name: &str,
    help: &str,
    labels: &[String],
    counts: &[u64],
    sum: f64,
) {
    let _ = writeln!(
        out,
        "# HELP {name} {help}; disjoint current-sample intervals"
    );
    let _ = writeln!(out, "# TYPE {name} gauge");
    for (index, count) in counts.iter().enumerate() {
        let label = labels.get(index).map_or("+Inf", String::as_str);
        let _ = writeln!(out, "{name}{{upper_bound=\"{label}\"}} {count}");
    }
    let _ = writeln!(
        out,
        "# HELP {name}_sum Sum of observations in the current sample\n# TYPE {name}_sum gauge"
    );
    let _ = writeln!(out, "{name}_sum {sum}");
    render_gauge(
        out,
        &format!("{name}_count"),
        "Observations in the current sample",
        counts.iter().sum(),
    );
}

/// RAII guard that decrements `connections_active` on drop.
/// Prevents gauge underflow/drift if the connection handler panics.
pub struct ConnectionGuard {
    inner: Arc<Inner>,
}

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.inner.connections_active.fetch_sub(1, Relaxed);
    }
}

fn render_counter(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} counter");
    let _ = writeln!(out, "{name} {value}");
}

/// Append a gauge computed by the caller at scrape time.
pub fn append_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    render_gauge(out, name, help, value);
}

fn render_gauge_f64(out: &mut String, name: &str, help: &str, value: f64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

fn render_gauge(out: &mut String, name: &str, help: &str, value: u64) {
    let _ = writeln!(out, "# HELP {name} {help}");
    let _ = writeln!(out, "# TYPE {name} gauge");
    let _ = writeln!(out, "{name} {value}");
}

fn render_optional_gauge(out: &mut String, name: &str, help: &str, value: Option<usize>) {
    if let Some(value) = value {
        render_gauge(out, name, help, value as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(body: &str, name: &str) -> Option<u64> {
        body.lines().find_map(|line| {
            line.strip_prefix(name)
                .and_then(|suffix| suffix.strip_prefix(' '))
                .map(|value| value.parse().unwrap())
        })
    }

    #[test]
    fn saturation_and_quality_samples_render_as_gauges() {
        let metrics = ServerMetrics::new();
        let body = metrics.render_prometheus(0, 0, 1);
        assert!(
            !body.contains("simplestchat_cpu_saturated"),
            "absent until sampled"
        );
        assert!(
            !body.contains("simplestchat_quality_consumers "),
            "absent until sampled"
        );
        metrics.set_saturation(SaturationSnapshot {
            throttling_available: true,
            pressure_available: false,
            saturated: true,
            throttled_fraction: 0.75,
            pressure_avg10: 0.0,
        });
        let mut sample = crate::media::quality::QualitySample::default();
        sample.record_consumer(10, Some(2), true, false);
        sample.record_consumer(6, Some(1), true, false);
        sample.record_producer([9, 7]);
        sample.record_transport(Some(0.03), Some(450_000));
        metrics.set_quality_sample(sample);
        metrics.inc_join_refused_saturated();
        let body = metrics.render_prometheus(0, 0, 1);
        assert_eq!(value(&body, "simplestchat_cpu_saturated"), Some(1));
        assert_eq!(
            value(&body, "simplestchat_cpu_throttling_available"),
            Some(1)
        );
        assert_eq!(value(&body, "simplestchat_cpu_pressure_available"), Some(0));
        assert!(body.contains("simplestchat_cpu_throttled_fraction 0.75\n"));
        assert_eq!(
            value(&body, "simplestchat_joins_refused_saturated_total"),
            Some(1)
        );
        assert_eq!(value(&body, "simplestchat_quality_consumers"), Some(2));
        assert!(body.contains("simplestchat_quality_consumer_score{upper_bound=\"6\"} 1\n"));
        assert!(body.contains("simplestchat_quality_consumer_score{upper_bound=\"10\"} 1\n"));
        assert!(body.contains("simplestchat_quality_consumer_score_count 2\n"));
        assert!(
            body.contains("simplestchat_quality_video_consumers_by_spatial_layer{layer=\"1\"} 1\n")
        );
        assert!(body.contains("simplestchat_quality_producer_score{upper_bound=\"7\"} 1\n"));
        assert!(body.contains("simplestchat_quality_downlink_loss{upper_bound=\"0.05\"} 1\n"));
        assert!(body.contains("simplestchat_quality_downlink_loss{upper_bound=\"0.02\"} 0\n"));
        assert!(body.contains("simplestchat_quality_downlink_loss{upper_bound=\"+Inf\"} 0\n"));
        assert!(body.contains(
            "simplestchat_quality_available_outgoing_bitrate{upper_bound=\"600000\"} 1\n"
        ));
        assert!(body.contains("simplestchat_quality_available_outgoing_bitrate_sum 450000\n"));
        assert!(body.contains("# TYPE simplestchat_quality_consumer_score gauge\n"));
        assert!(!body.contains("simplestchat_quality_consumer_score_bucket"));
        let empty = crate::media::quality::QualitySample {
            completed_at: Some(std::time::Instant::now()),
            ..Default::default()
        };
        metrics.set_quality_sample(empty);
        let body = metrics.render_prometheus(0, 0, 1);
        assert!(body.contains("simplestchat_quality_consumer_score_count 0\n"));
        assert!(body.contains("simplestchat_quality_sample_complete 1\n"));
    }

    #[test]
    fn rejections_are_exported_as_counters() {
        let metrics = ServerMetrics::new();
        metrics.inc_api_request_rejected();
        metrics.inc_upgrade_rejected();
        metrics.inc_upgrade_rejected();
        metrics.inc_media_worker_death();
        let body = metrics.render_prometheus(0, 0, 1);
        assert_eq!(
            value(&body, "simplestchat_media_worker_deaths_total"),
            Some(1)
        );
        assert_eq!(
            value(&body, "simplestchat_api_requests_rejected_total"),
            Some(1)
        );
        assert_eq!(
            value(&body, "simplestchat_upgrades_rejected_total"),
            Some(2)
        );
    }

    #[test]
    fn histogram_boundaries_are_cumulative_and_count_matches_infinity() {
        let histogram = Histogram::new();
        for microseconds in [0, 1_000, 1_001, 5_000, 5_001, 5_000_000, 5_000_001] {
            histogram.observe(Duration::from_micros(microseconds));
        }
        let mut output = String::new();
        histogram.render("test_seconds", "Test duration", &mut output);
        assert_eq!(value(&output, "test_seconds_bucket{le=\"0.001\"}"), Some(2));
        assert_eq!(value(&output, "test_seconds_bucket{le=\"0.005\"}"), Some(4));
        assert_eq!(value(&output, "test_seconds_bucket{le=\"0.01\"}"), Some(5));
        assert_eq!(value(&output, "test_seconds_bucket{le=\"5\"}"), Some(6));
        assert_eq!(value(&output, "test_seconds_bucket{le=\"+Inf\"}"), Some(7));
        assert_eq!(value(&output, "test_seconds_count"), Some(7));
        assert!(output.contains("test_seconds_sum 10.012003\n"));
        assert_eq!(
            histogram
                .buckets
                .iter()
                .map(|bucket| bucket.load(Relaxed))
                .sum::<u64>(),
            7,
            "each observation updates exactly one interval"
        );
    }

    #[test]
    fn histogram_interleaved_observations_cannot_invert_cumulative_buckets() {
        let histogram = Histogram::new();
        histogram.observe(Duration::from_micros(1));
        let snapshot = Histogram::cumulative_buckets(|index| {
            if index == 1 {
                // The first interval has already been sampled. Both a new
                // observation there and one in a later interval interleave
                // deterministically with this scrape.
                histogram.observe(Duration::from_micros(1));
                histogram.observe(Duration::from_millis(6));
            }
            histogram.buckets[index].load(Relaxed)
        });
        assert_eq!(snapshot[0], 1);
        assert_eq!(snapshot[1], 1);
        assert_eq!(snapshot[2], 2);
        assert_eq!(snapshot[10], 2);
        assert!(snapshot.windows(2).all(|pair| pair[0] <= pair[1]));
        let next = Histogram::cumulative_buckets(|index| histogram.buckets[index].load(Relaxed));
        assert_eq!(next[0], 2);
        assert_eq!(next[10], 3);
        assert!(next.windows(2).all(|pair| pair[0] <= pair[1]));
    }

    #[test]
    fn histogram_empty_snapshot_has_zero_count_and_sum() {
        let histogram = Histogram::new();
        let mut output = String::new();
        histogram.render("test_seconds", "Test duration", &mut output);
        assert_eq!(value(&output, "test_seconds_bucket{le=\"+Inf\"}"), Some(0));
        assert_eq!(value(&output, "test_seconds_count"), Some(0));
        assert!(output.contains("test_seconds_sum 0.000000\n"));
    }

    #[test]
    fn unavailable_snapshots_are_not_reported_as_zero_or_stale_values() {
        let metrics = ServerMetrics::new();
        let known = metrics.render_prometheus(2, 7, 1);
        assert_eq!(value(&known, "simplestchat_participants_active"), Some(7));
        let unknown = metrics.render_prometheus_snapshot(2, None, None);
        assert_eq!(value(&unknown, "simplestchat_participants_active"), None);
        assert_eq!(value(&unknown, "simplestchat_media_workers_live"), None);
        assert_eq!(
            value(&unknown, "simplestchat_participants_snapshot_complete"),
            Some(0)
        );
        assert_eq!(
            value(&unknown, "simplestchat_media_workers_snapshot_complete"),
            Some(0)
        );
        assert_eq!(value(&unknown, "simplestchat_rooms_active"), Some(2));
        let worker_only = metrics.render_prometheus_snapshot(2, None, Some(1));
        assert_eq!(
            value(&worker_only, "simplestchat_participants_active"),
            None
        );
        assert_eq!(
            value(&worker_only, "simplestchat_media_workers_live"),
            Some(1)
        );
        assert_eq!(
            value(&worker_only, "simplestchat_participants_snapshot_complete"),
            Some(0)
        );
        assert_eq!(
            value(&worker_only, "simplestchat_media_workers_snapshot_complete"),
            Some(1)
        );
        let empty = metrics.render_prometheus_snapshot(0, Some(0), Some(0));
        assert_eq!(value(&empty, "simplestchat_participants_active"), Some(0));
        assert_eq!(value(&empty, "simplestchat_media_workers_live"), Some(0));
        assert_eq!(
            value(&empty, "simplestchat_participants_snapshot_complete"),
            Some(1)
        );
        assert_eq!(
            value(&empty, "simplestchat_media_workers_snapshot_complete"),
            Some(1)
        );
    }

    #[test]
    fn worker_cpu_gauges_are_labelled_by_worker_index() {
        let metrics = ServerMetrics::new();
        assert!(
            !metrics
                .render_prometheus(0, 0, 1)
                .contains("simplestchat_media_worker_cpu")
        );
        metrics.set_worker_cpu(vec![
            WorkerCpuSnapshot {
                index: 0,
                utilization: Some(0.339),
                saturated: false,
            },
            WorkerCpuSnapshot {
                index: 1,
                utilization: None,
                saturated: true,
            },
        ]);
        let body = metrics.render_prometheus(0, 0, 2);
        assert!(
            body.contains("simplestchat_media_worker_cpu{worker=\"0\"} 0.339\n"),
            "{body}"
        );
        assert!(
            !body.contains("simplestchat_media_worker_cpu{worker=\"1\"}"),
            "{body}"
        );
        assert!(body.contains("simplestchat_media_worker_saturated{worker=\"0\"} 0\n"));
        assert!(body.contains("simplestchat_media_worker_saturated{worker=\"1\"} 1\n"));
    }

    #[test]
    fn successful_writes_failed_writes_and_enqueue_rejections_are_independent() {
        let metrics = ServerMetrics::with_diagnostics(Diagnostics::default());
        let other = metrics.clone();
        metrics.inc_messages_sent();
        other.inc_message_send_failed();
        other.inc_message_send_failed();
        metrics.inc_outbound_queue_full();
        metrics.inc_outbound_queue_closed();
        metrics.inc_joins();
        metrics.inc_leaves();
        let body = other.render_prometheus(0, 0, 0);
        assert_eq!(value(&body, "simplestchat_messages_sent_total"), Some(1));
        assert_eq!(
            value(&body, "simplestchat_message_send_failed_total"),
            Some(2)
        );
        assert_eq!(
            value(&body, "simplestchat_outbound_queue_full_total"),
            Some(1)
        );
        assert_eq!(
            value(&body, "simplestchat_outbound_queue_closed_total"),
            Some(1)
        );
        assert_eq!(value(&body, "simplestchat_errors_total"), Some(0));
        assert!(body.contains("excluding lobby admission and reconnect"));
        assert!(body.contains("excluding implicit or forced departures"));
    }

    #[test]
    fn live_worker_gauge_uses_each_current_snapshot() {
        let metrics = ServerMetrics::new();
        let active = metrics.render_prometheus(2, 5, 3);
        assert!(active.contains("# TYPE simplestchat_media_workers_live gauge\n"));
        assert!(active.contains("simplestchat_media_workers_live 3\n"));
        let closed = metrics.render_prometheus(2, 5, 0);
        assert!(closed.contains("simplestchat_media_workers_live 0\n"));
        assert!(closed.contains("simplestchat_rooms_active 2\n"));
        assert!(closed.contains("simplestchat_participants_active 5\n"));
    }
}
