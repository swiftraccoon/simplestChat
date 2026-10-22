//! Pure policy for the fixed thirty-account experiment. Inputs are bounded,
//! stopped, finite JSON observations supplied by the invocation owner. This
//! module starts no process and reads no file. It projects selected native
//! fields only; errors contain fixed codes rather than observed private data.

use crate::bench::{process::Exit, resources};
use anyhow::{Result, ensure};
use regex::Regex;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

pub(super) const CLIENTS: usize = 30;
pub(super) const DURATION: usize = 1200;
pub(super) const RAMP: usize = 60;
pub(super) const WARMUP: usize = 30;
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// Only locally selected failure codes can enter the public assessment. A
/// dependency or caller error's display text is never accepted as evidence.
pub(super) fn failure_code(error: &anyhow::Error) -> &'static str {
    match error.to_string().as_str() {
        "invalid_failure_reasons" => "invalid_failure_reasons",
        "incomplete_server_metrics" => "incomplete_server_metrics",
        "invalid_host_cpu_sample" => "invalid_host_cpu_sample",
        "invalid_host_memory_sample" => "invalid_host_memory_sample",
        "missing_database_query_timing" => "missing_database_query_timing",
        "invalid_database_probe" => "invalid_database_probe",
        "invalid_cgroup_cpu_sample" => "invalid_cgroup_cpu_sample",
        "invalid_generator_chat" => "invalid_generator_chat",
        "invalid_signaling_latencies" => "invalid_signaling_latencies",
        "invalid_generator_counters" => "invalid_generator_counters",
        "incomplete_authentication_cohort" => "incomplete_authentication_cohort",
        "duplicate_authentication_account" => "duplicate_authentication_account",
        "authentication_refresh_or_expiry_failed" => "authentication_refresh_or_expiry_failed",
        "retained_inventory_missing" => "retained_inventory_missing",
        "retained_message_count_mismatch" => "retained_message_count_mismatch",
        "unexpected_retained_message" => "unexpected_retained_message",
        "duplicate_expected_message_id" => "duplicate_expected_message_id",
        "retained_message_content_mismatch" => "retained_message_content_mismatch",
        "incomplete_retained_sender_cohort" => "incomplete_retained_sender_cohort",
        "invalid_measurement_window" => "invalid_measurement_window",
        "invalid_resource_observation" => "invalid_resource_observation",
        "unstable_or_missing_server_process_tree" => "unstable_or_missing_server_process_tree",
        "incomplete_server_resource_coverage" | "incomplete_generator_resource_coverage" => {
            // Reference resourceSummary errors are descriptive text; assess
            // deliberately reduces them to this fixed public category.
            "evidence_validation_failed"
        }
        "incomplete_host_resource_coverage" => "incomplete_host_resource_coverage",
        "invalid_host_cpu_window" => "invalid_host_cpu_window",
        "invalid_host_cpu_counter_delta" => "invalid_host_cpu_counter_delta",
        "incomplete_database_metrics_coverage" => "incomplete_database_metrics_coverage",
        "resource_sampling_stalled" => "resource_sampling_stalled",
        "missing_cgroup_throttling_evidence" => "missing_cgroup_throttling_evidence",
        "invalid_cgroup_throttling_window" => "invalid_cgroup_throttling_window",
        "missing_worker_cpu_evidence" => "missing_worker_cpu_evidence",
        "missing_application_pool_evidence" => "missing_application_pool_evidence",
        "incomplete_live_membership_evidence" => "incomplete_live_membership_evidence",
        _ => "invalid_policy_observation",
    }
}

fn numeric(value: &Value) -> Option<f64> {
    value.as_f64().filter(|value| value.is_finite())
}
fn integer(value: &Value) -> Option<f64> {
    numeric(value).filter(|value| value.fract() == 0.0 && (0.0..=SAFE_INTEGER).contains(value))
}
fn number(value: f64) -> Value {
    // Retain JavaScript's decimal spelling and negative-zero normalization.
    serde_json::from_str(ryu_js::Buffer::new().format(value)).unwrap_or(Value::Null)
}
fn matching(value: &Value, expected: f64) -> bool {
    numeric(value) == Some(expected)
}
fn failures(value: Option<&Value>) -> Result<Value> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(json!([]));
    };
    let values = value
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid_failure_reasons"))?;
    let mut result = Vec::new();
    for value in values.iter().take(100) {
        let text = value
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid_failure_reasons"))?;
        // A native Rust string cannot represent the lone surrogate that a JS
        // UTF-16 slice can create. Refuse that boundary instead of replacing it.
        let prefix: Vec<_> = text.encode_utf16().take(400).collect();
        let prefix =
            String::from_utf16(&prefix).map_err(|_| anyhow::anyhow!("invalid_failure_reasons"))?;
        result.push(Value::String(prefix));
    }
    Ok(Value::Array(result))
}

/// Parse only finite named application metrics. Duplicate observations retain
/// the last value, as in the reference. Unmatched exposition lines are ignored.
pub(super) fn parse_metrics(raw: &str) -> Result<Map<String, Value>> {
    static METRIC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^(simplestchat_[a-zA-Z0-9_]+(?:\{[^\n]*\})?) ([0-9.eE+-]+)$")
            .expect("literal metrics expression")
    });
    let mut values = Map::new();
    for line in raw.split('\n') {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(captures) = METRIC.captures(line)
            && let Ok(value) = captures[2].parse::<f64>()
            && value.is_finite()
        {
            values.insert(captures[1].to_owned(), number(value));
        }
    }
    ensure!(
        values
            .get("simplestchat_connections_active")
            .and_then(numeric)
            .is_some(),
        "incomplete_server_metrics"
    );
    Ok(values)
}

/// Host total excludes guest/guest-nice: their CPU time is already counted in
/// the first eight Linux counters. Available memory is measured, never zeroed.
pub(super) fn parse_host(stat: &str, memory: &str) -> Result<Value> {
    let cpu: Vec<_> = stat
        .split('\n')
        .find(|line| line.starts_with("cpu "))
        .unwrap_or("")
        .split_whitespace()
        .skip(1)
        .map(str::parse::<f64>)
        .collect();
    ensure!(
        cpu.len() >= 8
            && cpu
                .iter()
                .all(|value| value.as_ref().is_ok_and(|v| v.is_finite())),
        "invalid_host_cpu_sample"
    );
    let cpu: Vec<_> = cpu.into_iter().map(|value| value.unwrap()).collect();
    static AVAILABLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m)^MemAvailable:\s+([0-9]+) kB$").expect("literal memory expression")
    });
    let available = AVAILABLE
        .captures(memory)
        .and_then(|captures| captures[1].parse::<f64>().ok())
        .map(|value| value * 1024.0)
        .filter(|value| value.is_finite() && *value > 0.0)
        .ok_or_else(|| anyhow::anyhow!("invalid_host_memory_sample"))?;
    Ok(json!({"total": number(cpu[..8].iter().sum()),
        "idle": number(cpu[3] + cpu[4]), "steal": number(cpu[7]),
        "availableBytes": number(available)}))
}

/// Parse the separate libpq query timing after connection. No command stderr,
/// connection URI or credential becomes a diagnostic or result field.
pub(super) fn parse_database_probe(raw: &str) -> Result<Value> {
    static TIMING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^Time: ([0-9.]+) ms(?: .*)?$").expect("literal timing expression")
    });
    let lines: Vec<_> = raw.trim().split('\n').collect();
    let elapsed = lines
        .last()
        .and_then(|line| TIMING.captures(line))
        .and_then(|captures| captures[1].parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .ok_or_else(|| anyhow::anyhow!("missing_database_query_timing"))?;
    let counters: Value = serde_json::from_str(&lines[..lines.len() - 1].join("\n"))
        .map_err(|_| anyhow::anyhow!("invalid_database_probe"))?;
    ensure!(
        ["connections", "active", "waiting"]
            .iter()
            .all(|key| integer(&counters[key]).is_some()),
        "invalid_database_probe"
    );
    Ok(
        json!({"connections":counters["connections"],"active":counters["active"],
        "waiting":counters["waiting"],"queryRoundTripMs":number(elapsed)}),
    )
}

pub(super) fn parse_cgroup(stat: &str, maximum: &str) -> Result<Value> {
    let mut counters = BTreeMap::new();
    for line in stat.trim().split('\n') {
        let mut fields = line.split_whitespace();
        if let Some(key) = fields.next() {
            counters.insert(
                key,
                fields.next().and_then(|value| value.parse::<f64>().ok()),
            );
        }
    }
    let counter = |key| {
        counters
            .get(key)
            .copied()
            .flatten()
            .filter(|value| value.is_finite())
    };
    let mut fields = maximum.split_whitespace();
    let quota = fields.next().unwrap_or("");
    let period = fields.next().and_then(|value| value.parse::<f64>().ok());
    let numeric_quota = quota.parse::<f64>().ok();
    ensure!(
        counter("usage_usec").is_some()
            && counter("nr_periods").is_some()
            && counter("nr_throttled").is_some()
            && period.is_some_and(|value| value.is_finite() && value > 0.0)
            && (quota == "max"
                || numeric_quota.is_some_and(|value| value.is_finite() && value > 0.0)),
        "invalid_cgroup_cpu_sample"
    );
    Ok(json!({"periods":number(counter("nr_periods").unwrap()),
        "throttledPeriods":number(counter("nr_throttled").unwrap()),
        "quotaCores":if quota == "max" {Value::Null}
            else {number(numeric_quota.unwrap()/period.unwrap())}}))
}

fn selected_chat(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let source = value
        .as_object()
        .ok_or_else(|| anyhow::anyhow!("invalid_generator_chat"))?;
    let mut output = Map::new();
    for (key, value) in source {
        match key.as_str() {
            "passed" => {
                ensure!(value.is_boolean(), "invalid_generator_chat");
                output.insert(key.clone(), value.clone());
            }
            "intervalMs"
            | "contentBytes"
            | "drainMs"
            | "clientsRegistered"
            | "expectedMessages"
            | "sentMessages"
            | "acknowledgements"
            | "expectedDeliveries"
            | "receivedDeliveries"
            | "invalidObservations" => {
                ensure!(integer(value).is_some(), "invalid_generator_chat");
                output.insert(key.clone(), value.clone());
            }
            "failureReasons" => {
                ensure!(
                    value
                        .as_array()
                        .is_some_and(|rows| rows.iter().all(Value::is_string)),
                    "invalid_generator_chat"
                );
                output.insert(key.clone(), value.clone());
            }
            _ => {}
        }
    }
    Ok(Value::Object(output))
}

fn selected_latencies(value: &Value) -> Result<Value> {
    if value.is_null() {
        return Ok(Value::Null);
    }
    let operations = value
        .get("operations")
        .and_then(Value::as_object)
        .ok_or_else(|| anyhow::anyhow!("invalid_signaling_latencies"))?;
    ensure!(operations.len() <= 128, "invalid_signaling_latencies");
    let mut selected = Map::new();
    for (name, value) in operations {
        ensure!(
            !name.is_empty()
                && name.len() <= 64
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_'),
            "invalid_signaling_latencies"
        );
        let stats: crate::LatencyStats = serde_json::from_value(value.clone())
            .map_err(|_| anyhow::anyhow!("invalid_signaling_latencies"))?;
        ensure!(
            stats.histogram_ms.len() <= 4096
                && u64::try_from(stats.count).is_ok_and(|value| value <= 9_007_199_254_740_991)
                && [
                    stats.min_ms,
                    stats.max_ms,
                    stats.avg_ms,
                    stats.p50_ms,
                    stats.p95_ms,
                    stats.p99_ms
                ]
                .iter()
                .all(|value| *value <= 9_007_199_254_740_991)
                && stats
                    .histogram_ms
                    .iter()
                    .all(|(endpoint, count)| *endpoint <= 9_007_199_254_740_991
                        && *count <= 9_007_199_254_740_991),
            "invalid_signaling_latencies"
        );
        selected.insert(name.clone(), serde_json::to_value(stats)?);
    }
    Ok(json!({"operations":selected}))
}

/// Completed failures keep useful full-window evidence. Setup or incomplete
/// observations do not become a completed operating point. Only native numeric
/// JSON fields are supported; JavaScript coercion of malformed strings is not.
pub(super) fn generator_verdict(summary: &Value, exit: &Exit) -> Result<Value> {
    let run = &summary["run"];
    let config = &run["configuration"];
    let completed = run["completed"] == true;
    let full = completed
        && matching(&summary["measurement"]["durationMs"], 1_200_000.0)
        && matching(&config["durationSecs"], DURATION as f64)
        && matching(&config["rampUpSecs"], RAMP as f64)
        && matching(&config["warmupSecs"], WARMUP as f64);
    let passed = full
        && exit.success()
        && run["passed"] == true
        && matching(&summary["totalClients"], CLIENTS as f64)
        && matching(&summary["successfulConnections"], CLIENTS as f64)
        && matching(&summary["failedConnections"], 0.0)
        && matching(&summary["failedConsumers"], 0.0)
        && matching(&summary["validatedConsumers"], 1140.0)
        && config["profile"]["name"] == "browser"
        && matching(&config["maxVideoConsumers"], 9.0)
        && numeric(&config["maxAudioConsumers"]).is_some_and(|value| value >= 29.0);
    let mut output = json!({"completed":completed,"fullWindow":full,"passed":passed,
        "failure":if passed {Value::Null} else {json!("generator_workload_failed")},
        "failureReasons":failures(run.get("failureReasons"))?})
    .as_object()
    .unwrap()
    .clone();
    for key in [
        "totalClients",
        "failedConnections",
        "failedConsumers",
        "validatedConsumers",
    ] {
        if let Some(value) = summary.get(key) {
            ensure!(integer(value).is_some(), "invalid_generator_counters");
            output.insert(key.into(), value.clone());
        }
    }
    if let Some(value) = summary.get("chat") {
        output.insert("chat".into(), selected_chat(value)?);
    }
    if let Some(value) = summary.get("signalingLatencies") {
        output.insert("signalingLatencies".into(), selected_latencies(value)?);
    }
    Ok(Value::Object(output))
}

pub(super) fn authentication_verdict(
    authentication: &Value,
    account_ids: &[String],
) -> Result<Value> {
    let rows = authentication
        .as_array()
        .filter(|values| values.len() == CLIENTS)
        .ok_or_else(|| anyhow::anyhow!("incomplete_authentication_cohort"))?;
    let expected: BTreeSet<_> = account_ids.iter().map(String::as_str).collect();
    let observed: BTreeSet<_> = rows
        .iter()
        .filter_map(|row| row["accountId"].as_str())
        .collect();
    ensure!(
        expected.len() == CLIENTS && observed.len() == CLIENTS,
        "duplicate_authentication_account"
    );
    let (mut refreshes, mut renewals) = (0.0, 0.0);
    for row in rows {
        let original = numeric(&row["originalExpiresAt"]);
        let latest = numeric(&row["latestExpiresAt"]);
        let http = integer(&row["httpRefreshes"]);
        let socket = integer(&row["socketRenewals"]);
        ensure!(
            row["accountId"]
                .as_str()
                .is_some_and(|id| expected.contains(id))
                && row["passed"] == true
                && row["originalExpiryCrossed"] == true
                && http.is_some_and(|value| value >= 1.0)
                && socket.is_some_and(|value| value >= 1.0)
                && original
                    .zip(latest)
                    .is_some_and(|(original, latest)| latest > original)
                && matching(&row["failures"], 0.0),
            "authentication_refresh_or_expiry_failed"
        );
        refreshes += http.unwrap();
        renewals += socket.unwrap();
    }
    ensure!(
        refreshes <= SAFE_INTEGER && renewals <= SAFE_INTEGER,
        "authentication_refresh_or_expiry_failed"
    );
    Ok(
        json!({"passed":true,"accounts":CLIENTS,"httpRefreshes":number(refreshes),
        "socketRenewals":number(renewals),"originalExpiryCrossed":CLIENTS}),
    )
}

pub(super) fn retention_verdict(
    rows: &Value,
    inventory: &Value,
    account_ids: &[String],
) -> Result<Value> {
    let rows = rows
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("retained_inventory_missing"))?;
    let inventory = inventory
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or_else(|| anyhow::anyhow!("retained_inventory_missing"))?;
    ensure!(
        rows.len() == inventory.len(),
        "retained_message_count_mismatch"
    );
    let mut expected = BTreeMap::new();
    for row in inventory {
        let id = row["messageId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("unexpected_retained_message"))?;
        expected.insert(id, row);
    }
    ensure!(
        expected.len() == inventory.len(),
        "duplicate_expected_message_id"
    );
    let allowed: BTreeSet<_> = account_ids.iter().map(String::as_str).collect();
    let (mut seen, mut senders) = (BTreeSet::new(), BTreeSet::new());
    for row in rows {
        let message = row["messageId"].as_str();
        let sender = row["senderAccountId"].as_str();
        let original = message.and_then(|id| expected.get(id));
        ensure!(
            original.is_some()
                && message.is_some_and(|id| seen.insert(id))
                && sender.is_some_and(|id| allowed.contains(id)),
            "unexpected_retained_message"
        );
        let original = original.unwrap();
        let content = row["content"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("retained_message_content_mismatch"))?;
        ensure!(
            row["clientMessageId"].as_str().is_some()
                && original["clientMessageId"].as_str().is_some()
                && row["clientMessageId"] == original["clientMessageId"]
                && row["senderAccountId"] == original["senderAccountId"]
                && original["contentSha256"] == hex::encode(Sha256::digest(content.as_bytes())),
            "retained_message_content_mismatch"
        );
        senders.insert(sender.unwrap());
    }
    ensure!(
        senders.len() == CLIENTS,
        "incomplete_retained_sender_cohort"
    );
    Ok(json!({"passed":true,"messages":rows.len(),"accounts":CLIENTS}))
}

fn field(value: &Value, key: &str) -> Result<f64> {
    numeric(&value[key]).ok_or_else(|| anyhow::anyhow!("invalid_resource_observation"))
}
fn observation(row: &Value, key: &str) -> bool {
    row.get(key).is_some_and(|value| !value.is_null())
}
fn process(value: &Value) -> Result<resources::ProcessSample> {
    Ok(resources::ProcessSample {
        rss_ki_b: field(value, "rssKiB")?,
        cpu_seconds: field(value, "cpuSeconds")?,
    })
}

fn identities(value: &Value) -> Result<Value> {
    let rows = value
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 128)
        .ok_or_else(|| anyhow::anyhow!("unstable_or_missing_server_process_tree"))?;
    let mut selected = Vec::with_capacity(rows.len());
    for row in rows {
        ensure!(
            integer(&row["pid"]).is_some_and(|value| value > 1.0)
                && integer(&row["startTicks"]).is_some(),
            "unstable_or_missing_server_process_tree"
        );
        selected.push(json!({"pid":row["pid"],"startTicks":row["startTicks"]}));
    }
    Ok(Value::Array(selected))
}

/// Inclusive steady-state resource policy. Missing observations remain failed
/// coverage; completed generator failures can still be assessed independently.
/// The owner separately applies the exactly1200 runtime-lag observations gate.
pub(super) fn measurement_resources(samples: &[Value], start: f64, end: f64) -> Result<Value> {
    ensure!(
        start.is_finite() && end.is_finite() && end > start,
        "invalid_measurement_window"
    );
    let mut projected = Vec::with_capacity(samples.len());
    let mut window = Vec::new();
    for row in samples {
        let elapsed = field(row, "elapsedMs")?;
        if elapsed >= start && elapsed <= end {
            window.push(row);
        }
        if elapsed >= start && elapsed <= end {
            projected.push(resources::Sample {
                elapsed_ms: elapsed,
                server: if observation(row, "server") {
                    Some(process(&row["server"])?)
                } else {
                    None
                },
                generator: if observation(row, "generator") {
                    Some(process(&row["generator"])?)
                } else {
                    None
                },
            });
        }
    }
    let observed_identities: BTreeSet<_> = window
        .iter()
        .filter(|row| observation(row, "server"))
        .map(|row| identities(&row["server"]["processes"]).map(|value| value.to_string()))
        .collect::<Result<_>>()?;
    ensure!(
        observed_identities.len() == 1
            && window.iter().any(|row| row["server"]["processes"]
                .as_array()
                .is_some_and(|values| !values.is_empty())),
        "unstable_or_missing_server_process_tree"
    );
    let mut server = serde_json::to_value(
        resources::summarize(&projected, resources::Role::Server, start, end)
            .map_err(|_| anyhow::anyhow!("incomplete_server_resource_coverage"))?,
    )?;
    let generator = serde_json::to_value(
        resources::summarize(&projected, resources::Role::Generator, start, end)
            .map_err(|_| anyhow::anyhow!("incomplete_generator_resource_coverage"))?,
    )?;
    let first_server = &window
        .iter()
        .find(|row| observation(row, "server"))
        .unwrap()["server"];
    server["processes"] = identities(&first_server["processes"])?;
    ensure!(
        integer(&first_server["childCount"]).is_some(),
        "invalid_resource_observation"
    );
    server["childCount"] = first_server["childCount"].clone();
    server["scope"] = json!(concat!(
        "Server process including every thread (embedded native workers), ",
        "plus owned descendants; RSS sums process resident pages including shared mappings"
    ));
    let hosts: Vec<_> = window
        .iter()
        .filter(|row| observation(row, "host"))
        .collect();
    ensure!(
        hosts.len() >= 2 && hosts.len() as f64 >= window.len() as f64 * 0.9,
        "incomplete_host_resource_coverage"
    );
    let first = &hosts[0]["host"];
    let last = &hosts[hosts.len() - 1]["host"];
    let total = field(last, "total")? - field(first, "total")?;
    ensure!(total > 0.0, "invalid_host_cpu_window");
    let busy = 1.0 - (field(last, "idle")? - field(first, "idle")?) / total;
    let steal = (field(last, "steal")? - field(first, "steal")?) / total;
    ensure!(
        (0.0..=1.0).contains(&busy) && (0.0..=1.0).contains(&steal),
        "invalid_host_cpu_counter_delta"
    );
    let scrapes: Vec<_> = window
        .iter()
        .filter(|row| row["metricsExpected"] == true)
        .collect();
    let healthy: Vec<_> = scrapes
        .iter()
        .filter(|row| observation(row, "metrics") && observation(row, "database"))
        .collect();
    ensure!(
        scrapes.len() >= DURATION / 6 && healthy.len() as f64 >= scrapes.len() as f64 * 0.9,
        "incomplete_database_metrics_coverage"
    );
    ensure!(
        window
            .windows(2)
            .all(|pair| field(pair[1], "elapsedMs").unwrap()
                - field(pair[0], "elapsedMs").unwrap()
                <= 1500.0),
        "resource_sampling_stalled"
    );
    let cgroups: Vec<_> = window
        .iter()
        .filter(|row| observation(row, "cgroup"))
        .collect();
    ensure!(
        !cgroups.is_empty() && cgroups.len() as f64 >= window.len() as f64 * 0.9,
        "missing_cgroup_throttling_evidence"
    );
    let periods = field(&cgroups[cgroups.len() - 1]["cgroup"], "periods")?
        - field(&cgroups[0]["cgroup"], "periods")?;
    let throttled = field(&cgroups[cgroups.len() - 1]["cgroup"], "throttledPeriods")?
        - field(&cgroups[0]["cgroup"], "throttledPeriods")?;
    ensure!(
        periods >= 0.0 && throttled >= 0.0 && throttled <= periods,
        "invalid_cgroup_throttling_window"
    );
    let throttled_share = if periods == 0.0 {
        0.0
    } else {
        throttled / periods
    };
    let mut peak_worker = f64::NEG_INFINITY;
    let (mut database_errors, mut saturated) = (false, false);
    let (mut timings, mut used, mut maximum, mut waiting) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    let mut membership = Vec::new();
    for row in &healthy {
        let metrics = row["metrics"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("invalid_resource_observation"))?;
        for (key, value) in metrics {
            if key.starts_with("simplestchat_media_worker_cpu{") {
                let value =
                    numeric(value).ok_or_else(|| anyhow::anyhow!("missing_worker_cpu_evidence"))?;
                peak_worker = peak_worker.max(value);
            }
            if key.starts_with("simplestchat_database_errors_total{") && !matching(value, 0.0) {
                database_errors = true;
            }
        }
        saturated |= !matching(&row["metrics"]["simplestchat_cpu_saturated"], 0.0)
            || !matching(&row["metrics"]["simplestchat_memory_saturated"], 0.0);
        ensure!(
            numeric(&row["metrics"]["simplestchat_db_pool_connections{state=\"max\"}"])
                .is_some_and(|value| value > 0.0)
                && numeric(&row["metrics"]["simplestchat_db_pool_connections{state=\"used\"}"])
                    .is_some(),
            "missing_application_pool_evidence"
        );
        timings.push(field(&row["database"], "queryRoundTripMs")?);
        used.push(field(
            &row["metrics"],
            "simplestchat_db_pool_connections{state=\"used\"}",
        )?);
        maximum.push(field(
            &row["metrics"],
            "simplestchat_db_pool_connections{state=\"max\"}",
        )?);
        waiting.push(field(&row["database"], "waiting")?);
        if matching(
            &row["metrics"]["simplestchat_participants_snapshot_complete"],
            1.0,
        ) {
            membership.push(*row);
        }
    }
    ensure!(peak_worker.is_finite(), "missing_worker_cpu_evidence");
    ensure!(
        membership.len() as f64 >= healthy.len() as f64 * 0.9,
        "incomplete_live_membership_evidence"
    );
    let cohort = membership.iter().all(|row| {
        matching(
            &row["metrics"]["simplestchat_connections_active"],
            CLIENTS as f64,
        ) && matching(
            &row["metrics"]["simplestchat_participants_active"],
            CLIENTS as f64,
        ) && matching(&row["metrics"]["simplestchat_rooms_active"], 1.0)
    });
    timings.sort_by(f64::total_cmp);
    let percentile = |fraction: f64| timings[(timings.len() as f64 * fraction).ceil() as usize - 1];
    let maximum_value = |values: &[f64]| values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let available: Vec<_> = hosts
        .iter()
        .map(|row| field(&row["host"], "availableBytes"))
        .collect::<Result<_>>()?;
    Ok(
        json!({"passed":cohort && busy <= 0.85 && steal <= 0.05 && throttled_share <= 0.05
            && peak_worker <= 0.7 && !database_errors && !saturated,
        "server":server,"generator":generator,
        "host":{"busyShare":number(busy),"stealShare":number(steal),
            "minimumAvailableBytes":number(available.into_iter().fold(f64::INFINITY,f64::min))},
        "cgroup":{"throttledShare":number(throttled_share),
            "quotaCores":cgroups[0]["cgroup"]["quotaCores"],
            "scope":"shared runner cgroup, not separate per-process quotas"},
        "peakWorkerCores":number(peak_worker),"databaseErrors":database_errors,
        "saturated":saturated,"completeCohort":cohort,"membershipSamples":membership.len(),
        "metricsSamples":healthy.len(),"database":{
            "probe":"psql query round trip after connection; separate from application pool waits",
            "p50Ms":number(percentile(0.5)),"p99Ms":number(percentile(0.99)),
            "peakPoolUsed":number(maximum_value(&used)),
            "maxPoolConnections":number(maximum_value(&maximum)),
            "peakWaitingSessions":number(maximum_value(&waiting))}}),
    )
}

#[cfg(test)]
#[path = "policy_oracle_tests.rs"]
mod oracle_tests;
#[cfg(test)]
#[path = "policy_tests.rs"]
mod tests;
