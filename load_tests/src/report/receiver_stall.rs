//! Scheduled-sample identity joins kept separate from stall-trigger evidence.

use super::media;
use super::media_generator;
use super::media_schema::{MAX_SAFE, reference, uint};
use super::receiver_stall_evidence;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) const INTERPRETATION: &str = "Identity correlation with retained scheduled SFU samples, \
    not observations at the stall. Trigger elapsedMs uses the client collector clock; sample \
    elapsedMs uses the runner clock; observedUs and workerTimestampMs retain native clocks. No \
    before/after-trigger arithmetic, loss estimate, causal attribution or continuous-health claim. \
    Empty legacy reports do not establish detector support. At most 128 triggers are retained in \
    client/attempt order; receiver snapshots remain in load_test_results.json.";

fn accounting(observations: &[Value]) -> Value {
    let usable: Vec<_> = observations
        .iter()
        .filter(|row| row["status"] == "ok" && !row["streams"].as_array().unwrap().is_empty())
        .collect();
    let initial = if usable.is_empty() {
        "unavailable"
    } else {
        "single_observation"
    };
    let result = |movement| json!({"movement":movement, "packetDelta":null, "byteDelta":null});
    if usable.len() < 2 {
        return result(initial);
    }
    if usable.iter().any(|row| row["transportReference"].is_null()) {
        return result("identity_unavailable");
    }
    for pair in usable.windows(2) {
        if pair[0]["transportReference"] != pair[1]["transportReference"] {
            return result("reset_or_replaced");
        }
        let previous: BTreeMap<_, _> = pair[0]["streams"]
            .as_array()
            .unwrap()
            .iter()
            .map(|stream| (uint(&stream["ssrc"]).unwrap(), stream))
            .collect();
        let after = pair[1]["streams"].as_array().unwrap();
        if previous.len() != after.len()
            || after
                .iter()
                .any(|stream| !previous.contains_key(&uint(&stream["ssrc"]).unwrap()))
        {
            return result("stream_set_changed");
        }
        if after.iter().any(|stream| {
            let prior = previous[&uint(&stream["ssrc"]).unwrap()];
            ["packetCount", "rtpBytes", "workerTimestampMs"]
                .iter()
                .any(|key| uint(&stream[*key]).unwrap() < uint(&prior[*key]).unwrap())
        }) {
            return result("reset_or_replaced");
        }
    }
    // Accumulate like the JS reference: all terms are individually safe, but
    // their total may not be. The final safe-integer test gates any subtraction.
    let total = |row: &Value, field: &str| -> Option<u64> {
        let sum = row["streams"]
            .as_array()
            .unwrap()
            .iter()
            .fold(0.0, |sum, stream| {
                sum + uint(&stream[field]).unwrap() as f64
            });
        (sum <= MAX_SAFE as f64 && sum.fract() == 0.0).then_some(sum as u64)
    };
    let (first, last) = (usable[0], usable[usable.len() - 1]);
    let totals: Option<Vec<_>> = [
        total(first, "packetCount"),
        total(last, "packetCount"),
        total(first, "rtpBytes"),
        total(last, "rtpBytes"),
    ]
    .into_iter()
    .collect();
    let Some(totals) = totals else {
        return result("counter_overflow");
    };
    let packets = totals[1] - totals[0];
    json!({"movement":if packets > 0 { "increased" } else if totals[1] == 0 { "flat_zero" }
        else { "flat_nonzero" }, "packetDelta":packets, "byteDelta":totals[3] - totals[2]})
}

fn producer_observations(
    samples: &[Value],
    rows: &[Value],
    producer_id: &str,
    kind: &str,
    issues: &mut BTreeSet<&'static str>,
) -> Vec<Value> {
    let mut observations = Vec::new();
    for row in rows {
        let sample = samples
            .iter()
            .find(|sample| sample["ordinal"] == row["sampleOrdinal"])
            .expect("consumer observations enumerate only the validated native samples");
        let native = &sample["snapshot"];
        let reference = reference(
            native["correlationSalt"].as_str().unwrap(),
            "producer",
            producer_id,
        );
        let producer = native["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entity| entity["entityType"] == "producer" && entity["reference"] == reference);
        let status = match producer {
            None => Some("not_observed"),
            Some(producer) if producer["kind"] != kind => {
                issues.insert("producer_identity_mismatch");
                Some("identity_mismatch")
            }
            _ => None,
        };
        if let Some(status) = status {
            observations.push(json!({"sampleOrdinal":sample["ordinal"], "status":status,
                "paused":null, "transportReference":null, "observedUs":null, "streams":[]}));
        } else {
            let producer = producer.unwrap();
            observations.push(
                json!({"sampleOrdinal":sample["ordinal"], "status":producer["status"],
                "paused":producer["paused"], "transportReference":producer["transportReference"],
                "observedUs":producer["observedUs"], "streams":producer["streams"]}),
            );
        }
    }
    observations
}

pub(super) fn correlate(samples: &Value, results: &Value) -> Value {
    let mut report = json!({"schemaVersion":1, "coverage":{"available":false, "complete":false,
        "scope":"recorded_stall_triggers", "issues":[], "triggersObserved":0, "entriesRetained":0},
        "samples":[], "stalls":[], "interpretation":INTERPRETATION});
    let Ok(identities) = media_generator::consumers(results) else {
        report["coverage"]["issues"] = json!(["generator_stall_evidence_unavailable"]);
        return report;
    };
    let Ok(evidence) = receiver_stall_evidence::read(results, &identities) else {
        report["coverage"]["issues"] = json!(["generator_stall_evidence_unavailable"]);
        return report;
    };
    report["coverage"]["available"] = true.into();
    report["coverage"]["triggersObserved"] = evidence.stalls.len().into();
    let mut issues = evidence.issues;
    if results.as_array().unwrap().iter().any(|client| {
        !client["diagnostics"]["failures"]
            .as_array()
            .unwrap()
            .is_empty()
    }) {
        issues.insert("generator_diagnostics_incomplete");
    }
    if evidence.stalls.is_empty() {
        report["coverage"]["complete"] = if issues.is_empty() {
            Value::Null
        } else {
            false.into()
        };
        report["coverage"]["issues"] = json!(issues);
        return report;
    }
    let analysis = media::analyze(samples, results);
    report["samples"] = analysis.report["samples"].clone();
    if analysis.report["coverage"]["complete"] != true {
        issues.insert("scheduled_media_evidence_incomplete");
    }
    if evidence.stalls.len() > 128 {
        issues.insert("stall_report_limit");
    }
    let rows: BTreeMap<_, _> = analysis.report["consumers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                (
                    uint(&row["generator"]["clientOrdinal"]).unwrap(),
                    uint(&row["generator"]["consumerOrdinal"]).unwrap(),
                ),
                row,
            )
        })
        .collect();
    let mut entries = Vec::new();
    for stall in evidence.stalls.into_iter().take(128) {
        let row = rows
            .get(&(stall.client as u64, stall.consumer as u64))
            .copied();
        let mut entry_issues: BTreeSet<_> = stall.issues.into_iter().collect();
        let empty = Vec::new();
        let observations = row
            .and_then(|row| row["observations"].as_array())
            .unwrap_or(&empty);
        if observations
            .iter()
            .filter(|row| {
                row["status"] == "ok"
                    && !row["stream"].is_null()
                    && !row["transportReference"].is_null()
            })
            .count()
            < 2
        {
            entry_issues.insert("consumer_counter_pair_missing");
        }
        if row.is_some_and(|row| {
            ["reset_or_replaced", "identity_unavailable"]
                .iter()
                .any(|movement| row["nativeAccounting"]["movement"] == *movement)
        }) {
            entry_issues.insert("consumer_counter_comparison_unavailable");
        }
        let producers = producer_observations(
            &analysis.validated_samples,
            observations,
            &stall.identity.producer_id,
            &stall.identity.kind,
            &mut entry_issues,
        );
        if producers
            .iter()
            .filter(|row| {
                row["status"] == "ok"
                    && !row["streams"].as_array().unwrap().is_empty()
                    && !row["transportReference"].is_null()
            })
            .count()
            < 2
        {
            entry_issues.insert("producer_counter_pair_missing");
        }
        let accounting = accounting(&producers);
        if [
            "reset_or_replaced",
            "stream_set_changed",
            "identity_unavailable",
            "counter_overflow",
        ]
        .iter()
        .any(|movement| accounting["movement"] == *movement)
        {
            entry_issues.insert("producer_counter_comparison_unavailable");
        }
        issues.extend(&entry_issues);
        let consumer_accounting = row
            .map(|row| row["nativeAccounting"].clone())
            .unwrap_or_else(|| {
                json!({"movement":"unavailable",
                "packetDelta":null, "byteDelta":null})
            });
        entries.push(json!({
            "clientOrdinal":stall.client, "attemptOrdinal":stall.attempt,
            "consumerOrdinal":stall.consumer,
            "trigger":stall.trigger, "receiverCapture":stall.capture,
            "consumerReference":row.map(|row| &row["consumerReference"]),
            "producerReference":row.map(|row| &row["producerReference"]),
            "kind":stall.identity.kind,
            "delivery":{"passed":stall.delivery["passed"],
                "eligibleSeconds":stall.delivery["eligibleSeconds"],
                "secondsWithPackets":stall.delivery["secondsWithPackets"],
                "longestGapSeconds":stall.delivery["longestGapSeconds"]},
            "consumer":{"observations":observations, "nativeAccounting":consumer_accounting},
            "producer":{"observations":producers, "nativeAccounting":accounting},
            "issues":entry_issues
        }));
    }
    report["coverage"]["entriesRetained"] = entries.len().into();
    report["stalls"] = entries.into();
    report["coverage"]["complete"] = issues.is_empty().into();
    report["coverage"]["issues"] = json!(issues);
    report
}
