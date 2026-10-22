//! Serialized generator evidence shared with offline readers and benchmarks.
//!
//! These types hold observations only. Importing them starts no collector,
//! runtime, native resource or workload. Typed deserialization does not prove
//! complete measurement coverage or successful delivery; readers retain their
//! own bounds, workload identity checks and verdict policy.

use serde::{Deserialize, Serialize};

/// Latency statistics for a single operation type.
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

/// Counts for the shared steady-state measurement interval. Queued packets are
/// local writer observations, distinct from independently received packets.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MeasurementMetrics {
    pub duration_ms: u64,
    pub packets_queued: u64,
    pub packets_received: u64,
    pub bytes_queued: u64,
    pub bytes_received: u64,
}

/// One observed directed subscription, retaining immutable native identities.
/// Presence alone does not establish the target graph or sustained delivery.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedSubscriptionEdge {
    pub publisher_client_id: String,
    pub producer_id: String,
    pub consumer_id: String,
    pub is_audio: bool,
}

/// Completion metadata surrounding a generator summary. The generator supplies
/// its actual configuration and provenance; offline readers may project those
/// fields into narrower types without importing runtime collectors.
///
/// Writers convert this to a JSON value before inserting it into a summary,
/// preserving the existing map serialization and pretty-printed field order.
/// No fields acquire deserialization defaults or new omission rules here.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunMetadata<C, P> {
    pub completed: bool,
    pub passed: bool,
    pub failure_reasons: Vec<String>,
    pub started_at: String,
    pub finished_at: String,
    pub configuration: C,
    pub provenance: P,
}

#[cfg(test)]
#[path = "evidence_tests.rs"]
mod tests;
