//! Join bounded stall triggers, terminal capture events and retained snapshots.

use super::media_generator::Consumer;
use super::media_schema::{json_bytes, keys, one_of, uint};
use anyhow::{Result, ensure};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

const TRIGGER: &[&str] = &[
    "consumerOrdinal",
    "ssrc",
    "isAudio",
    "beginBucket",
    "endBucket",
];

pub(super) struct Stall<'a> {
    pub client: usize,
    pub attempt: u64,
    pub consumer: usize,
    pub trigger: Value,
    pub identity: &'a Consumer,
    pub delivery: &'a Value,
    pub capture: Value,
    pub issues: Vec<&'static str>,
}

pub(super) struct Evidence<'a> {
    pub stalls: Vec<Stall<'a>>,
    pub issues: BTreeSet<&'static str>,
}

fn invalid() -> anyhow::Error {
    anyhow::anyhow!("invalid_stall_evidence")
}

pub(super) fn read<'a>(results: &'a Value, identities: &'a [Consumer]) -> Result<Evidence<'a>> {
    let identities: BTreeMap<_, _> = identities
        .iter()
        .map(|identity| {
            (
                (
                    uint(&identity.generator["clientOrdinal"]).unwrap(),
                    uint(&identity.generator["consumerOrdinal"]).unwrap(),
                ),
                identity,
            )
        })
        .collect();
    let mut stalls = Vec::new();
    let mut issues = BTreeSet::new();
    let mut snapshots = 0;
    for (client_index, client) in results.as_array().ok_or_else(invalid)?.iter().enumerate() {
        let client_ordinal = client_index + 1;
        let mut triggers = BTreeMap::new();
        let mut completions = BTreeMap::new();
        let mut captures = BTreeMap::new();
        for event in client["diagnostics"]["events"]
            .as_array()
            .ok_or_else(invalid)?
        {
            let attempt = uint(&event["attempt"]).ok_or_else(invalid)?;
            let detail = &event["details"];
            let event_elapsed = uint(&event["elapsedMs"]).ok_or_else(invalid)?;
            if event["kind"] == "receiver-stall-triggered" {
                let trigger = &detail["trigger"];
                let elapsed = uint(&detail["triggerElapsedMs"]).ok_or_else(invalid)?;
                ensure!(
                    keys(detail, &["triggerElapsedMs", "trigger"])
                        && event_elapsed >= elapsed
                        && keys(trigger, TRIGGER)
                        && trigger["isAudio"].is_boolean(),
                    "invalid_stall_evidence"
                );
                let consumer = uint(&trigger["consumerOrdinal"]).ok_or_else(invalid)?;
                let ssrc = uint(&trigger["ssrc"]).ok_or_else(invalid)?;
                let begin = uint(&trigger["beginBucket"]).ok_or_else(invalid)?;
                let end = uint(&trigger["endBucket"]).ok_or_else(invalid)?;
                ensure!(
                    consumer > 0 && ssrc <= u32::MAX.into() && end.checked_sub(begin) == Some(3),
                    "invalid_stall_evidence"
                );
                let identity = *identities
                    .get(&(client_ordinal as u64, consumer))
                    .ok_or_else(invalid)?;
                let delivery = client["consumerDelivery"]
                    .get((consumer - 1) as usize)
                    .ok_or_else(invalid)?;
                ensure!(
                    uint(&identity.generator["attemptOrdinal"]) == Some(attempt)
                        && identity.ssrc == ssrc
                        && (identity.kind == "audio") == (trigger["isAudio"] == true)
                        && uint(&delivery["attempt"]) == Some(attempt)
                        && delivery["isAudio"] == trigger["isAudio"]
                        && delivery["passed"] == false
                        && delivery["skippedShortLived"] == false
                        && uint(&delivery["longestGapSeconds"]).is_some_and(|gap| gap >= 3),
                    "invalid_stall_evidence"
                );
                let packets = delivery["packetsBySecond"].as_array().ok_or_else(invalid)?;
                ensure!(
                    end <= packets.len() as u64
                        && packets[begin as usize..end as usize]
                            .iter()
                            .all(|count| uint(count) == Some(0)),
                    "invalid_stall_evidence"
                );
                ensure!(
                    triggers
                        .insert(
                            attempt,
                            (trigger, elapsed, identity, delivery, event_elapsed)
                        )
                        .is_none(),
                    "invalid_stall_evidence"
                );
            } else if event["kind"] == "receiver-stall-capture" {
                ensure!(
                    keys(detail, &["triggerElapsedMs", "status"])
                        && uint(&detail["triggerElapsedMs"])
                            .is_some_and(|elapsed| event_elapsed >= elapsed)
                        && one_of(
                            &detail["status"],
                            &[
                                "captured",
                                "busy",
                                "budget_exhausted",
                                "timed_out",
                                "attempt_ended",
                                "unavailable",
                                "snapshot_rejected",
                                "cancelled"
                            ]
                        )
                        && completions.insert(attempt, event).is_none(),
                    "invalid_stall_evidence"
                );
            }
        }
        let empty = Vec::new();
        let stored = match client["diagnostics"].get("receiverStalls") {
            Some(value) => value.as_array().ok_or_else(invalid)?,
            None => &empty,
        };
        snapshots += stored.len();
        ensure!(
            stored.len() <= 8 && snapshots <= 8,
            "invalid_stall_evidence"
        );
        for (index, entry) in stored.iter().enumerate() {
            let attempt = uint(&entry["attempt"]).ok_or_else(invalid)?;
            let elapsed = uint(&entry["elapsedMs"]).ok_or_else(invalid)?;
            let detail = &entry["details"];
            ensure!(
                keys(entry, &["attempt", "elapsedMs", "kind", "details"])
                    && attempt > 0
                    && attempt <= client["connectionAttempts"].as_array().unwrap().len() as u64
                    && entry["kind"] == "receiver-stall"
                    && keys(detail, &["triggerElapsedMs", "trigger", "snapshot"])
                    && uint(&detail["triggerElapsedMs"]).is_some_and(|start| elapsed >= start)
                    && json_bytes(entry, 256 * 1024) <= 256 * 1024,
                "invalid_stall_evidence"
            );
            let transports = detail["snapshot"]["transports"]
                .as_array()
                .ok_or_else(invalid)?;
            ensure!(transports.len() == 1, "invalid_stall_evidence");
            let peer = &transports[0];
            let stats = peer["stats"].as_array().ok_or_else(invalid)?;
            let mappings = peer["consumerMappings"].as_array().ok_or_else(invalid)?;
            ensure!(
                peer["direction"] == "receive"
                    && one_of(
                        &peer["connectionState"],
                        &[
                            "new",
                            "connecting",
                            "connected",
                            "disconnected",
                            "failed",
                            "closed"
                        ]
                    )
                    && stats.len() <= 512
                    && stats.iter().any(|stat| stat["type"] == "transport")
                    && mappings.len() <= 256
                    && captures.insert(attempt, (entry, index + 1, peer)).is_none(),
                "invalid_stall_evidence"
            );
        }
        for attempt in completions.keys().chain(captures.keys()) {
            if !triggers.contains_key(attempt) {
                issues.insert("orphan_receiver_capture");
            }
        }
        for (attempt, (trigger, elapsed, identity, delivery, event_elapsed)) in triggers {
            let completion = completions.get(&attempt).copied();
            let capture = captures.get(&attempt);
            if let Some(completion) = completion {
                ensure!(
                    uint(&completion["details"]["triggerElapsedMs"]) == Some(elapsed)
                        && uint(&completion["elapsedMs"]).is_some_and(|time| time >= event_elapsed),
                    "invalid_stall_evidence"
                );
            }
            if let Some((entry, _, peer)) = capture {
                ensure!(
                    uint(&entry["details"]["triggerElapsedMs"]) == Some(elapsed)
                        && uint(&entry["elapsedMs"]).is_some_and(|time| time >= event_elapsed)
                        && keys(&entry["details"]["trigger"], TRIGGER)
                        && TRIGGER
                            .iter()
                            .all(|key| trigger[*key] == entry["details"]["trigger"][*key])
                        && peer["consumerMappings"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|mapping| mapping["ssrc"] == trigger["ssrc"])
                        && completion.is_none_or(|completion| uint(&entry["elapsedMs"])
                            <= uint(&completion["elapsedMs"])),
                    "invalid_stall_evidence"
                );
            }
            let status = completion
                .map(|event| &event["details"]["status"])
                .and_then(Value::as_str)
                .unwrap_or("missing");
            let mut issues = Vec::new();
            if status != "captured" {
                issues.push("receiver_capture_incomplete");
            }
            if status == "captured" && capture.is_none() {
                issues.push("receiver_snapshot_missing");
            }
            if status != "captured" && capture.is_some() {
                issues.push("receiver_capture_contradiction");
            }
            let mut trigger = trigger.clone();
            trigger
                .as_object_mut()
                .unwrap()
                .insert("elapsedMs".into(), elapsed.into());
            stalls.push(Stall {
                client: client_ordinal,
                attempt,
                consumer: uint(&trigger["consumerOrdinal"]).unwrap() as usize,
                trigger,
                identity,
                delivery,
                issues,
                capture: json!({"status":status,
                    "completedElapsedMs":completion.map(|event| &event["elapsedMs"]),
                    "snapshotOrdinal":capture.map(|(_, ordinal, _)| ordinal),
                    "peerState":capture.map(|(_, _, peer)| &peer["connectionState"])}),
            });
        }
    }
    Ok(Evidence { stalls, issues })
}
