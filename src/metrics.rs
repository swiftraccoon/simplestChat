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
    leaves_total: AtomicU64,
    producers_created_total: AtomicU64,
    consumers_created_total: AtomicU64,

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
                connections_total: AtomicU64::new(0),
                messages_received_total: AtomicU64::new(0),
                messages_sent_total: AtomicU64::new(0),
                message_send_failed_total: AtomicU64::new(0),
                outbound_queue_full_total: AtomicU64::new(0),
                outbound_queue_closed_total: AtomicU64::new(0),
                errors_total: AtomicU64::new(0),
                rooms_created_total: AtomicU64::new(0),
                joins_total: AtomicU64::new(0),
                leaves_total: AtomicU64::new(0),
                producers_created_total: AtomicU64::new(0),
                consumers_created_total: AtomicU64::new(0),
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

    pub fn inc_errors(&self) {
        self.inner.errors_total.fetch_add(1, Relaxed);
    }

    pub fn inc_rooms_created(&self) {
        self.inner.rooms_created_total.fetch_add(1, Relaxed);
    }

    pub fn inc_joins(&self) {
        self.inner.joins_total.fetch_add(1, Relaxed);
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

        out
    }
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
