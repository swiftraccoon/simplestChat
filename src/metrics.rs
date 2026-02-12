#![forbid(unsafe_code)]

// Server metrics — lock-free AtomicU64 counters and Prometheus-compatible histogram.

use std::fmt::Write;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::Arc;
use std::time::Duration;

/// Fixed histogram bucket boundaries (in microseconds for internal storage).
const BUCKET_BOUNDS_US: [u64; 10] = [
    1_000,      // 1ms
    5_000,      // 5ms
    10_000,     // 10ms
    25_000,     // 25ms
    50_000,     // 50ms
    100_000,    // 100ms
    250_000,    // 250ms
    500_000,    // 500ms
    1_000_000,  // 1s
    5_000_000,  // 5s
];

/// Prometheus-compatible cumulative histogram with fixed buckets.
pub struct Histogram {
    /// Cumulative bucket counters — bucket[i] counts observations <= BUCKET_BOUNDS_US[i]
    buckets: [AtomicU64; 10],
    /// +Inf bucket (total count)
    count: AtomicU64,
    /// Sum of all observations in microseconds
    sum_us: AtomicU64,
}

impl Histogram {
    fn new() -> Self {
        Self {
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            count: AtomicU64::new(0),
            sum_us: AtomicU64::new(0),
        }
    }

    /// Record a duration observation.
    pub fn observe(&self, duration: Duration) {
        let us = duration.as_micros() as u64;
        self.sum_us.fetch_add(us, Relaxed);
        self.count.fetch_add(1, Relaxed);
        for (i, &bound) in BUCKET_BOUNDS_US.iter().enumerate() {
            if us <= bound {
                self.buckets[i].fetch_add(1, Relaxed);
            }
        }
    }

    /// Render in Prometheus text exposition format.
    fn render(&self, name: &str, help: &str, out: &mut String) {
        let _ = writeln!(out, "# HELP {name} {help}");
        let _ = writeln!(out, "# TYPE {name} histogram");

        let labels = [
            "0.001", "0.005", "0.01", "0.025", "0.05",
            "0.1", "0.25", "0.5", "1", "5",
        ];
        for (i, label) in labels.iter().enumerate() {
            let val = self.buckets[i].load(Relaxed);
            let _ = writeln!(out, "{name}_bucket{{le=\"{label}\"}} {val}");
        }
        let count = self.count.load(Relaxed);
        let _ = writeln!(out, "{name}_bucket{{le=\"+Inf\"}} {count}");
        let sum_us = self.sum_us.load(Relaxed);
        // Convert microseconds to seconds with 6 decimal places
        let _ = writeln!(out, "{name}_sum {}.{:06}", sum_us / 1_000_000, sum_us % 1_000_000);
        let _ = writeln!(out, "{name}_count {count}");
    }
}

/// Server-wide metrics using lock-free atomics.
#[derive(Clone)]
pub struct ServerMetrics {
    inner: Arc<Inner>,
}

struct Inner {
    // Monotonic counters
    connections_total: AtomicU64,
    messages_received_total: AtomicU64,
    messages_sent_total: AtomicU64,
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

impl ServerMetrics {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                connections_total: AtomicU64::new(0),
                messages_received_total: AtomicU64::new(0),
                messages_sent_total: AtomicU64::new(0),
                errors_total: AtomicU64::new(0),
                rooms_created_total: AtomicU64::new(0),
                joins_total: AtomicU64::new(0),
                leaves_total: AtomicU64::new(0),
                producers_created_total: AtomicU64::new(0),
                consumers_created_total: AtomicU64::new(0),
                connections_active: AtomicU64::new(0),
                message_handling: Histogram::new(),
            }),
        }
    }

    // --- Counter increments ---

    pub fn inc_connections_total(&self) {
        self.inner.connections_total.fetch_add(1, Relaxed);
    }

    pub fn inc_messages_received(&self) {
        self.inner.messages_received_total.fetch_add(1, Relaxed);
    }

    pub fn inc_messages_sent(&self) {
        self.inner.messages_sent_total.fetch_add(1, Relaxed);
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
        ConnectionGuard { inner: self.inner.clone() }
    }

    // --- Histogram ---

    pub fn observe_message_handling(&self, duration: Duration) {
        self.inner.message_handling.observe(duration);
    }

    // --- Prometheus rendering ---

    /// Render all metrics in Prometheus text exposition format.
    /// `rooms_active` and `participants_active` are passed in from RoomManager (on-demand gauges).
    pub fn render_prometheus(&self, rooms_active: usize, participants_active: usize) -> String {
        let mut out = String::with_capacity(4096);

        let i = &self.inner;

        // Counters
        render_counter(&mut out, "simplestchat_connections_total", "Total WebSocket connections", i.connections_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_messages_received_total", "Total messages received from clients", i.messages_received_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_messages_sent_total", "Total messages sent to clients", i.messages_sent_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_errors_total", "Total errors", i.errors_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_rooms_created_total", "Total rooms created", i.rooms_created_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_joins_total", "Total room joins", i.joins_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_leaves_total", "Total room leaves", i.leaves_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_producers_created_total", "Total producers created", i.producers_created_total.load(Relaxed));
        render_counter(&mut out, "simplestchat_consumers_created_total", "Total consumers created", i.consumers_created_total.load(Relaxed));

        // Gauges
        render_gauge(&mut out, "simplestchat_connections_active", "Currently active WebSocket connections", i.connections_active.load(Relaxed));
        render_gauge(&mut out, "simplestchat_rooms_active", "Currently active rooms", rooms_active as u64);
        render_gauge(&mut out, "simplestchat_participants_active", "Currently active participants", participants_active as u64);

        // Histogram
        i.message_handling.render(
            "simplestchat_message_handling_seconds",
            "Message handling latency in seconds",
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
