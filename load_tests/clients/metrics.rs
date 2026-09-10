use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

include!("measurement.rs");

/// Metrics collected during a test client session
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientMetrics {
    pub client_id: String,
    pub room_id: String,
    pub connection_successful: bool,
    pub connection_time_ms: u64,
    pub time_to_first_media_sent_ms: Option<u64>,
    pub time_to_first_media_received_ms: Option<u64>,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub errors: Vec<String>,
    pub session_duration_ms: u64,
    pub producers_created: u32,
    pub consumers_created: u32,
    pub signaling_latencies: SignalingLatencyReport,
    pub reconnections: u32,
    pub reconnection_failures: u32,
    pub connection_attempts: Vec<ConnectionAttempt>,
    pub measurement: MeasurementMetrics,
    pub consumer_delivery: Vec<ConsumerDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<DiagnosticReport>,
}

/// Opt-in, bounded lifecycle evidence. Times are relative to client creation,
/// not the measurement window. No packet-by-packet events are collected.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReport {
    pub events: Vec<DiagnosticEntry>,
    pub snapshots: Vec<DiagnosticEntry>,
    pub failures: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticEntry {
    pub attempt: usize,
    pub elapsed_ms: u64,
    pub kind: String,
    pub details: serde_json::Value,
}

const MAX_DIAGNOSTIC_EVENTS: usize = 4096;
const MAX_DIAGNOSTIC_SNAPSHOTS: usize = 128;

/// Signaling latency report per operation
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignalingLatencyReport {
    pub operations: HashMap<String, LatencyStats>,
}

/// Latency statistics for a single operation type
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatencyStats {
    pub count: usize,
    pub min_ms: u64,
    pub max_ms: u64,
    pub avg_ms: u64,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub p99_ms: u64,
    /// Exact millisecond frequencies; aggregation never takes percentiles of medians.
    pub histogram_ms: std::collections::BTreeMap<u64, u64>,
}

/// Real-time metrics collector (thread-safe)
pub struct MetricsCollector {
    client_id: String,
    room_id: std::sync::Mutex<String>,
    start_time: Instant,
    observations: Measurements,
    connection_successful: AtomicBool,
    connection_time_ms: AtomicU64,
    first_media_sent: AtomicU64,     // 0 = not set
    first_media_received: AtomicU64, // 0 = not set
    packets_sent: AtomicU64,
    packets_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    producers_created: AtomicU64,
    consumers_created: AtomicU64,
    errors: std::sync::Mutex<Vec<String>>,
    signaling_latencies: std::sync::Mutex<HashMap<String, std::collections::BTreeMap<u64, u64>>>,
    reconnections: AtomicU64,
    reconnection_failures: AtomicU64,
    diagnostics_enabled: AtomicBool,
    diagnostics: std::sync::Mutex<DiagnosticReport>,
}

impl MetricsCollector {
    pub fn new(client_id: String) -> Self {
        Self::with_window(
            client_id,
            std::sync::Arc::new(MeasurementWindow::new(
                Instant::now(),
                std::time::Duration::from_secs(30),
            )),
        )
    }

    pub fn with_window(client_id: String, window: std::sync::Arc<MeasurementWindow>) -> Self {
        let now = Instant::now();
        Self {
            client_id,
            room_id: std::sync::Mutex::new(String::new()),
            start_time: now,
            observations: Measurements::new(window),
            connection_successful: AtomicBool::new(false),
            connection_time_ms: AtomicU64::new(0),
            first_media_sent: AtomicU64::new(0),
            first_media_received: AtomicU64::new(0),
            packets_sent: AtomicU64::new(0),
            packets_received: AtomicU64::new(0),
            bytes_sent: AtomicU64::new(0),
            bytes_received: AtomicU64::new(0),
            producers_created: AtomicU64::new(0),
            consumers_created: AtomicU64::new(0),
            errors: std::sync::Mutex::new(Vec::new()),
            signaling_latencies: std::sync::Mutex::new(HashMap::new()),
            reconnections: AtomicU64::new(0),
            reconnection_failures: AtomicU64::new(0),
            diagnostics_enabled: AtomicBool::new(false),
            diagnostics: std::sync::Mutex::new(DiagnosticReport::default()),
        }
    }

    pub fn enable_diagnostics(&self) {
        self.diagnostics_enabled.store(true, Ordering::Relaxed);
    }

    pub fn diagnostics_enabled(&self) -> bool {
        self.diagnostics_enabled.load(Ordering::Relaxed)
    }

    pub fn diagnostic_attempt(&self) -> usize {
        self.attempts.lock().unwrap().len()
    }

    pub fn diagnostic_event(&self, kind: &str, details: serde_json::Value) {
        if self.diagnostics_enabled() {
            self.diagnostic_event_for_attempt(self.diagnostic_attempt(), kind, details);
        }
    }

    pub fn diagnostic_event_for_attempt(
        &self,
        attempt: usize,
        kind: &str,
        details: serde_json::Value,
    ) {
        if !self.diagnostics_enabled() {
            return;
        }
        self.push_diagnostic(attempt, kind, details, false);
    }

    pub fn diagnostic_snapshot(&self, details: serde_json::Value) {
        if self.diagnostics_enabled() {
            self.push_diagnostic(self.diagnostic_attempt(), "pre-close", details, true);
        }
    }

    fn push_diagnostic(
        &self,
        attempt: usize,
        kind: &str,
        details: serde_json::Value,
        snapshot: bool,
    ) {
        let mut report = self.diagnostics.lock().unwrap();
        let (entries, limit) = if snapshot {
            (&mut report.snapshots, MAX_DIAGNOSTIC_SNAPSHOTS)
        } else {
            (&mut report.events, MAX_DIAGNOSTIC_EVENTS)
        };
        if entries.len() < limit {
            entries.push(DiagnosticEntry {
                attempt,
                elapsed_ms: self.start_time.elapsed().as_millis() as u64,
                kind: kind.to_string(),
                details,
            });
        } else {
            drop(report);
            self.diagnostic_failure("Diagnostic capture limit exceeded; evidence is incomplete");
        }
    }

    pub fn diagnostic_failure(&self, reason: &str) {
        if !self.diagnostics_enabled() {
            return;
        }
        let failure = format!("Attempt {}: {reason}", self.diagnostic_attempt());
        let mut report = self.diagnostics.lock().unwrap();
        if !report.failures.contains(&failure) {
            report.failures.push(failure.clone());
            drop(report);
            self.record_error(format!("Diagnostic failure: {failure}"));
        }
    }

    pub fn set_room_id(&self, room_id: &str) {
        if let Ok(mut r) = self.room_id.lock() {
            *r = room_id.to_string();
        }
    }

    pub fn mark_connection_successful(&self) {
        let elapsed = self.attempt_elapsed_ms();
        if !self.connection_successful.swap(true, Ordering::SeqCst) {
            self.connection_time_ms.store(elapsed, Ordering::SeqCst);
        }
        if let Some(attempt) = self.attempts.lock().unwrap().last_mut() {
            attempt.room_join_ms = Some(elapsed);
        }
    }

    pub fn mark_first_media_sent(&self) {
        let current = self.first_media_sent.load(Ordering::SeqCst);
        if current == 0 {
            let elapsed = self.attempt_elapsed_ms().max(1);
            self.first_media_sent.store(elapsed, Ordering::SeqCst);
        }
    }

    pub fn mark_first_media_received(&self) {
        let current = self.first_media_received.load(Ordering::SeqCst);
        if current == 0 {
            let elapsed = self.attempt_elapsed_ms().max(1);
            self.first_media_received.store(elapsed, Ordering::SeqCst);
        }
    }

    pub fn record_packet_sent(&self, size: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(size as u64, Ordering::Relaxed);
        self.record_queued(size);
    }

    pub fn record_packet_received(&self, size: usize) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received
            .fetch_add(size as u64, Ordering::Relaxed);
        self.record_received(size);
    }

    pub fn record_rtp_received(&self, ssrc: u32, size: usize) {
        self.record_packet_received(size);
        self.record_ssrc(ssrc);
    }

    pub fn record_producer_created(&self) {
        self.producers_created.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_consumer_created(&self) {
        self.consumers_created.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_error(&self, error: String) {
        if let Ok(mut errors) = self.errors.lock() {
            errors.push(error);
        }
    }

    /// Record a signaling round-trip latency for a named operation
    pub fn record_signaling_latency(&self, operation: &str, ms: u64) {
        if let Ok(mut latencies) = self.signaling_latencies.lock() {
            *latencies
                .entry(operation.to_string())
                .or_default()
                .entry(ms)
                .or_default() += 1;
        }
    }

    pub fn record_reconnection(&self) {
        self.reconnections.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_reconnection_failure(&self) {
        self.reconnection_failures.fetch_add(1, Ordering::Relaxed);
    }

    /// Generate final metrics report (sync — safe to call from OS thread)
    pub fn generate_report(&self) -> ClientMetrics {
        let session_duration = self.start_time.elapsed().as_millis() as u64;
        let errors = self.errors.lock().map(|e| e.clone()).unwrap_or_default();
        let room_id = self.room_id.lock().map(|r| r.clone()).unwrap_or_default();

        let first_sent = self.first_media_sent.load(Ordering::SeqCst);
        let first_received = self.first_media_received.load(Ordering::SeqCst);

        let signaling_latencies = self.compute_latency_report();

        ClientMetrics {
            client_id: self.client_id.clone(),
            room_id,
            connection_successful: self.connection_successful.load(Ordering::SeqCst),
            connection_time_ms: self.connection_time_ms.load(Ordering::SeqCst),
            time_to_first_media_sent_ms: if first_sent > 0 {
                Some(first_sent)
            } else {
                None
            },
            time_to_first_media_received_ms: if first_received > 0 {
                Some(first_received)
            } else {
                None
            },
            total_packets_sent: self.packets_sent.load(Ordering::Relaxed),
            total_packets_received: self.packets_received.load(Ordering::Relaxed),
            total_bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            total_bytes_received: self.bytes_received.load(Ordering::Relaxed),
            errors,
            session_duration_ms: session_duration,
            producers_created: self.producers_created.load(Ordering::Relaxed) as u32,
            consumers_created: self.consumers_created.load(Ordering::Relaxed) as u32,
            signaling_latencies,
            reconnections: self.reconnections.load(Ordering::Relaxed) as u32,
            reconnection_failures: self.reconnection_failures.load(Ordering::Relaxed) as u32,
            connection_attempts: self.attempts.lock().unwrap().clone(),
            measurement: self.measurement.lock().unwrap().clone(),
            consumer_delivery: self.delivery_report(),
            diagnostics: self
                .diagnostics_enabled()
                .then(|| self.diagnostics.lock().unwrap().clone()),
        }
    }

    fn compute_latency_report(&self) -> SignalingLatencyReport {
        let latencies = self
            .signaling_latencies
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default();

        let mut operations = HashMap::new();
        for (op, histogram) in latencies {
            operations.insert(op, histogram_stats(histogram));
        }

        SignalingLatencyReport { operations }
    }
}

impl std::ops::Deref for MetricsCollector {
    type Target = Measurements;
    fn deref(&self) -> &Self::Target {
        &self.observations
    }
}

/// Per-room summary statistics
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSummary {
    pub room_id: String,
    pub total_clients: usize,
    pub successful_connections: usize,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
    pub total_errors: usize,
    pub total_producers_created: u32,
    pub total_consumers_created: u32,
}

/// Aggregated signaling latency across all clients
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AggregatedLatencies {
    pub operations: HashMap<String, LatencyStats>,
}

/// Aggregates metrics from multiple clients
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TestSummary {
    pub total_clients: usize,
    pub successful_connections: usize,
    pub failed_connections: usize,
    pub average_connection_time_ms: u64,
    pub p50_connection_time_ms: u64,
    pub p95_connection_time_ms: u64,
    pub p99_connection_time_ms: u64,
    pub total_packets_sent: u64,
    pub total_packets_received: u64,
    pub total_bytes_sent: u64,
    pub total_bytes_received: u64,
    pub average_session_duration_ms: u64,
    pub total_errors: usize,
    pub total_producers_created: u32,
    pub total_consumers_created: u32,
    pub signaling_latencies: AggregatedLatencies,
    pub rooms: Vec<RoomSummary>,
    pub total_reconnections: u32,
    pub total_reconnection_failures: u32,
    pub connection_attempts: usize,
    pub failed_connection_attempts: usize,
    pub send_media_ready: LatencyStats,
    pub receive_media_ready: LatencyStats,
    pub measurement: MeasurementMetrics,
    pub validated_consumers: usize,
    pub failed_consumers: usize,
    pub skipped_short_lived_consumers: usize,
    #[serde(default)]
    pub diagnostic_failures: usize,
}

impl TestSummary {
    pub fn from_metrics(metrics: &[ClientMetrics]) -> Self {
        if metrics.is_empty() {
            return Self::default();
        }

        let total_clients = metrics.len();
        let successful_connections = metrics.iter().filter(|m| m.connection_successful).count();
        let failed_connections = total_clients - successful_connections;

        // Connection time statistics
        let mut connection_times: Vec<u64> = metrics
            .iter()
            .flat_map(|m| &m.connection_attempts)
            .filter_map(|attempt| attempt.room_join_ms)
            .collect();
        connection_times.sort_unstable();

        let avg_connection_time = if !connection_times.is_empty() {
            connection_times.iter().sum::<u64>() / connection_times.len() as u64
        } else {
            0
        };

        let p50 = percentile(&connection_times, 0.50);
        let p95 = percentile(&connection_times, 0.95);
        let p99 = percentile(&connection_times, 0.99);

        // Aggregated totals
        let total_packets_sent: u64 = metrics.iter().map(|m| m.total_packets_sent).sum();
        let total_packets_received: u64 = metrics.iter().map(|m| m.total_packets_received).sum();
        let total_bytes_sent: u64 = metrics.iter().map(|m| m.total_bytes_sent).sum();
        let total_bytes_received: u64 = metrics.iter().map(|m| m.total_bytes_received).sum();
        let avg_session_duration: u64 =
            metrics.iter().map(|m| m.session_duration_ms).sum::<u64>() / total_clients as u64;
        let total_errors: usize = metrics.iter().map(|m| m.errors.len()).sum();
        let total_producers: u32 = metrics.iter().map(|m| m.producers_created).sum();
        let total_consumers: u32 = metrics.iter().map(|m| m.consumers_created).sum();
        let total_reconnections: u32 = metrics.iter().map(|m| m.reconnections).sum();
        let total_reconnection_failures: u32 =
            metrics.iter().map(|m| m.reconnection_failures).sum();

        // Per-room breakdown
        let rooms = Self::compute_room_summaries(metrics);

        // Aggregate signaling latencies across all clients
        let signaling_latencies = Self::aggregate_latencies(metrics);

        Self {
            total_clients,
            successful_connections,
            failed_connections,
            average_connection_time_ms: avg_connection_time,
            p50_connection_time_ms: p50,
            p95_connection_time_ms: p95,
            p99_connection_time_ms: p99,
            total_packets_sent,
            total_packets_received,
            total_bytes_sent,
            total_bytes_received,
            average_session_duration_ms: avg_session_duration,
            total_errors,
            total_producers_created: total_producers,
            total_consumers_created: total_consumers,
            signaling_latencies,
            rooms,
            total_reconnections,
            total_reconnection_failures,
            connection_attempts: metrics.iter().map(|m| m.connection_attempts.len()).sum(),
            failed_connection_attempts: metrics
                .iter()
                .flat_map(|m| &m.connection_attempts)
                .filter(|a| a.room_join_ms.is_none())
                .count(),
            send_media_ready: samples_stats(
                metrics
                    .iter()
                    .flat_map(|m| &m.connection_attempts)
                    .filter_map(|a| a.send_media_ready_ms),
            ),
            receive_media_ready: samples_stats(
                metrics
                    .iter()
                    .flat_map(|m| &m.connection_attempts)
                    .filter_map(|a| a.receive_media_ready_ms),
            ),
            measurement: MeasurementMetrics {
                duration_ms: metrics[0].measurement.duration_ms,
                packets_queued: metrics.iter().map(|m| m.measurement.packets_queued).sum(),
                packets_received: metrics.iter().map(|m| m.measurement.packets_received).sum(),
                bytes_queued: metrics.iter().map(|m| m.measurement.bytes_queued).sum(),
                bytes_received: metrics.iter().map(|m| m.measurement.bytes_received).sum(),
            },
            validated_consumers: metrics
                .iter()
                .flat_map(|m| &m.consumer_delivery)
                .filter(|c| c.passed)
                .count(),
            failed_consumers: metrics
                .iter()
                .flat_map(|m| &m.consumer_delivery)
                .filter(|c| !c.passed && !c.skipped_short_lived)
                .count(),
            skipped_short_lived_consumers: metrics
                .iter()
                .flat_map(|m| &m.consumer_delivery)
                .filter(|c| c.skipped_short_lived)
                .count(),
            diagnostic_failures: metrics
                .iter()
                .filter_map(|m| m.diagnostics.as_ref())
                .map(|d| d.failures.len())
                .sum(),
        }
    }

    fn compute_room_summaries(metrics: &[ClientMetrics]) -> Vec<RoomSummary> {
        let mut room_map: HashMap<String, Vec<&ClientMetrics>> = HashMap::new();
        for m in metrics {
            room_map.entry(m.room_id.clone()).or_default().push(m);
        }

        let mut rooms: Vec<RoomSummary> = room_map
            .into_iter()
            .map(|(room_id, clients)| RoomSummary {
                room_id,
                total_clients: clients.len(),
                successful_connections: clients.iter().filter(|c| c.connection_successful).count(),
                total_packets_sent: clients.iter().map(|c| c.total_packets_sent).sum(),
                total_packets_received: clients.iter().map(|c| c.total_packets_received).sum(),
                total_errors: clients.iter().map(|c| c.errors.len()).sum(),
                total_producers_created: clients.iter().map(|c| c.producers_created).sum(),
                total_consumers_created: clients.iter().map(|c| c.consumers_created).sum(),
            })
            .collect();

        rooms.sort_by(|a, b| a.room_id.cmp(&b.room_id));
        rooms
    }

    fn aggregate_latencies(metrics: &[ClientMetrics]) -> AggregatedLatencies {
        let mut all_samples: HashMap<String, std::collections::BTreeMap<u64, u64>> = HashMap::new();
        for m in metrics {
            for (op, stats) in &m.signaling_latencies.operations {
                let histogram = all_samples.entry(op.clone()).or_default();
                for (ms, count) in &stats.histogram_ms {
                    *histogram.entry(*ms).or_default() += count;
                }
            }
        }

        let mut operations = HashMap::new();
        for (op, histogram) in all_samples {
            operations.insert(op, histogram_stats(histogram));
        }

        AggregatedLatencies { operations }
    }

    pub fn print_summary(&self) {
        println!("\n=== Load Test Summary ===");
        println!("Total Clients: {}", self.total_clients);
        println!("Successful Connections: {}", self.successful_connections);
        println!("Failed Connections: {}", self.failed_connections);
        println!("\nWebSocket + room admission (per attempt; not ICE/DTLS):");
        println!("  Average: {} ms", self.average_connection_time_ms);
        println!("  P50: {} ms", self.p50_connection_time_ms);
        println!("  P95: {} ms", self.p95_connection_time_ms);
        println!("  P99: {} ms", self.p99_connection_time_ms);
        println!("\nMedia Statistics:");
        println!("  Producers Created: {}", self.total_producers_created);
        println!("  Consumers Created: {}", self.total_consumers_created);
        println!(
            "  Lifetime Packets Queued (not confirmed egress): {}",
            self.total_packets_sent
        );
        println!("  Total Packets Received: {}", self.total_packets_received);
        println!(
            "  Total Bytes Sent: {} ({:.2} MB)",
            self.total_bytes_sent,
            self.total_bytes_sent as f64 / 1_000_000.0
        );
        println!(
            "  Total Bytes Received: {} ({:.2} MB)",
            self.total_bytes_received,
            self.total_bytes_received as f64 / 1_000_000.0
        );
        println!(
            "  Shared measurement: {} ms, {} queued / {} received packets",
            self.measurement.duration_ms,
            self.measurement.packets_queued,
            self.measurement.packets_received
        );
        println!(
            "  Consumer delivery: {} passed, {} failed, {} too short to validate",
            self.validated_consumers, self.failed_consumers, self.skipped_short_lived_consumers
        );

        // Bandwidth (computed from bytes + duration)
        if self.successful_connections > 0 && self.average_session_duration_ms > 0 {
            let bytes_sent_per_client = self.total_bytes_sent / self.successful_connections as u64;
            let bytes_recv_per_client =
                self.total_bytes_received / self.successful_connections as u64;
            let send_bps = bytes_sent_per_client * 8 * 1000 / self.average_session_duration_ms;
            let recv_bps = bytes_recv_per_client * 8 * 1000 / self.average_session_duration_ms;
            let total_send_bps =
                self.total_bytes_sent * 8 * 1000 / self.average_session_duration_ms;
            println!(
                "  Avg Send Bitrate: {:.2} Mbps/client",
                send_bps as f64 / 1_000_000.0
            );
            println!(
                "  Avg Recv Bitrate: {:.2} Mbps/client",
                recv_bps as f64 / 1_000_000.0
            );
            println!(
                "  Total Aggregate Send: {:.2} Gbps",
                total_send_bps as f64 / 1_000_000_000.0
            );
        }

        // Signaling latencies
        if !self.signaling_latencies.operations.is_empty() {
            println!("\nSignaling Latencies (aggregated across clients):");
            let mut ops: Vec<_> = self.signaling_latencies.operations.iter().collect();
            ops.sort_by_key(|(k, _)| (*k).clone());
            for (op, stats) in &ops {
                println!(
                    "  {}: avg={}ms p50={}ms p95={}ms p99={}ms (n={})",
                    op, stats.avg_ms, stats.p50_ms, stats.p95_ms, stats.p99_ms, stats.count
                );
            }
        }

        // Room breakdown (only if more than 1 room)
        if self.rooms.len() > 1 {
            println!("\nPer-Room Breakdown ({} rooms):", self.rooms.len());
            for room in &self.rooms {
                let recv_ratio = if room.total_packets_sent > 0 {
                    format!(
                        "{:.1}%",
                        (room.total_packets_received as f64 / room.total_packets_sent as f64)
                            * 100.0
                    )
                } else {
                    "N/A".to_string()
                };
                println!(
                    "  {}: {} clients, {} producers, {} consumers, recv ratio: {}",
                    room.room_id,
                    room.total_clients,
                    room.total_producers_created,
                    room.total_consumers_created,
                    recv_ratio
                );
            }
        }

        // Churn stats (only if churn occurred)
        if self.total_reconnections > 0 || self.total_reconnection_failures > 0 {
            println!("\nChurn Statistics:");
            println!("  Total Reconnections: {}", self.total_reconnections);
            println!(
                "  Reconnection Failures: {}",
                self.total_reconnection_failures
            );
        }

        println!("\nSession:");
        println!(
            "  Average Duration: {} ms ({:.2} s)",
            self.average_session_duration_ms,
            self.average_session_duration_ms as f64 / 1000.0
        );
        println!("  Total Errors: {}", self.total_errors);
        println!("========================\n");
    }
}

fn percentile(sorted_data: &[u64], p: f64) -> u64 {
    if sorted_data.is_empty() {
        return 0;
    }
    let idx = (p * sorted_data.len() as f64).ceil().max(1.0) as usize - 1;
    sorted_data[idx.min(sorted_data.len() - 1)]
}

fn samples_stats(samples: impl Iterator<Item = u64>) -> LatencyStats {
    let mut histogram = std::collections::BTreeMap::new();
    for ms in samples {
        *histogram.entry(ms).or_default() += 1;
    }
    histogram_stats(histogram)
}

fn histogram_stats(histogram_ms: std::collections::BTreeMap<u64, u64>) -> LatencyStats {
    let count = histogram_ms.values().sum::<u64>();
    if count == 0 {
        return LatencyStats::default();
    }
    let percentile = |fraction: f64| {
        let rank = (fraction * count as f64).ceil().max(1.0) as u64;
        let mut accumulated = 0;
        for (ms, frequency) in &histogram_ms {
            accumulated += frequency;
            if accumulated >= rank {
                return *ms;
            }
        }
        0
    };
    LatencyStats {
        count: count as usize,
        min_ms: *histogram_ms.first_key_value().unwrap().0,
        max_ms: *histogram_ms.last_key_value().unwrap().0,
        avg_ms: (histogram_ms
            .iter()
            .map(|(ms, n)| *ms as u128 * *n as u128)
            .sum::<u128>()
            / count as u128) as u64,
        p50_ms: percentile(0.50),
        p95_ms: percentile(0.95),
        p99_ms: percentile(0.99),
        histogram_ms,
    }
}

#[cfg(test)]
mod measurement_tests {
    use super::*;
    use std::{sync::Arc, time::Duration};

    #[test]
    fn diagnostics_are_opt_in_and_preserve_attempt_and_resume_evidence() {
        let metrics = MetricsCollector::new("diagnostic".into());
        metrics.begin_connection_attempt();
        metrics.diagnostic_event("ignored", serde_json::json!({}));
        assert!(
            serde_json::to_value(metrics.generate_report())
                .unwrap()
                .get("diagnostics")
                .is_none()
        );
        metrics.enable_diagnostics();
        metrics.diagnostic_event("resume-requested", serde_json::json!({"consumerId": "c1"}));
        metrics.diagnostic_event("resume-ack", serde_json::json!({"consumerId": "c1"}));
        metrics.diagnostic_snapshot(serde_json::json!({"transports": []}));
        metrics.begin_connection_attempt();
        metrics.diagnostic_event_for_attempt(
            1,
            "peer-state",
            serde_json::json!({"state": "closed"}),
        );
        metrics.diagnostic_event("resume-requested", serde_json::json!({"consumerId": "c2"}));
        metrics.diagnostic_failure("snapshot unavailable");
        let report = metrics.generate_report();
        let evidence = report.diagnostics.as_ref().unwrap();
        assert_eq!(
            evidence
                .events
                .iter()
                .map(|e| e.attempt)
                .collect::<Vec<_>>(),
            vec![1, 1, 1, 2]
        );
        assert_eq!(evidence.events[1].kind, "resume-ack");
        assert_eq!(evidence.events[1].details["consumerId"], "c1");
        assert_eq!(evidence.snapshots[0].attempt, 1);
        assert_eq!(evidence.failures, vec!["Attempt 2: snapshot unavailable"]);
        assert_eq!(report.errors.len(), 1);
        assert_eq!(TestSummary::from_metrics(&[report]).diagnostic_failures, 1);
    }

    #[test]
    fn diagnostic_capture_limit_fails_instead_of_silently_truncating() {
        let metrics = MetricsCollector::new("bounded".into());
        metrics.begin_connection_attempt();
        metrics.enable_diagnostics();
        for _ in 0..MAX_DIAGNOSTIC_EVENTS + 2 {
            metrics.diagnostic_event("event", serde_json::json!({}));
        }
        let report = metrics.generate_report();
        let evidence = report.diagnostics.unwrap();
        assert_eq!(evidence.events.len(), MAX_DIAGNOSTIC_EVENTS);
        assert_eq!(evidence.failures.len(), 1);
        assert_eq!(report.errors.len(), 1);
    }

    #[test]
    fn aggregate_percentiles_merge_every_operation_not_client_medians() {
        let first = MetricsCollector::new("first".into());
        for ms in [1, 1, 1000] {
            first.record_signaling_latency("consume", ms);
        }
        let second = MetricsCollector::new("second".into());
        second.record_signaling_latency("consume", 2);
        let summary =
            TestSummary::from_metrics(&[first.generate_report(), second.generate_report()]);
        let stats = &summary.signaling_latencies.operations["consume"];
        assert_eq!(stats.count, 4);
        assert_eq!(stats.p50_ms, 1);
        assert_eq!(stats.p99_ms, 1000);
        assert_eq!(stats.histogram_ms[&1], 2);
    }

    #[test]
    fn reconnection_timing_excludes_idle_and_preserves_initial_admission() {
        let metrics = MetricsCollector::new("client".into());
        metrics.begin_connection_attempt();
        *metrics.connection_start.lock().unwrap() = Instant::now() - Duration::from_millis(30);
        metrics.mark_connection_successful();
        let initial = metrics.generate_report().connection_time_ms;
        *metrics.connection_start.lock().unwrap() = Instant::now() - Duration::from_secs(60);
        metrics.begin_connection_attempt();
        metrics.mark_connection_successful();
        metrics.mark_media_ready(true);
        let report = metrics.generate_report();
        assert_eq!(report.connection_time_ms, initial);
        assert_eq!(report.connection_attempts.len(), 2);
        assert!(report.connection_attempts[1].room_join_ms.unwrap() < 1000);
        assert!(report.connection_attempts[1].send_media_ready_ms.unwrap() < 1000);
        assert_eq!(TestSummary::from_metrics(&[report]).connection_attempts, 2);
    }

    #[test]
    fn shared_window_excludes_ramp_and_late_packets() {
        let now = Instant::now();
        let future = Arc::new(MeasurementWindow::new(
            now + Duration::from_secs(10),
            Duration::from_secs(5),
        ));
        let warmup = MetricsCollector::with_window("warmup".into(), future);
        warmup.record_packet_sent(100);
        assert_eq!(warmup.generate_report().measurement.packets_queued, 0);
        let past = Arc::new(MeasurementWindow::new(
            now - Duration::from_secs(10),
            Duration::from_secs(5),
        ));
        let late = MetricsCollector::with_window("late".into(), past);
        late.record_rtp_received(1, 100);
        assert_eq!(late.generate_report().measurement.packets_received, 0);
        let active = Arc::new(MeasurementWindow::new(
            now - Duration::from_secs(1),
            Duration::from_secs(5),
        ));
        let steady = MetricsCollector::with_window("steady".into(), active);
        steady.record_packet_sent(100);
        steady.record_rtp_received(1, 80);
        assert_eq!(steady.generate_report().measurement.packets_queued, 1);
        assert_eq!(steady.generate_report().measurement.bytes_received, 80);
    }

    #[test]
    fn delivery_detects_stalls_but_excludes_intentional_publisher_churn() {
        let start = Instant::now() - Duration::from_secs(10);
        let window = Arc::new(MeasurementWindow::new(start, Duration::from_secs(10)));
        let metrics = MetricsCollector::with_window("viewer".into(), window.clone());
        metrics.record_consumer("consumer", "producer", 123);
        {
            let mut consumers = metrics.consumers.lock().unwrap();
            consumers[0].created = start - Duration::from_secs(3);
            consumers[0].packets_by_second = vec![10, 10, 10, 10, 0, 0, 0, 0, 0, 0];
        }
        let before = metrics.generate_report();
        assert!(!before.consumer_delivery[0].passed);
        assert_eq!(before.consumer_delivery[0].longest_gap_seconds, 6);
        window.publishers.lock().unwrap().insert(
            "producer".into(),
            (
                start - Duration::from_secs(3),
                Some(start + Duration::from_secs(4)),
            ),
        );
        let after = metrics.generate_report();
        assert!(after.consumer_delivery[0].passed);
        assert_eq!(after.consumer_delivery[0].eligible_seconds, 4);
    }

    #[test]
    fn empty_and_short_lived_delivery_is_not_a_success() {
        assert_eq!(delivery_coverage(&[0, 0, 0, 0]), (0, 4));
        let metrics = MetricsCollector::new("client".into());
        metrics.record_consumer("consumer", "producer", 1);
        metrics.end_session();
        let delivery = &metrics.generate_report().consumer_delivery[0];
        assert!(delivery.skipped_short_lived);
        assert!(!delivery.passed);
    }

    #[test]
    fn unexpected_server_closure_does_not_excuse_an_active_publishers_missing_tail() {
        let start = Instant::now() - Duration::from_secs(10);
        let window = Arc::new(MeasurementWindow::new(start, Duration::from_secs(10)));
        let viewer = MetricsCollector::with_window("viewer".into(), window.clone());
        window
            .publishers
            .lock()
            .unwrap()
            .insert("producer".into(), (start - Duration::from_secs(3), None));
        viewer.subscribe("producer", true);
        viewer.record_consumer("consumer", "producer", 123);
        {
            let mut consumers = viewer.consumers.lock().unwrap();
            consumers[0].created = start - Duration::from_secs(3);
            consumers[0].packets_by_second = vec![10, 10, 10, 10, 0, 0, 0, 0, 0, 0];
        }
        let (kind, unexpected) = viewer.close_producer("producer");
        assert_eq!(kind, Some(true));
        assert!(unexpected);
        let delivery = &viewer.generate_report().consumer_delivery[0];
        assert_eq!(delivery.eligible_seconds, 10);
        assert!(!delivery.passed);
    }

    #[test]
    fn planned_lifetime_is_published_before_other_clients_observe_closure() {
        let window = Arc::new(MeasurementWindow::new(
            Instant::now(),
            Duration::from_secs(10),
        ));
        let publisher = MetricsCollector::with_window("publisher".into(), window.clone());
        let viewer = MetricsCollector::with_window("viewer".into(), window);
        publisher.record_publisher("producer");
        viewer.subscribe("producer", false);
        viewer.record_consumer("consumer", "producer", 123);
        publisher.end_session();
        let (kind, unexpected) = viewer.close_producer("producer");
        assert_eq!(kind, Some(false));
        assert!(!unexpected);
    }
}
