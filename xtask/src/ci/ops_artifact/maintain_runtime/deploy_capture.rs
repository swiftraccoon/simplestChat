//! Passive first-failure capture with independent stdout/outcome evidence states.
use super::{Error, Result, projection::exact};
use serde_json::Value;
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
fn integer(value: &Value, min: u64, max: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|number| (min..=max).contains(&number))
}
fn allowed(value: &Value, values: &[&str]) -> bool {
    value.as_str().is_some_and(|text| values.contains(&text))
}
const STAGES: [&str; 17] = [
    "recovery-admission",
    "stop",
    "selection-compose-read",
    "selection-compose-write",
    "selection-env-read",
    "selection-env-write",
    "selection-images-read",
    "selection-images-write",
    "assets-select",
    "up",
    "application-inspect",
    "application-image",
    "loopback-readiness",
    "public-readiness",
    "database-continuity",
    "proxy-continuity",
    "journal-finalize",
];
const BOUNDARIES: [&str; 11] = [
    "operation",
    "originals-before",
    "role-ceiling",
    "stdout-admission",
    "stderr-admission",
    "executor",
    "settlement",
    "exit-status",
    "stderr-seal",
    "stdout-seal",
    "originals-after",
];
const CAUSES: [&str; 12] = [
    "input",
    "policy",
    "archive",
    "changed",
    "io",
    "close",
    "deadline",
    "interrupt",
    "terminate",
    "zip",
    "bounds",
    "command",
];
pub(super) fn rollback_failure(value: &Value) -> Result<()> {
    exact(value, &["stage", "boundary", "cause", "logOrdinal"])?;
    require(
        allowed(&value["stage"], &STAGES)
            && allowed(&value["boundary"], &BOUNDARIES)
            && allowed(&value["cause"], &CAUSES)
            && (value["logOrdinal"].is_null()
                || value["logOrdinal"]
                    .as_u64()
                    .is_some_and(|number| number > 0)),
    )
}
const FAILURE_LABELS: &[&str] = &[
    "native-input",
    "native-policy",
    "native-archive",
    "native-input-changed",
    "filesystem",
    "native-close",
    "deadline",
    "interrupted",
    "output-limit",
    "release-error",
    "runtime-profile",
    "asset-tar-path",
    "asset-error",
    "readiness",
    "backup-headroom",
    "empty-backup",
    "running-config",
    "browser-proxy-config",
    "deployed-identity",
    "same-image",
    "public-origin",
    "database-health",
    "schema-change",
    "candidate-migrations",
    "migration-container",
    "selected-image",
    "asset_capture_count",
    "asset_capture_file",
    "asset_capture_revision",
    "asset_command_owner",
    "asset_generation_count",
    "asset_generation_fields",
    "asset_generation_files",
    "asset_immutable_collision",
    "asset_index_duplicate",
    "asset_index_generations",
    "asset_index_schema",
    "asset_index_size",
    "asset_index_total",
    "asset_lifetime",
    "asset_name",
    "asset_operation_failed",
    "asset_public_files",
    "asset_public_pointer",
    "asset_public_target",
    "asset_record",
    "asset_record_fields",
    "asset_required_capacity",
    "asset_revision",
    "asset_revision_changed",
    "asset_selected_revision",
    "asset_store_digest",
    "asset_store_file_size",
    "asset_store_foreign",
    "asset_store_ownership",
    "asset_store_unindexed",
    "asset_tar_changed",
    "asset_tar_content",
    "asset_tar_content_size",
    "asset_tar_count",
    "asset_tar_duplicate",
    "asset_tar_empty",
    "asset_tar_extension",
    "asset_tar_file_size",
    "asset_tar_header_count",
    "asset_tar_names",
    "asset_tar_path",
    "asset_tar_root",
    "asset_tar_size",
    "asset_tar_total",
    "asset_tar_type",
    "asset_tar_unsupported",
    "asset_union_digest",
    "asset_union_files",
    "asset_union_size",
    "asset_version_schema",
    "asset_version_size",
    "asset_workload_busy",
    "native-command",
    "other-refusal",
];
const CHECKPOINTS: [&str; 16] = [
    "input",
    "sources",
    "payload",
    "configuration",
    "network",
    "database",
    "tls",
    "baseline",
    "prepared",
    "positive",
    "negative",
    "injection",
    "observation",
    "probe",
    "cleanup",
    "complete",
];
// The guest owns this fixed vocabulary; this shared admission is deliberately closed.
pub(super) fn admit(value: &Value) -> Result<()> {
    exact(
        value,
        &[
            "checkpoint",
            "failure",
            "trial",
            "nativeRawExit",
            "stdoutState",
            "outcomeState",
            "nativePassed",
            "rollbackAttempted",
            "rollbackPassed",
            "rollbackFailure",
            "outcomeRollbackFailure",
            "nativePhase",
            "nativeFailureLabel",
            "nativeCommandOrdinal",
        ],
    )?;
    require(
        allowed(&value["checkpoint"], &CHECKPOINTS)
            && allowed(
                &value["failure"],
                &[
                    "input",
                    "refused",
                    "changed",
                    "io",
                    "deadline",
                    "cancelled",
                    "command",
                    "bounds",
                    "native-status",
                ],
            ),
    )?;
    require(
        value["nativePhase"].is_null()
            || allowed(
                &value["nativePhase"],
                &["stage", "preflight", "replace_application", "complete"],
            ),
    )?;
    require(
        value["nativeFailureLabel"].is_null()
            || allowed(&value["nativeFailureLabel"], FAILURE_LABELS),
    )?;
    require(
        value["nativeCommandOrdinal"].is_null() || integer(&value["nativeCommandOrdinal"], 1, 192),
    )?;
    require(
        value["nativeCommandOrdinal"].is_null() || value["nativeFailureLabel"] == "native-command",
    )?;
    if value["stdoutState"] != "decoded" {
        require(
            [
                "nativePassed",
                "rollbackAttempted",
                "rollbackPassed",
                "nativePhase",
                "nativeFailureLabel",
                "nativeCommandOrdinal",
                "rollbackFailure",
            ]
            .iter()
            .all(|name| value[*name].is_null()),
        )?;
    }
    require(value["trial"].is_null() || allowed(&value["trial"], &["positive", "negative"]))?;
    require(
        value["nativeRawExit"].is_null()
            || value["nativeRawExit"]
                .as_i64()
                .is_some_and(|number| (-255..=255).contains(&number)),
    )?;
    for name in ["stdoutState", "outcomeState"] {
        require(allowed(
            &value[name],
            &["not-observed", "invalid", "decoded"],
        ))?;
    }
    for name in ["nativePassed", "rollbackAttempted", "rollbackPassed"] {
        require(value[name].is_null() || value[name].as_bool().is_some())?;
    }
    for (name, state) in [
        ("rollbackFailure", "stdoutState"),
        ("outcomeRollbackFailure", "outcomeState"),
    ] {
        if !value[name].is_null() {
            rollback_failure(&value[name])?;
            require(value[state] == "decoded" && !value["trial"].is_null())?;
            if name == "rollbackFailure" {
                require(
                    value["nativePassed"] == false
                        && value["rollbackAttempted"] == true
                        && value["rollbackPassed"] == false
                        && value["nativePhase"] == "replace_application",
                )?;
            }
        }
    }
    Ok(())
}
