//! Offline correlation of independently clocked native samples and deliveries.
//!
//! This is evidence, not a workload verdict. Never subtract clocks belonging to
//! different collectors or infer packet loss, receiver health or causation.

use super::media_generator;
use super::media_schema::{keys, one_of, snapshot, uint};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) const INTERPRETATION: &str = "Native cumulative RTP accounting and generator \
    measurement buckets have different intervals. No cross-clock arithmetic or \
    packet-loss/peer-receipt inference. Flat counters are observations, not workload failures; \
    sparse samples cannot exclude unobserved resets or state changes.";

pub(super) struct Analysis {
    pub report: Value,
    pub validated_samples: Vec<Value>,
}

fn elapsed(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|value| value.is_finite() && *value >= 0.0)
}

fn valid_sample(sample: &Value) -> bool {
    let Some(start) = uint(&sample["windowStartElapsedMs"]) else {
        return false;
    };
    let Some(end) = uint(&sample["windowEndElapsedMs"]) else {
        return false;
    };
    sample["schemaVersion"] == 1
        && uint(&sample["ordinal"]).is_some_and(|ordinal| (1..=3).contains(&ordinal))
        && start >= 3000
        && end
            .checked_sub(start)
            .is_some_and(|length| (3000..=180000).contains(&length))
        && uint(&sample["scheduledElapsedMs"]).is_some_and(|time| (start..end).contains(&time))
        && elapsed(&sample["finishedElapsedMs"]).is_some()
        && (sample["startedElapsedMs"].is_null() || elapsed(&sample["startedElapsedMs"]).is_some())
        && sample.get("startedElapsedMs").is_some()
        && one_of(&sample["status"], &["ok", "error", "missed"])
        && ["aliveAtStart", "aliveAtEnd"].iter().all(|key| {
            let alive = &sample[*key];
            keys(alive, &["server", "generator"])
                && alive["server"].is_boolean()
                && alive["generator"].is_boolean()
        })
}

/// Match JS truthiness only for optional issue markers; their content is never
/// copied into the report, and a marker cannot establish complete coverage.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_none_or(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn sample_summary(sample: &Value) -> Value {
    json!({
        "ordinal":sample["ordinal"], "status":sample["status"],
        "scheduledElapsedMs":sample["scheduledElapsedMs"],
        "startedElapsedMs":sample["startedElapsedMs"],
        "finishedElapsedMs":sample["finishedElapsedMs"], "aliveAtStart":sample["aliveAtStart"],
        "aliveAtEnd":sample["aliveAtEnd"]
    })
}

fn samples(
    input: &Value,
    issues: &mut BTreeSet<&'static str>,
) -> (Vec<Value>, Vec<Value>, Option<String>) {
    let empty = Vec::new();
    let input = input.as_array().unwrap_or(&empty);
    if !(2..=3).contains(&input.len()) {
        issues.insert("media_sample_count");
    }
    let mut valid = Vec::new();
    let mut summaries = Vec::new();
    let mut salt = None;
    let (mut last_id, mut last_ordinal, mut last_scheduled) = (0, 0, None);
    let (mut last_started, mut last_finished) = (f64::NEG_INFINITY, f64::NEG_INFINITY);
    let mut window = None;
    for sample in input.iter().take(3) {
        if !valid_sample(sample) {
            issues.insert("invalid_media_sample");
            continue;
        }
        let ordinal = uint(&sample["ordinal"]).unwrap();
        let scheduled = uint(&sample["scheduledElapsedMs"]).unwrap();
        let current_window = (
            uint(&sample["windowStartElapsedMs"]).unwrap(),
            uint(&sample["windowEndElapsedMs"]).unwrap(),
        );
        if ordinal <= last_ordinal
            || last_scheduled.is_some_and(|last| scheduled <= last)
            || window.is_some_and(|window| window != current_window)
        {
            issues.insert("media_sample_order");
            continue;
        }
        window = Some(current_window);
        let (start, end) = current_window;
        let length = end - start;
        let mut schedule = vec![start as f64, start as f64 + length as f64 / 2.0];
        if length >= 4000 {
            schedule.push((end - 2000) as f64);
        }
        schedule.sort_by(f64::total_cmp);
        schedule.dedup();
        if schedule.get((ordinal - 1) as usize) != Some(&(scheduled as f64))
            || input.len() != schedule.len()
        {
            issues.insert("media_schedule_mismatch");
        }
        last_ordinal = ordinal;
        last_scheduled = Some(scheduled);
        let started = elapsed(&sample["startedElapsedMs"]);
        let finished = elapsed(&sample["finishedElapsedMs"]).unwrap();
        let timing_valid = if sample["status"] == "missed" {
            started.is_none()
        } else {
            started.is_some_and(|started| {
                started >= scheduled as f64
                    && finished >= started
                    && started - last_started >= 250.0
                    && started >= last_finished
            })
        };
        if !timing_valid {
            issues.insert("invalid_media_sample_timing");
            continue;
        }
        if let Some(started) = started {
            last_started = started;
            last_finished = finished;
        }
        summaries.push(sample_summary(sample));
        if sample["status"] != "ok" {
            issues.insert("media_sample_unavailable");
            continue;
        }
        let native = &sample["snapshot"];
        if snapshot(native).is_err() {
            issues.insert("invalid_media_schema");
            continue;
        }
        let started = started.expect("ok samples require a start time");
        if truthy(&sample["timingIssue"])
            || truthy(&sample["persistenceIssue"])
            || ["aliveAtStart", "aliveAtEnd"]
                .iter()
                .any(|key| sample[*key]["generator"] != true || sample[*key]["server"] != true)
            || started - scheduled as f64 > 1000.0
            || finished >= end as f64
            || finished - started > 2000.0
        {
            issues.insert("media_sample_incomplete");
        }
        let coverage = &native["coverage"];
        if coverage["complete"] != true
            || coverage["registryBusy"] == true
            || uint(&coverage["busyParticipants"]) != Some(0)
            || [
                "participantLimitReached",
                "entityLimitReached",
                "deadlineReached",
            ]
            .iter()
            .any(|key| coverage[*key] == true)
            || native["entities"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entity| entity["status"] != "ok")
        {
            issues.insert("native_coverage_incomplete");
        }
        let current_salt = native["correlationSalt"].as_str().unwrap();
        if salt.as_deref().is_some_and(|salt| salt != current_salt) {
            issues.insert("media_namespace_changed");
            continue;
        }
        salt = Some(current_salt.to_owned());
        let id = uint(&native["sampleId"]).unwrap();
        if id <= last_id {
            issues.insert("media_sample_order");
            continue;
        }
        last_id = id;
        valid.push(sample.clone());
    }
    if valid.len() < 2 {
        issues.insert("insufficient_media_samples");
    }
    (summaries, valid, salt)
}

pub(super) fn analyze(input: &Value, results: &Value) -> Analysis {
    let mut issues = BTreeSet::new();
    let identities = match media_generator::consumers(results) {
        Ok(consumers) => {
            if results.as_array().unwrap().iter().any(|client| {
                !client["diagnostics"]["failures"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            }) {
                issues.insert("generator_diagnostics_incomplete");
            }
            Some(consumers)
        }
        Err(_) => {
            issues.insert("generator_correlation_unavailable");
            None
        }
    };
    let (summaries, valid, salt) = samples(input, &mut issues);
    let available = identities.is_some() && !valid.is_empty();
    let mut rows = Vec::new();
    let (mut matched, mut short_lived, mut pairs) = (0, 0, 0);
    if let (Some(identities), Some(salt)) = (identities, salt) {
        for consumer in identities {
            let row = super::media_correlation::consumer(&consumer, &salt, &valid, &mut issues);
            matched += usize::from(row.matched);
            short_lived += usize::from(row.short_lived_unobserved);
            pairs += usize::from(row.has_pair);
            rows.push(row.value);
        }
    }
    let report = json!({
        "schemaVersion":1, "coverage": {
            "available":available, "complete":available && issues.is_empty() && matched > 0,
            "issues":issues, "expectedConsumers":rows.len(), "matchedConsumers":matched,
            "shortLivedUnobservedConsumers":short_lived, "consumersWithCounterPairs":pairs
        }, "samples":summaries, "consumers":rows, "interpretation":INTERPRETATION
    });
    Analysis {
        report,
        validated_samples: valid,
    }
}

/// Match JSON.parse's numeric domain before comparisons. Canonical reports keep
/// safe integral values as integers; sub-millisecond timings retain fractions.
pub(super) fn normalize_numbers(value: &mut Value) {
    match value {
        Value::Number(_) => {
            if let Some(number) = uint(value) {
                *value = number.into();
            }
        }
        Value::Array(values) => values.iter_mut().for_each(normalize_numbers),
        Value::Object(values) => values.values_mut().for_each(normalize_numbers),
        _ => {}
    }
}
