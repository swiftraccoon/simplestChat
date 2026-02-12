use std::sync::atomic::{AtomicU64, AtomicBool, Ordering};
use std::time::Instant;
use std::collections::HashMap;
use serde::{Deserialize, Serialize};

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
}

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
}

/// Real-time metrics collector (thread-safe)
pub struct MetricsCollector {
    client_id: String,
    room_id: std::sync::Mutex<String>,
    start_time: Instant,
    connection_start: Instant,
    connection_successful: AtomicBool,
    connection_time_ms: AtomicU64,
    first_media_sent: AtomicU64, // 0 = not set
    first_media_received: AtomicU64, // 0 = not set
    packets_sent: AtomicU64,
    packets_received: AtomicU64,
    bytes_sent: AtomicU64,
    bytes_received: AtomicU64,
    producers_created: AtomicU64,
    consumers_created: AtomicU64,
    errors: std::sync::Mutex<Vec<String>>,
    signaling_latencies: std::sync::Mutex<HashMap<String, Vec<u64>>>,
    reconnections: AtomicU64,
    reconnection_failures: AtomicU64,
}

impl MetricsCollector {
    pub fn new(client_id: String) -> Self {
        let now = Instant::now();
        Self {
            client_id,
            room_id: std::sync::Mutex::new(String::new()),
            start_time: now,
            connection_start: now,
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
        }
    }

    pub fn set_room_id(&self, room_id: &str) {
        if let Ok(mut r) = self.room_id.lock() {
            *r = room_id.to_string();
        }
    }

    pub fn mark_connection_successful(&self) {
        self.connection_successful.store(true, Ordering::SeqCst);
        let elapsed = self.connection_start.elapsed().as_millis() as u64;
        self.connection_time_ms.store(elapsed, Ordering::SeqCst);
    }

    pub fn mark_first_media_sent(&self) {
        let current = self.first_media_sent.load(Ordering::SeqCst);
        if current == 0 {
            let elapsed = self.start_time.elapsed().as_millis() as u64;
            self.first_media_sent.store(elapsed, Ordering::SeqCst);
        }
    }

    pub fn mark_first_media_received(&self) {
        let current = self.first_media_received.load(Ordering::SeqCst);
        if current == 0 {
            let elapsed = self.start_time.elapsed().as_millis() as u64;
            self.first_media_received.store(elapsed, Ordering::SeqCst);
        }
    }

    pub fn record_packet_sent(&self, size: usize) {
        self.packets_sent.fetch_add(1, Ordering::Relaxed);
        self.bytes_sent.fetch_add(size as u64, Ordering::Relaxed);
    }

    pub fn record_packet_received(&self, size: usize) {
        self.packets_received.fetch_add(1, Ordering::Relaxed);
        self.bytes_received.fetch_add(size as u64, Ordering::Relaxed);
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
            latencies.entry(operation.to_string()).or_default().push(ms);
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
            time_to_first_media_sent_ms: if first_sent > 0 { Some(first_sent) } else { None },
            time_to_first_media_received_ms: if first_received > 0 { Some(first_received) } else { None },
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
        }
    }

    fn compute_latency_report(&self) -> SignalingLatencyReport {
        let latencies = self.signaling_latencies.lock()
            .map(|l| l.clone())
            .unwrap_or_default();

        let mut operations = HashMap::new();
        for (op, mut samples) in latencies {
            if samples.is_empty() {
                continue;
            }
            samples.sort_unstable();
            let count = samples.len();
            let min_ms = samples[0];
            let max_ms = *samples.last().unwrap();
            let avg_ms = samples.iter().sum::<u64>() / count as u64;
            let p50_ms = percentile(&samples, 0.50);
            let p95_ms = percentile(&samples, 0.95);
            let p99_ms = percentile(&samples, 0.99);
            operations.insert(op, LatencyStats { count, min_ms, max_ms, avg_ms, p50_ms, p95_ms, p99_ms });
        }

        SignalingLatencyReport { operations }
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
            .filter(|m| m.connection_successful)
            .map(|m| m.connection_time_ms)
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
        let avg_session_duration: u64 = metrics.iter().map(|m| m.session_duration_ms).sum::<u64>() / total_clients as u64;
        let total_errors: usize = metrics.iter().map(|m| m.errors.len()).sum();
        let total_producers: u32 = metrics.iter().map(|m| m.producers_created).sum();
        let total_consumers: u32 = metrics.iter().map(|m| m.consumers_created).sum();
        let total_reconnections: u32 = metrics.iter().map(|m| m.reconnections).sum();
        let total_reconnection_failures: u32 = metrics.iter().map(|m| m.reconnection_failures).sum();

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
        }
    }

    fn compute_room_summaries(metrics: &[ClientMetrics]) -> Vec<RoomSummary> {
        let mut room_map: HashMap<String, Vec<&ClientMetrics>> = HashMap::new();
        for m in metrics {
            room_map.entry(m.room_id.clone()).or_default().push(m);
        }

        let mut rooms: Vec<RoomSummary> = room_map.into_iter().map(|(room_id, clients)| {
            RoomSummary {
                room_id,
                total_clients: clients.len(),
                successful_connections: clients.iter().filter(|c| c.connection_successful).count(),
                total_packets_sent: clients.iter().map(|c| c.total_packets_sent).sum(),
                total_packets_received: clients.iter().map(|c| c.total_packets_received).sum(),
                total_errors: clients.iter().map(|c| c.errors.len()).sum(),
                total_producers_created: clients.iter().map(|c| c.producers_created).sum(),
                total_consumers_created: clients.iter().map(|c| c.consumers_created).sum(),
            }
        }).collect();

        rooms.sort_by(|a, b| a.room_id.cmp(&b.room_id));
        rooms
    }

    fn aggregate_latencies(metrics: &[ClientMetrics]) -> AggregatedLatencies {
        let mut all_samples: HashMap<String, Vec<u64>> = HashMap::new();
        for m in metrics {
            for (op, stats) in &m.signaling_latencies.operations {
                // We only have stats per client, not raw samples. Approximate by
                // using p50 as a representative sample per client (good enough for
                // aggregated P50/P95/P99 across many clients).
                all_samples.entry(op.clone()).or_default().push(stats.p50_ms);
            }
        }

        let mut operations = HashMap::new();
        for (op, mut samples) in all_samples {
            if samples.is_empty() {
                continue;
            }
            samples.sort_unstable();
            let count = samples.len();
            let min_ms = samples[0];
            let max_ms = *samples.last().unwrap();
            let avg_ms = samples.iter().sum::<u64>() / count as u64;
            let p50_ms = percentile(&samples, 0.50);
            let p95_ms = percentile(&samples, 0.95);
            let p99_ms = percentile(&samples, 0.99);
            operations.insert(op, LatencyStats { count, min_ms, max_ms, avg_ms, p50_ms, p95_ms, p99_ms });
        }

        AggregatedLatencies { operations }
    }

    pub fn print_summary(&self) {
        println!("\n=== Load Test Summary ===");
        println!("Total Clients: {}", self.total_clients);
        println!("Successful Connections: {}", self.successful_connections);
        println!("Failed Connections: {}", self.failed_connections);
        println!("\nConnection Time:");
        println!("  Average: {} ms", self.average_connection_time_ms);
        println!("  P50: {} ms", self.p50_connection_time_ms);
        println!("  P95: {} ms", self.p95_connection_time_ms);
        println!("  P99: {} ms", self.p99_connection_time_ms);
        println!("\nMedia Statistics:");
        println!("  Producers Created: {}", self.total_producers_created);
        println!("  Consumers Created: {}", self.total_consumers_created);
        println!("  Total Packets Sent: {}", self.total_packets_sent);
        println!("  Total Packets Received: {}", self.total_packets_received);
        println!("  Total Bytes Sent: {} ({:.2} MB)", self.total_bytes_sent, self.total_bytes_sent as f64 / 1_000_000.0);
        println!("  Total Bytes Received: {} ({:.2} MB)", self.total_bytes_received, self.total_bytes_received as f64 / 1_000_000.0);

        // Signaling latencies
        if !self.signaling_latencies.operations.is_empty() {
            println!("\nSignaling Latencies (aggregated across clients):");
            let mut ops: Vec<_> = self.signaling_latencies.operations.iter().collect();
            ops.sort_by_key(|(k, _)| (*k).clone());
            for (op, stats) in &ops {
                println!("  {}: avg={}ms p50={}ms p95={}ms p99={}ms (n={})",
                    op, stats.avg_ms, stats.p50_ms, stats.p95_ms, stats.p99_ms, stats.count);
            }
        }

        // Room breakdown (only if more than 1 room)
        if self.rooms.len() > 1 {
            println!("\nPer-Room Breakdown ({} rooms):", self.rooms.len());
            for room in &self.rooms {
                let recv_ratio = if room.total_packets_sent > 0 {
                    format!("{:.1}%", (room.total_packets_received as f64 / room.total_packets_sent as f64) * 100.0)
                } else {
                    "N/A".to_string()
                };
                println!("  {}: {} clients, {} producers, {} consumers, recv ratio: {}",
                    room.room_id, room.total_clients, room.total_producers_created,
                    room.total_consumers_created, recv_ratio);
            }
        }

        // Churn stats (only if churn occurred)
        if self.total_reconnections > 0 || self.total_reconnection_failures > 0 {
            println!("\nChurn Statistics:");
            println!("  Total Reconnections: {}", self.total_reconnections);
            println!("  Reconnection Failures: {}", self.total_reconnection_failures);
        }

        println!("\nSession:");
        println!("  Average Duration: {} ms ({:.2} s)", self.average_session_duration_ms, self.average_session_duration_ms as f64 / 1000.0);
        println!("  Total Errors: {}", self.total_errors);
        println!("========================\n");
    }
}

fn percentile(sorted_data: &[u64], p: f64) -> u64 {
    if sorted_data.is_empty() {
        return 0;
    }
    let idx = (p * (sorted_data.len() - 1) as f64).round() as usize;
    sorted_data[idx.min(sorted_data.len() - 1)]
}
