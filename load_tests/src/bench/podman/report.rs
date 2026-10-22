//! Projection of admitted workload and measured resource evidence.

use super::{
    attribution,
    options::{Options, Workload},
    sampler::Data,
};
use crate::bench::{
    resources::{self, Role},
    subscription,
    summary::Summary,
    verdict,
};
use anyhow::Result;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) struct Evidence<'a> {
    pub(super) options: &'a Options,
    pub(super) scenario: &'a Workload,
    pub(super) repetition: u32,
    pub(super) started_at: &'a str,
    pub(super) ticks: f64,
    pub(super) summary: &'a Summary,
    pub(super) subscription: subscription::Identity,
    pub(super) finish: &'a BTreeMap<String, f64>,
    pub(super) cleanup_ms: f64,
    pub(super) udp: Value,
}

pub(super) fn row(data: &Data, evidence: Evidence<'_>) -> Result<Value> {
    let Evidence {
        options,
        scenario,
        repetition,
        started_at,
        ticks,
        summary,
        subscription,
        finish,
        cleanup_ms,
        udp,
    } = evidence;
    let start = (options.ramp_up + options.warmup) as f64 * 1000.0;
    let end = start + options.duration as f64 * 1000.0;
    let (server, generator) = data.resource_samples();
    let server = resources::summarize(&server, Role::Server, start, end)?;
    let generator = resources::summarize(&generator, Role::Generator, start, end)?;
    let attribution = attribution::summarize(&data.server, start, end, ticks, options.cpus)?;
    let metrics: BTreeMap<_, _> = finish
        .iter()
        .filter(|(key, _)| {
            [
                "rejected",
                "deaths",
                "queue_full",
                "queue_closed",
                "errors_total",
                "consumers_created",
            ]
            .iter()
            .any(|part| key.contains(part))
        })
        .collect();
    let mut result = json!({
        "purpose": "production-shape", "scenario":scenario.name,
        "variant":"candidate", "repetition":repetition, "startedAt":started_at,
        "netem":options.netem, "netemScope":options.netem.as_ref().map(|_|&options.netem_scope),
        "workloadPassed":true, "joinP99Ms":summary.p99_connection_time_ms,
        "sendReadyP99Ms":summary.send_media_ready.p99_ms,
        "receiveReadyP99Ms":summary.receive_media_ready.p99_ms,
        "receivedPacketsPerSecond":summary.measurement.packets_received as f64
            / (summary.measurement.duration_ms as f64 / 1000.0),
        "keyframesGenerated":summary.keyframes_generated,
        "keyframesRequested":summary.keyframes_requested,
        "bandwidthEstimates":summary.bandwidth_estimates,
        "clientsWithBandwidthEstimate":summary.clients_with_bandwidth_estimate,
        "serverCpuPercent":server.cpu_percent_of_one_core,
        "serverPeakRssMiB":server.peak_rss_mi_b,
        "serverCpuPercentOfQuota":attribution.cgroup.cpu_percent_of_quota,
        "serverThrottledPeriodFraction":attribution.cgroup.throttled_period_fraction,
        "serverThrottledSeconds":attribution.cgroup.throttled_seconds,
        "serverThreadCpuSeconds":attribution.thread_cpu_seconds,
        "generatorCpuPercent":generator.cpu_percent_of_one_core,
        "generatorPeakRssMiB":generator.peak_rss_mi_b,
        "cleanupMs":cleanup_ms, "serverResources":server, "generatorResources":generator,
        "serverAttribution":attribution,
        "media":{"validatedConsumers":summary.validated_consumers,
            "failedConsumers":summary.failed_consumers,
            "skippedShortLivedConsumers":summary.skipped_short_lived_consumers},
        "serverUdp":udp, "serverMetricsAtFinish":metrics,
    });
    result.as_object_mut().unwrap().extend(
        serde_json::to_value(subscription)?
            .as_object()
            .unwrap()
            .clone(),
    );
    Ok(result)
}

pub(super) fn completed(row: &mut Value, at: &str) -> Result<()> {
    let status = verdict::performance(std::slice::from_ref(row));
    row.as_object_mut()
        .unwrap()
        .extend(serde_json::to_value(status)?.as_object().unwrap().clone());
    row["completedAt"] = at.into();
    Ok(())
}

pub(super) fn comparison(rows: &[Value]) -> Result<Value> {
    let fields = [
        "receiveReadyP99Ms",
        "sendReadyP99Ms",
        "receivedPacketsPerSecond",
        "serverCpuPercentOfQuota",
        "serverThrottledPeriodFraction",
        "serverPeakRssMiB",
        "generatorCpuPercent",
    ];
    let mut capacity = serde_json::Map::new();
    for row in rows {
        let name = row["scenario"].as_str().unwrap_or("");
        if capacity.contains_key(name) {
            continue;
        }
        let mut values = serde_json::Map::new();
        for field in fields {
            let samples: Vec<_> = rows
                .iter()
                .filter(|r| r["scenario"] == name)
                .map(|r| r[field].as_f64().unwrap_or(0.0))
                .collect();
            values.insert(
                field.into(),
                json!({"median":resources::median(&samples),
                "range":[samples.iter().copied().fold(f64::INFINITY,f64::min),
                    samples.iter().copied().fold(f64::NEG_INFINITY,f64::max)]}),
            );
        }
        capacity.insert(name.into(), values.into());
    }
    let status = verdict::performance(rows);
    let mut result = json!({"purpose":"production-shape", "completed":true,
        "rows":rows, "capacity":capacity});
    result
        .as_object_mut()
        .unwrap()
        .extend(serde_json::to_value(status)?.as_object().unwrap().clone());
    Ok(result)
}
