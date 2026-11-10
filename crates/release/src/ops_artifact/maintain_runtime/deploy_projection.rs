//! Closed deploy observations; decoded metadata never grants operation authority.
use super::deploy_capture::admit as failure_capture;
use super::{
    Error, Result, control::Control, decode, files::Original, options::digest, projection::exact,
};
use serde_json::Value;

fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Refused) }
}
fn hex(value: &Value, size: usize) -> bool {
    value.as_str().is_some_and(|text| digest(text, size))
}
fn image(value: &Value) -> bool {
    value
        .as_str()
        .and_then(|text| text.strip_prefix("sha256:"))
        .is_some_and(|text| digest(text, 64))
}
fn integer(value: &Value, min: u64, max: u64) -> bool {
    value
        .as_u64()
        .is_some_and(|number| (min..=max).contains(&number))
}
fn duration(value: &Value, maximum: f64) -> bool {
    value
        .as_f64()
        .is_some_and(|number| number.is_finite() && number >= 0.0 && number < maximum)
}
pub(super) fn same_migrations(
    current: &Original,
    previous: &Original,
    control: &mut Control,
) -> Result<()> {
    let current = decode(&current.data(1024 * 1024, control)?)?;
    let previous = decode(&previous.data(1024 * 1024, control)?)?;
    require(
        current["migrations"]
            .as_object()
            .is_some_and(|map| !map.is_empty())
            && current["migrations"] == previous["migrations"],
    )
}
fn identity(value: &Value, expected: &Value) -> Result<()> {
    require(value["schemaVersion"].as_u64() == Some(1))?;
    for name in ["runId", "revision", "previousRevision"] {
        require(value[name] == expected[name])?;
    }
    require(value["binarySha256"] == expected["binary"]["sha256"])
}
pub(super) fn prepared(value: Value, expected: &Value) -> Result<Value> {
    exact(
        &value,
        &[
            "schemaVersion",
            "kind",
            "runId",
            "revision",
            "previousRevision",
            "passed",
            "binarySha256",
            "firstImage",
            "dependencySha256",
            "migrationSha256",
        ],
    )?;
    identity(&value, expected)?;
    require(
        value["kind"] == "prepared"
            && value["passed"] == true
            && image(&value["firstImage"])
            && hex(&value["dependencySha256"], 64)
            && hex(&value["migrationSha256"], 64),
    )?;
    Ok(value)
}
fn trial(value: &Value, expected: &Value, negative: bool, current_image: &Value) -> Result<()> {
    exact(
        value,
        &[
            "case",
            "passed",
            "exitStatus",
            "rollbackAttempted",
            "rollbackPassed",
            "retainedBackup",
            "actorHistory",
            "selected",
            "injectedCandidateId",
            "manualStopObserved",
            "dependencyProfile",
            "seconds",
        ],
    )?;
    let case = if negative {
        "owned-candidate-stop-rollback"
    } else {
        "first-A-to-B"
    };
    let profile = if negative {
        "negative-inspection-response-barrier-v1"
    } else {
        "original-docker-cli"
    };
    require(
        value["case"] == case
            && value["passed"] == true
            && value["exitStatus"].as_i64() == Some(i64::from(negative))
            && value["rollbackAttempted"] == negative
            && value["rollbackPassed"] == negative
            && value["manualStopObserved"] == negative
            && value["dependencyProfile"] == profile,
    )?;
    require(if negative {
        hex(&value["injectedCandidateId"], 64)
    } else {
        value["injectedCandidateId"].is_null()
    })?;
    let backup = &value["retainedBackup"];
    exact(backup, &["bytes", "sha256", "listed", "restoreRehearsed"])?;
    require(
        integer(&backup["bytes"], 1, 4 * 1024 * 1024 * 1024)
            && hex(&backup["sha256"], 64)
            && backup["listed"] == true
            && backup["restoreRehearsed"] == false,
    )?;
    let actors = &value["actorHistory"];
    exact(
        actors,
        &[
            "actorIds",
            "validatorCount",
            "assetCaptureCount",
            "allCreatedStartedAsRequiredExitedZeroAndRemoved",
            "historyRows",
            "nativeIsolationInspected",
        ],
    )?;
    let ids = actors["actorIds"].as_array().ok_or(Error::Refused)?;
    let unique = ids
        .iter()
        .filter_map(Value::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    require(
        ids.len() == 4
            && unique.len() == 4
            && ids.iter().all(|value| hex(value, 64))
            && actors["validatorCount"].as_u64() == Some(2)
            && actors["assetCaptureCount"].as_u64() == Some(2)
            && actors["allCreatedStartedAsRequiredExitedZeroAndRemoved"] == true
            && actors["nativeIsolationInspected"] == false
            && integer(&actors["historyRows"], 1, 255),
    )?;
    exact(&value["selected"], &["revision", "image", "appId"])?;
    require(
        value["selected"]["revision"] == expected["revision"]
            && value["selected"]["image"] == *current_image
            && image(current_image)
            && hex(&value["selected"]["appId"], 64)
            && duration(&value["seconds"], 1920.0),
    )
}
const OBSERVATIONS: [&str; 7] = [
    "dependenciesUnchanged",
    "markerUnchanged",
    "seedAggregateUnchanged",
    "schemaUnchanged",
    "currentSelected",
    "actorHistoryComplete",
    "ownedInjectorStoppedCandidate",
];
pub(super) fn capture_failure(value: &Value, expected: &Value) -> Result<Option<Value>> {
    if value["failureCapture"].is_null() {
        return Ok(None);
    }
    identity(value, expected)?;
    require(value["kind"] == "result" && value["passed"] == false)?;
    failure_capture(&value["failureCapture"])?;
    Ok(Some(value["failureCapture"].clone()))
}
pub(super) fn guest(value: Value, expected: &Value, ready: Option<&Value>) -> Result<Value> {
    let mut fields = vec![
        "schemaVersion",
        "kind",
        "runId",
        "revision",
        "previousRevision",
        "passed",
        "cleanupPassed",
        "binarySha256",
        "firstImage",
        "currentImage",
        "trials",
        "nativeValidatorIsolationIndependentlyInspected",
        "phaseSeconds",
        "separateProbe",
        "failureCapture",
    ];
    fields.extend(OBSERVATIONS);
    exact(&value, &fields)?;
    identity(&value, expected)?;
    require(
        value["kind"] == "result"
            && value["passed"].as_bool().is_some()
            && value["cleanupPassed"].as_bool().is_some()
            && value["nativeValidatorIsolationIndependentlyInspected"] == false,
    )?;
    for name in OBSERVATIONS {
        require(value[name].as_bool().is_some())?;
    }
    for name in ["firstImage", "currentImage"] {
        require(value[name].is_null() || image(&value[name]))?;
    }
    if let Some(ready) = ready {
        require(value["firstImage"] == ready["firstImage"])?;
    }
    exact(&value["phaseSeconds"], &["prepare", "positive", "negative"])?;
    for (name, cap) in [
        ("prepare", 600.0),
        ("positive", 1920.0),
        ("negative", 1920.0),
    ] {
        require(
            value["phaseSeconds"][name].is_null() || duration(&value["phaseSeconds"][name], cap),
        )?;
    }
    let trials = value["trials"].as_array().ok_or(Error::Refused)?;
    require(trials.len() <= 2)?;
    for (ordinal, row) in trials.iter().enumerate() {
        trial(row, expected, ordinal == 1, &value["currentImage"])?;
    }
    if !value["failureCapture"].is_null() {
        failure_capture(&value["failureCapture"])?;
    }
    let probe = &value["separateProbe"];
    if !probe.is_null() {
        exact(
            probe,
            &[
                "passed",
                "isolationIndependentlyInspected",
                "migrationMapMatched",
                "exitStatus",
                "containerId",
            ],
        )?;
        require(
            probe["passed"] == true
                && probe["isolationIndependentlyInspected"] == true
                && probe["migrationMapMatched"] == true
                && probe["exitStatus"].as_i64() == Some(0)
                && hex(&probe["containerId"], 64),
        )?;
    }
    if value["passed"] == true {
        require(
            ready.is_some()
                && value["cleanupPassed"] == true
                && value["failureCapture"].is_null()
                && trials.len() == 2
                && image(&value["firstImage"])
                && image(&value["currentImage"])
                && value["firstImage"] != value["currentImage"]
                && OBSERVATIONS.iter().all(|name| value[*name] == true)
                && !probe.is_null()
                && ["prepare", "positive", "negative"]
                    .iter()
                    .all(|name| !value["phaseSeconds"][*name].is_null()),
        )?;
    } else {
        require(!value["failureCapture"].is_null())?;
    }
    Ok(value)
}
#[cfg(test)]
#[path = "deploy_projection_tests.rs"]
mod tests;
