//! Consumer counters compared only within an unchanged native identity.

use super::media_generator::Consumer;
use super::media_schema::{reference, uint};
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) struct Row {
    pub value: Value,
    pub matched: bool,
    pub short_lived_unobserved: bool,
    pub has_pair: bool,
}

pub(super) fn consumer(
    consumer: &Consumer,
    salt: &str,
    samples: &[Value],
    issues: &mut BTreeSet<&'static str>,
) -> Row {
    let reference = reference(salt, "consumer", &consumer.id);
    let producer_reference =
        super::media_schema::reference(salt, "producer", &consumer.producer_id);
    let mut observations = Vec::new();
    let mut matched = false;
    for sample in samples {
        let entity = sample["snapshot"]["entities"]
            .as_array()
            .unwrap()
            .iter()
            .find(|entity| entity["entityType"] == "consumer" && entity["reference"] == reference);
        let Some(entity) = entity else {
            observations.push(json!({"sampleOrdinal":sample["ordinal"], "status":"not_observed"}));
            continue;
        };
        if entity["producerReference"] != producer_reference || entity["kind"] != consumer.kind {
            issues.insert("consumer_identity_mismatch");
            observations
                .push(json!({"sampleOrdinal":sample["ordinal"], "status":"identity_mismatch"}));
            continue;
        }
        let stream = entity["streams"]
            .as_array()
            .unwrap()
            .iter()
            .find(|stream| uint(&stream["ssrc"]) == Some(consumer.ssrc));
        matched = true;
        observations.push(json!({
            "sampleOrdinal":sample["ordinal"], "status":entity["status"], "paused":entity["paused"],
            "producerPaused":entity["producerPaused"],
            "transportReference":entity["transportReference"],
            "observedUs":entity["observedUs"], "stream":stream
        }));
        if !["ok", "closed", "no_streams"]
            .iter()
            .any(|status| entity["status"] == *status)
        {
            issues.insert("consumer_observation_incomplete");
        }
        if entity["status"] == "ok" && stream.is_none() {
            issues.insert("consumer_stream_missing");
        }
    }
    if !matched && !consumer.short_lived {
        issues.insert("consumer_not_observed");
    }
    let counted: Vec<_> = observations
        .iter()
        .filter(|row| row["status"] == "ok" && !row["stream"].is_null())
        .collect();
    if counted
        .iter()
        .any(|row| row["transportReference"].is_null())
    {
        issues.insert("consumer_identity_unavailable");
    }
    let has_pair = counted.len() >= 2
        && counted
            .iter()
            .all(|row| !row["transportReference"].is_null());
    if !has_pair && !consumer.short_lived {
        issues.insert("consumer_counter_pair_missing");
    }
    let mut movement = if counted.is_empty() {
        "unavailable"
    } else {
        "single_observation"
    };
    let (mut packet_delta, mut byte_delta) = (None, None);
    if counted.len() >= 2 {
        let reset = counted.windows(2).any(|pair| {
            pair[0]["transportReference"] != pair[1]["transportReference"]
                || ["packetCount", "rtpBytes", "workerTimestampMs"]
                    .iter()
                    .any(|key| {
                        uint(&pair[1]["stream"][*key]).unwrap()
                            < uint(&pair[0]["stream"][*key]).unwrap()
                    })
        });
        let (first, last) = (counted[0], counted[counted.len() - 1]);
        if reset {
            movement = "reset_or_replaced";
        } else if first["transportReference"].is_null() || last["transportReference"].is_null() {
            movement = "identity_unavailable";
        } else {
            let packets = uint(&last["stream"]["packetCount"]).unwrap()
                - uint(&first["stream"]["packetCount"]).unwrap();
            packet_delta = Some(packets);
            byte_delta = Some(
                uint(&last["stream"]["rtpBytes"]).unwrap()
                    - uint(&first["stream"]["rtpBytes"]).unwrap(),
            );
            movement = if packets > 0 {
                "increased"
            } else if uint(&last["stream"]["packetCount"]) == Some(0) {
                "flat_zero"
            } else {
                "flat_nonzero"
            };
        }
    }
    Row {
        matched,
        short_lived_unobserved: !matched && consumer.short_lived,
        has_pair,
        value: json!({
            "consumerReference":reference, "producerReference":producer_reference,
            "ssrc":consumer.ssrc,
            "kind":consumer.kind, "generator":consumer.generator, "observations":observations,
            "nativeAccounting":{"movement":movement,
                "packetDelta":packet_delta, "byteDelta":byte_delta}
        }),
    }
}
