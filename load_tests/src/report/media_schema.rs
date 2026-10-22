//! Legacy/current snapshot validation before any native value enters a report.

use anyhow::{Result, ensure};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

pub(super) const MAX_SAFE: u64 = 9_007_199_254_740_991;

pub(super) fn uint(value: &Value) -> Option<u64> {
    let value = value.as_f64()?;
    (value.is_finite() && value >= 0.0 && value <= MAX_SAFE as f64 && value.fract() == 0.0)
        .then_some(value as u64)
}

pub(super) fn keys(value: &Value, names: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == names.len() && names.iter().all(|name| object.contains_key(*name))
    })
}

pub(super) fn hex(value: &Value, size: usize) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == size
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

pub(super) fn uuid(value: &Value) -> bool {
    value.as_str().is_some_and(|value| {
        value.len() == 36
            && value.bytes().enumerate().all(|(index, byte)| {
                if [8, 13, 18, 23].contains(&index) {
                    byte == b'-'
                } else {
                    byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                }
            })
    })
}

pub(super) fn one_of(value: &Value, choices: &[&str]) -> bool {
    value.as_str().is_some_and(|value| choices.contains(&value))
}

fn nullable(value: &Value, predicate: impl FnOnce(&Value) -> bool) -> bool {
    value.is_null() || predicate(value)
}

pub(super) fn reference(salt: &str, kind: &str, identity: &str) -> String {
    // Callers establish the exact lowercase hexadecimal salt and UUID syntax.
    let salt = ::hex::decode(salt).expect("validated correlation salt");
    let mut hash = Sha256::new();
    hash.update(b"simplestchat-media-v1\0");
    hash.update(salt);
    hash.update(kind.as_bytes());
    hash.update(b"\0");
    hash.update(identity.as_bytes());
    ::hex::encode(hash.finalize())
}

/// Count JSON.stringify's UTF-8 bytes without retaining private serialized
/// snapshots. Key order does not affect length. ECMAScript number formatting
/// matters at the exact 256 KiB evidence bound (1e-6 is six fractional places).
pub(super) fn json_bytes(value: &Value, maximum: usize) -> usize {
    let string_bytes = |value: &str| {
        serde_json::to_vec(value)
            .expect("JSON strings always serialize")
            .len()
    };
    let count = match value {
        Value::Null => 4,
        Value::Bool(true) => 4,
        Value::Bool(false) => 5,
        Value::Number(value) => match value.as_f64().filter(|value| value.is_finite()) {
            Some(value) => ryu_js::Buffer::new().format(value).len(),
            None => 4, // JSON.stringify emits null for either infinity.
        },
        Value::String(value) => string_bytes(value),
        Value::Array(values) => {
            let mut size = 2 + values.len().saturating_sub(1);
            for value in values {
                size = size.saturating_add(json_bytes(value, maximum));
                if size > maximum {
                    break;
                }
            }
            size
        }
        Value::Object(values) => {
            let mut size = 2 + values.len().saturating_sub(1);
            for (key, value) in values {
                size = size
                    .saturating_add(string_bytes(key) + 1)
                    .saturating_add(json_bytes(value, maximum));
                if size > maximum {
                    break;
                }
            }
            size
        }
    };
    count.min(maximum.saturating_add(1))
}

/// Both schema versions are accepted, with exactly their documented fields.
/// Bounds use JavaScript safe integers because retained collectors use Node.
pub(super) fn snapshot(value: &Value) -> Result<()> {
    ensure!(
        keys(
            value,
            &[
                "schemaVersion",
                "correlationSalt",
                "sampleId",
                "startedUs",
                "finishedUs",
                "coverage",
                "entities"
            ]
        ) && [Some(1), Some(2)].contains(&uint(&value["schemaVersion"]))
            && hex(&value["correlationSalt"], 32)
            && uint(&value["sampleId"]).is_some_and(|id| id > 0),
        "invalid_media_schema"
    );
    let start = uint(&value["startedUs"]).ok_or_else(invalid)?;
    let finish = uint(&value["finishedUs"]).ok_or_else(invalid)?;
    ensure!(finish >= start, "invalid_media_schema");
    let coverage = &value["coverage"];
    let boolean_fields = [
        "complete",
        "registryBusy",
        "participantLimitReached",
        "entityLimitReached",
        "deadlineReached",
    ];
    ensure!(
        keys(
            coverage,
            &[
                "complete",
                "registryBusy",
                "participantsObserved",
                "participantsVisited",
                "busyParticipants",
                "participantLimitReached",
                "entityLimitReached",
                "deadlineReached"
            ]
        ) && boolean_fields.iter().all(|key| coverage[*key].is_boolean()),
        "invalid_media_schema"
    );
    let observed = uint(&coverage["participantsObserved"]).ok_or_else(invalid)?;
    let visited = uint(&coverage["participantsVisited"]).ok_or_else(invalid)?;
    let busy = uint(&coverage["busyParticipants"]).ok_or_else(invalid)?;
    ensure!(
        visited <= 64 && busy <= 64 && visited + busy <= observed.min(64),
        "invalid_media_schema"
    );
    let entities = value["entities"].as_array().ok_or_else(invalid)?;
    ensure!(entities.len() <= 1024, "invalid_media_schema");
    let mut identities = BTreeSet::new();
    for entity in entities {
        ensure!(
            keys(
                entity,
                &[
                    "entityType",
                    "reference",
                    "transportReference",
                    "producerReference",
                    "kind",
                    "status",
                    "paused",
                    "producerPaused",
                    "observedUs",
                    "streams"
                ]
            ) && one_of(&entity["entityType"], &["consumer", "producer"])
                && hex(&entity["reference"], 64)
                && nullable(&entity["transportReference"], |value| hex(value, 64))
                && nullable(&entity["producerReference"], |value| hex(value, 64))
                && one_of(&entity["kind"], &["audio", "video"])
                && one_of(
                    &entity["status"],
                    &[
                        "ok",
                        "closed",
                        "timeout",
                        "error",
                        "no_streams",
                        "stream_limit",
                        "not_collected"
                    ]
                )
                && nullable(&entity["paused"], Value::is_boolean)
                && nullable(&entity["producerPaused"], Value::is_boolean),
            "invalid_media_schema"
        );
        let observed = uint(&entity["observedUs"]).ok_or_else(invalid)?;
        ensure!((start..=finish).contains(&observed), "invalid_media_schema");
        ensure!(
            identities.insert((entity["entityType"].as_str(), entity["reference"].as_str())),
            "invalid_media_schema"
        );
        let streams = entity["streams"].as_array().ok_or_else(invalid)?;
        ensure!(
            streams.len() <= 16 && (entity["status"] != "ok" || !streams.is_empty()),
            "invalid_media_schema"
        );
        let mut ssrcs = BTreeSet::new();
        let mut encodings = BTreeSet::new();
        for stream in streams {
            let mut fields = vec!["ssrc", "packetCount", "rtpBytes", "workerTimestampMs"];
            if value["schemaVersion"] == 2 {
                fields.extend(["score", "encodingIndex"]);
            }
            ensure!(
                keys(stream, &fields)
                    && fields[..4].iter().all(|key| uint(&stream[*key]).is_some()),
                "invalid_media_schema"
            );
            let ssrc = uint(&stream["ssrc"]).ok_or_else(invalid)?;
            ensure!(
                ssrc <= u32::MAX.into() && ssrcs.insert(ssrc),
                "invalid_media_schema"
            );
            if value["schemaVersion"] == 2 {
                ensure!(
                    uint(&stream["score"]).is_some_and(|score| score <= 10)
                        && nullable(&stream["encodingIndex"], |value| uint(value)
                            .is_some_and(|index| index <= 255))
                        && (entity["entityType"] != "consumer"
                            || stream["encodingIndex"].is_null()),
                    "invalid_media_schema"
                );
                if let Some(index) = uint(&stream["encodingIndex"]) {
                    ensure!(encodings.insert(index), "invalid_media_schema");
                }
            }
        }
    }
    ensure!(
        coverage["complete"] != true
            || (boolean_fields[1..]
                .iter()
                .all(|key| coverage[*key] == false)
                && busy == 0
                && visited == observed
                && entities.iter().all(|entity| entity["status"] == "ok")),
        "invalid_media_schema"
    );
    Ok(())
}

fn invalid() -> anyhow::Error {
    anyhow::anyhow!("invalid_media_schema")
}
