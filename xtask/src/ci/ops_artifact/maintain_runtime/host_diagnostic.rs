//! Bounded passive failure detail. It grants no event or execution authority.

use super::{Error, Result, options, projection};
use serde_json::Value;

const BASELINE_STAGE: &str = "daemon-pg-history";
pub(super) const STAGES: [&str; 18] = [
    "host-none-network",
    "host-attempt-delta",
    "host-shell-outcome",
    "host-shell-validation",
    "host-event-history",
    "host-event-input",
    "host-event-shape",
    "host-event-image",
    "host-event-container",
    "host-event-network",
    "host-event-actor-counts",
    "host-event-actor-traces",
    "host-event-network-traces",
    "host-event-pg",
    "host-event-interruption-order",
    "host-event-proxy-validation",
    "host-event-proxy-replacement",
    BASELINE_STAGE,
];
const CLASSES: [&str; 7] = [
    "shape",
    "identity",
    "ordering",
    "cardinality",
    "unknown-command",
    "nonzero-exit",
    "incomplete-sequence",
];
const BASELINE_CLASSES: [&str; 15] = [
    "pg-source-missing",
    "pg-source-utf8",
    "pg-query-shape",
    "pg-query-count",
    "pg-action-shape",
    "pg-exec-id",
    "pg-duplicate-create",
    "pg-unknown-command",
    "pg-missing-create",
    "pg-start-order",
    "pg-die-action",
    "pg-die-order",
    "pg-nonzero-exit",
    "pg-incomplete-sequence",
    "pg-command-count",
];
const MAXIMUM: usize = 1024;
const ROWS: u64 = 2048;

fn classes(stage: &str) -> Option<&'static [&'static str]> {
    if stage == BASELINE_STAGE {
        Some(&BASELINE_CLASSES)
    } else if STAGES.contains(&stage) {
        Some(&CLASSES)
    } else {
        None
    }
}

pub(super) fn validate(value: &Value, positive: bool, checkpoint: &str) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    if positive || serde_json::to_vec(value).map_err(|_| Error::Refused)?.len() > MAXIMUM {
        return Err(Error::Refused);
    }
    projection::exact(
        value,
        &[
            "schemaVersion",
            "scope",
            "stage",
            "failureClass",
            "rowIndex",
            "rows",
            "expectedCommandSha256",
            "observedCommandSha256",
            "expectedCount",
            "observedCount",
        ],
    )?;
    let rows = value["rows"].as_u64().ok_or(Error::Refused)?;
    let stage = value["stage"].as_str().ok_or(Error::Refused)?;
    let class = value["failureClass"].as_str().ok_or(Error::Refused)?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "PreparedHostObservationDiagnostic"
        || stage != checkpoint
        || !classes(stage).is_some_and(|allowed| allowed.contains(&class))
        || rows > ROWS
        || (!value["rowIndex"].is_null()
            && !value["rowIndex"].as_u64().is_some_and(|index| index < rows))
    {
        return Err(Error::Refused);
    }
    for name in ["expectedCommandSha256", "observedCommandSha256"] {
        if !value[name].is_null()
            && !value[name]
                .as_str()
                .is_some_and(|hash| options::digest(hash, 64))
        {
            return Err(Error::Refused);
        }
    }
    for name in ["expectedCount", "observedCount"] {
        if !value[name].is_null() && !value[name].as_u64().is_some_and(|count| count <= ROWS) {
            return Err(Error::Refused);
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) fn fixture() -> Value {
    use serde_json::json;
    // The longest admitted strings, nonnull hashes, and maximum row/count
    // values exercise the complete diagnostic's serialized upper boundary.
    let (stage, class) = STAGES
        .iter()
        .flat_map(|stage| {
            classes(stage)
                .unwrap()
                .iter()
                .map(move |class| (*stage, *class))
        })
        .max_by_key(|(stage, class)| stage.len() + class.len())
        .unwrap();
    json!({"schemaVersion":1,"scope":"PreparedHostObservationDiagnostic",
        "stage":stage,"failureClass":class,"rowIndex":2047,"rows":2048,
        "expectedCommandSha256":"a".repeat(64),"observedCommandSha256":"b".repeat(64),
        "expectedCount":2048,"observedCount":2048})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // These synthetic diagnostic values test decoding boundaries only. They
    // contain no captured Docker history or operational runtime evidence.
    #[test]
    fn diagnostics_are_negative_only_closed_and_bound_to_the_first_failure_checkpoint() {
        let value = fixture();
        let checkpoint = value["stage"].as_str().unwrap();
        validate(&Value::Null, true, "complete").unwrap();
        validate(&value, false, checkpoint).unwrap();
        assert!(validate(&value, true, checkpoint).is_err());
        assert!(validate(&value, false, "host-cleanup").is_err());
        for stage in STAGES {
            if stage == BASELINE_STAGE {
                continue;
            }
            let mut changed = value.clone();
            changed["stage"] = stage.into();
            validate(&changed, false, stage).unwrap();
        }
        for class in CLASSES {
            let mut changed = value.clone();
            changed["failureClass"] = class.into();
            validate(&changed, false, checkpoint).unwrap();
        }
        for (name, replacement) in [
            ("schemaVersion", json!(true)),
            ("schemaVersion", json!(1.0)),
            ("scope", json!("UnboundObservation")),
            ("stage", json!("host-event-neighbor")),
            ("stage", json!("host-event-pg")),
            ("failureClass", json!("arbitrary-error-text")),
            ("rawRows", json!([])),
        ] {
            let mut changed = value.clone();
            changed[name] = replacement;
            assert!(validate(&changed, false, checkpoint).is_err(), "{name}");
        }
        for name in value.as_object().unwrap().keys() {
            let mut changed = value.clone();
            changed.as_object_mut().unwrap().remove(name);
            assert!(validate(&changed, false, checkpoint).is_err(), "{name}");
        }
    }

    #[test]
    fn baseline_classes_are_closed_and_cannot_cross_the_original_stage_domain() {
        for stage in STAGES {
            for class in CLASSES {
                let mut value = fixture();
                value["stage"] = stage.into();
                value["failureClass"] = class.into();
                assert_eq!(
                    validate(&value, false, stage).is_ok(),
                    stage != BASELINE_STAGE
                );
            }
            for class in BASELINE_CLASSES {
                let mut value = fixture();
                value["stage"] = stage.into();
                value["failureClass"] = class.into();
                assert_eq!(
                    validate(&value, false, stage).is_ok(),
                    stage == BASELINE_STAGE
                );
                assert!(validate(&value, true, stage).is_err());
                assert!(validate(&value, false, "host-cleanup").is_err());
            }
        }
        for class in [
            "pg-command-count-neighbor",
            "pg-unknown-command\nPRIVATE",
            "pg-source-missing\0",
            "unknown-command",
        ] {
            let mut value = fixture();
            value["stage"] = BASELINE_STAGE.into();
            value["failureClass"] = class.into();
            assert!(validate(&value, false, BASELINE_STAGE).is_err());
        }
    }

    #[test]
    fn baseline_source_failure_preserves_null_and_zero_without_exporting_source_bytes() {
        let mut value = fixture();
        value["stage"] = BASELINE_STAGE.into();
        value["failureClass"] = "pg-source-missing".into();
        value["rows"] = 0.into();
        for name in [
            "rowIndex",
            "expectedCommandSha256",
            "observedCommandSha256",
            "expectedCount",
        ] {
            value[name] = Value::Null;
        }
        value["observedCount"] = 0.into();
        validate(&value, false, BASELINE_STAGE).unwrap();
        assert_eq!(value.as_object().unwrap().len(), 10);
        assert!(value["expectedCount"].is_null());
        assert_eq!(value["observedCount"].as_u64(), Some(0));
        assert!(validate(&value, true, "complete").is_err());
        assert!(validate(&value, false, "host-event-pg").is_err());
        let mut changed = value.clone();
        changed["rowIndex"] = 0.into();
        assert!(validate(&changed, false, BASELINE_STAGE).is_err());
        for (name, extra) in [("rawRows", json!([])), ("source", json!("PRIVATE"))] {
            let mut changed = value.clone();
            changed[name] = extra;
            assert!(validate(&changed, false, BASELINE_STAGE).is_err());
        }
    }

    #[test]
    fn row_indices_and_counts_require_bounded_integers_and_keep_null_distinct_from_zero() {
        let value = fixture();
        let checkpoint = value["stage"].as_str().unwrap();
        for name in ["rows", "rowIndex", "expectedCount", "observedCount"] {
            for replacement in [json!(true), json!(-1), json!(1.0), json!(2049)] {
                let mut changed = value.clone();
                changed[name] = replacement;
                assert!(validate(&changed, false, checkpoint).is_err(), "{name}");
            }
        }
        let mut changed = value.clone();
        changed["rowIndex"] = 2048.into();
        assert!(validate(&changed, false, checkpoint).is_err());
        changed["rows"] = 0.into();
        changed["rowIndex"] = Value::Null;
        validate(&changed, false, checkpoint).unwrap();
        changed["rowIndex"] = 0.into();
        assert!(validate(&changed, false, checkpoint).is_err());
        changed["rows"] = 1.into();
        validate(&changed, false, checkpoint).unwrap();
        changed["rowIndex"] = 1.into();
        assert!(validate(&changed, false, checkpoint).is_err());
        for name in ["rowIndex", "expectedCount", "observedCount"] {
            let mut changed = value.clone();
            changed[name] = Value::Null;
            validate(&changed, false, checkpoint).unwrap();
        }
        let mut changed = value.clone();
        changed["rows"] = Value::Null;
        assert!(validate(&changed, false, checkpoint).is_err());
    }

    #[test]
    fn command_hashes_never_accept_raw_command_text_or_neighbor_shapes() {
        let value = fixture();
        let checkpoint = value["stage"].as_str().unwrap();
        for name in ["expectedCommandSha256", "observedCommandSha256"] {
            for replacement in [
                json!("SELECT private_fixture_data"),
                json!("a".repeat(63)),
                json!("a".repeat(65)),
                json!("A".repeat(64)),
                json!(true),
                json!(64),
            ] {
                let mut changed = value.clone();
                changed[name] = replacement;
                assert!(validate(&changed, false, checkpoint).is_err(), "{name}");
            }
            let mut changed = value.clone();
            changed[name] = Value::Null;
            validate(&changed, false, checkpoint).unwrap();
        }
    }

    #[test]
    fn serialized_diagnostics_have_a_separate_bound_before_wire_projection() {
        let value = fixture();
        let checkpoint = value["stage"].as_str().unwrap();
        let maximum_bytes = serde_json::to_vec(&value).unwrap().len();
        validate(&value, false, checkpoint).unwrap();
        assert!(maximum_bytes <= MAXIMUM);
        for stage in STAGES {
            for class in classes(stage).unwrap() {
                let mut admitted = value.clone();
                admitted["stage"] = stage.into();
                admitted["failureClass"] = (*class).into();
                validate(&admitted, false, stage).unwrap();
                assert!(serde_json::to_vec(&admitted).unwrap().len() <= maximum_bytes);
            }
        }
        let mut oversized = value.clone();
        oversized["observedCommandSha256"] = "a".repeat(MAXIMUM).into();
        assert!(serde_json::to_vec(&oversized).unwrap().len() > MAXIMUM);
        assert!(validate(&oversized, false, checkpoint).is_err());
    }
}
