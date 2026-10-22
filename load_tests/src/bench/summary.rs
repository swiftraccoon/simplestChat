//! Selected typed generator evidence. Native IDs and unrelated private fields
//! remain in the owned input artifact; they never become a benchmark summary.

use super::options::{Departure, Options, Plan};
use crate::{LatencyStats, MeasurementMetrics, RunMetadata};
use anyhow::Result;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
    pub schema_version: u32,
    pub run: RunMetadata<Value, Value>,
    pub total_errors: u64,
    #[serde(default)]
    pub successful_connections: u64,
    #[serde(default)]
    pub total_packets_received: u64,
    pub failed_connections: u64,
    pub failed_consumers: u64,
    pub validated_consumers: u64,
    pub skipped_short_lived_consumers: u64,
    pub p99_connection_time_ms: u64,
    pub send_media_ready: LatencyStats,
    pub receive_media_ready: LatencyStats,
    pub measurement: MeasurementMetrics,
    #[serde(default)]
    pub diagnostic_failures: Option<u64>,
    #[serde(default)]
    pub keyframes_generated: u64,
    #[serde(default)]
    pub keyframes_requested: u64,
    #[serde(default)]
    pub bandwidth_estimates: u64,
    #[serde(default)]
    pub clients_with_bandwidth_estimate: u64,
}

impl Summary {
    /// The impairment controller retains completed delivery failures as
    /// evidence. Its eight public counters reuse this selected summary parser;
    /// legacy comparison summaries may omit counters through serde defaults.
    pub fn impaired_projection(&self, original: &Value) -> Result<Value> {
        for (name, expected) in [
            ("successfulConnections", self.successful_connections),
            ("failedConnections", self.failed_connections),
            ("validatedConsumers", self.validated_consumers),
            ("failedConsumers", self.failed_consumers),
            ("keyframesRequested", self.keyframes_requested),
            ("bandwidthEstimates", self.bandwidth_estimates),
            ("totalPacketsReceived", self.total_packets_received),
            ("totalErrors", self.total_errors),
        ] {
            anyhow::ensure!(
                original.get(name).and_then(Value::as_u64) == Some(expected),
                "Incomplete impaired generator counters"
            );
        }
        Ok(json!({"successfulConnections":self.successful_connections,
            "failedConnections":self.failed_connections,
            "validatedConsumers":self.validated_consumers,"failedConsumers":self.failed_consumers,
            "keyframesRequested":self.keyframes_requested,
            "bandwidthEstimates":self.bandwidth_estimates,
            "totalPacketsReceived":self.total_packets_received,"totalErrors":self.total_errors}))
    }

    /// Coverage and owned process exits are checked separately by the caller.
    pub fn workload_passed(&self) -> bool {
        self.schema_version == 2
            && self.run.completed
            && self.run.passed
            && self.total_errors == 0
            && self.failed_connections == 0
            && self.failed_consumers == 0
    }
}

pub fn parse(value: &Value) -> Result<Summary> {
    serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("Incomplete generator summary"))
}

pub fn read(path: &Path) -> Result<(Summary, Value)> {
    let value = super::artifacts::read_json(path, 8 * 1024 * 1024)?;
    Ok((parse(&value)?, value))
}

fn numeric(value: &Value, expected: u32) -> bool {
    value.as_f64() == Some(expected as f64)
}

fn departure(departure: Departure) -> &'static str {
    match departure {
        Departure::Abrupt => "abrupt",
        Departure::ExplicitLeave => "explicit-leave",
    }
}

/// Completed failing runs still carry nominal boundaries. Match the actual
/// requested workload and executable before admitting those boundaries.
pub fn lifecycle_workload(summary: &Value, options: &Options, hash: &str) -> Option<Value> {
    let run = &summary["run"];
    let config = &run["configuration"];
    let mode = if config["subscriptionPlan"].is_null() {
        Some("fifo")
    } else {
        config["subscriptionPlan"].as_str()
    };
    let seed_matches = options.subscription_seed.map_or_else(
        || config["subscriptionSeed"].is_null(),
        |seed| config["subscriptionSeed"].as_f64() == Some(seed as f64),
    );
    if summary["schemaVersion"].as_f64() != Some(2.0)
        || run["completed"] != true
        || config["diagnostics"] != true
        || run["provenance"]["generatorBinarySha256"] != hash
        || config["departure"] != departure(options.departure)
        || mode != Some(options.subscription_plan.name())
        || !seed_matches
        || !numeric(&config["rampUpSecs"], options.ramp_up)
        || !numeric(&config["warmupSecs"], options.warmup)
        || !numeric(&config["durationSecs"], options.duration)
    {
        return None;
    }
    // Preserve omitted timestamp keys for malformed legacy input so the
    // lifecycle reader, rather than substituted anchors, owns their refusal.
    let mut result = json!({"rampUpSecs":config["rampUpSecs"],
        "warmupSecs":config["warmupSecs"],"durationSecs":config["durationSecs"]});
    for key in ["startedAt", "finishedAt"] {
        if let Some(value) = run.get(key) {
            result[key] = value.clone();
        }
    }
    Some(result)
}

/// A timeout/foreign run cannot be read back into diagnostic evidence. Positive
/// diagnostic failure counts retain useful failure evidence without passing it.
pub fn admits_diagnostics(summary: &Value, options: &Options, hash: &str, timed_out: bool) -> bool {
    options.full_diagnostics()
        && !timed_out
        && hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        && lifecycle_workload(summary, options, hash).is_some()
        && summary["diagnosticFailures"].as_f64().is_some_and(|value| {
            value.is_finite() && value.fract() == 0.0 && (0.0..=409600.0).contains(&value)
        })
}

/// JSON null is the historical FIFO marker; a non-null seed is never silently
/// accepted for the discovery workload.
pub fn graph_matches(summary: &Summary, options: &Options) -> bool {
    let config = &summary.run.configuration;
    let mode = if config["subscriptionPlan"].is_null() {
        Some("fifo")
    } else {
        config["subscriptionPlan"].as_str()
    };
    mode == Some(options.subscription_plan.name())
        && (options.subscription_plan != Plan::Fifo || config["subscriptionSeed"].is_null())
}
