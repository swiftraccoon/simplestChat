//! Immutable consumer identities and safe aggregate delivery fields.

use super::media_schema::{MAX_SAFE, one_of, uint, uuid};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub(super) struct Consumer {
    pub id: String,
    pub producer_id: String,
    pub kind: String,
    pub ssrc: u64,
    pub short_lived: bool,
    pub generator: Value,
}

struct Created<'a> {
    attempt: u64,
    producer_id: &'a Value,
    ssrc: u64,
    kind: &'a Value,
}

fn array(value: &Value, maximum: usize) -> Result<&Vec<Value>> {
    let values = value.as_array().ok_or_else(invalid)?;
    ensure!(values.len() <= maximum, "invalid_generator_results");
    Ok(values)
}

fn text(value: &Value) -> Result<&str> {
    value.as_str().ok_or_else(invalid)
}

fn invalid() -> anyhow::Error {
    anyhow::anyhow!("invalid_generator_results")
}

/// Extra generator fields stay private. Reports receive only validated ordinals,
/// immutable correlation identities and selected delivery counters/booleans.
pub(super) fn consumers(value: &Value) -> Result<Vec<Consumer>> {
    let clients = array(value, 100)?;
    ensure!(!clients.is_empty(), "invalid_generator_results");
    let mut output = Vec::new();
    let mut all_ids = BTreeSet::new();
    for (client_index, client) in clients.iter().enumerate() {
        let deliveries = array(&client["consumerDelivery"], 2048)?;
        let attempts = array(&client["connectionAttempts"], 128)?;
        let diagnostics = &client["diagnostics"];
        let events = array(&diagnostics["events"], 4096)?;
        let failures = array(&diagnostics["failures"], 4096)?;
        ensure!(
            failures.iter().all(|reason| reason
                .as_str()
                .is_some_and(|reason| reason.encode_utf16().count() <= 4096)),
            "invalid_generator_results"
        );
        let mut created = BTreeMap::new();
        let mut requested = BTreeSet::new();
        let mut resumed = BTreeSet::new();
        let mut received = BTreeSet::new();
        for event in events {
            let attempt = uint(&event["attempt"]).ok_or_else(invalid)?;
            ensure!(
                attempt > 0
                    && attempt <= attempts.len() as u64
                    && uint(&event["elapsedMs"]).is_some()
                    && event["kind"].is_string()
                    && (event["details"].is_object() || event["details"].is_array()),
                "invalid_generator_results"
            );
            let detail = &event["details"];
            match text(&event["kind"])? {
                "consumer-created" => {
                    ensure!(
                        uuid(&detail["consumerId"])
                            && uuid(&detail["producerId"])
                            && uint(&detail["ssrc"]).is_some_and(|ssrc| ssrc <= u32::MAX.into())
                            && one_of(&detail["kind"], &["audio", "video"]),
                        "invalid_generator_results"
                    );
                    let id = text(&detail["consumerId"])?;
                    let prior = created.insert(
                        id,
                        Created {
                            attempt,
                            producer_id: &detail["producerId"],
                            ssrc: uint(&detail["ssrc"]).ok_or_else(invalid)?,
                            kind: &detail["kind"],
                        },
                    );
                    ensure!(prior.is_none(), "invalid_generator_results");
                }
                "resume-requested" | "resume-ack" => {
                    ensure!(uuid(&detail["consumerId"]), "invalid_generator_results");
                    let identity = (attempt, text(&detail["consumerId"])?);
                    if event["kind"] == "resume-ack" {
                        resumed.insert(identity);
                    } else {
                        requested.insert(identity);
                    }
                }
                "track-first-rtp" => {
                    let ssrc = uint(&detail["ssrc"]).ok_or_else(invalid)?;
                    ensure!(ssrc <= u32::MAX.into(), "invalid_generator_results");
                    received.insert((attempt, ssrc));
                }
                _ => {}
            }
        }
        for (consumer_index, delivery) in deliveries.iter().enumerate() {
            ensure!(
                uuid(&delivery["consumerId"]) && uuid(&delivery["producerId"]),
                "invalid_generator_results"
            );
            let id = text(&delivery["consumerId"])?;
            let ssrc = uint(&delivery["ssrc"]).ok_or_else(invalid)?;
            ensure!(ssrc <= u32::MAX.into(), "invalid_generator_results");
            let packets = array(&delivery["packetsBySecond"], 3600)?;
            let mut total = 0_u64;
            for packet in packets {
                total = total
                    .checked_add(uint(packet).ok_or_else(invalid)?)
                    .ok_or_else(invalid)?;
                ensure!(total <= MAX_SAFE, "invalid_generator_results");
            }
            let eligible = uint(&delivery["eligibleSeconds"]).ok_or_else(invalid)?;
            let seconds = uint(&delivery["secondsWithPackets"]).ok_or_else(invalid)?;
            let gap = uint(&delivery["longestGapSeconds"]).ok_or_else(invalid)?;
            ensure!(
                eligible <= packets.len() as u64
                    && seconds <= eligible
                    && gap <= eligible
                    && delivery["passed"].is_boolean()
                    && delivery["skippedShortLived"].is_boolean(),
                "invalid_generator_results"
            );
            let event = created.get(id).ok_or_else(invalid)?;
            ensure!(
                *event.producer_id == delivery["producerId"] && event.ssrc == ssrc,
                "invalid_generator_results"
            );
            if delivery.get("attempt").is_some() {
                ensure!(
                    uint(&delivery["attempt"]) == Some(event.attempt),
                    "invalid_generator_results"
                );
            }
            if delivery.get("isAudio").is_some() {
                ensure!(
                    delivery["isAudio"].as_bool() == Some(event.kind == "audio"),
                    "invalid_generator_results"
                );
            }
            ensure!(all_ids.insert(id.to_owned()), "invalid_generator_results");
            output.push(Consumer {
                id: id.to_owned(),
                producer_id: text(event.producer_id)?.to_owned(),
                kind: text(event.kind)?.to_owned(),
                ssrc,
                short_lived: delivery["skippedShortLived"]
                    .as_bool()
                    .ok_or_else(invalid)?,
                generator: json!({
                    "clientOrdinal": client_index + 1, "consumerOrdinal": consumer_index + 1,
                    "attemptOrdinal": event.attempt, "packetsInMeasurement":total,
                    "eligibleSeconds":eligible, "secondsWithPackets":seconds,
                    "longestGapSeconds":gap, "deliveryPassed":delivery["passed"],
                    "resumeRequested":requested.contains(&(event.attempt, id)),
                    "resumeAcknowledged":resumed.contains(&(event.attempt, id)),
                    "firstRtpEventObserved":received.contains(&(event.attempt, ssrc))
                }),
            });
            ensure!(output.len() <= 16384, "generator_consumer_limit");
        }
    }
    ensure!(!output.is_empty(), "invalid_generator_results");
    Ok(output)
}
