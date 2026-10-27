//! Closed passive guest metadata. It never grants executable or daemon authority.

use super::{Error, Result, catalog, options};
use serde_json::Value;

pub(super) fn exact(value: &Value, names: &[&str]) -> Result<()> {
    let object = value.as_object().ok_or(Error::Refused)?;
    if object.len() != names.len() || names.iter().any(|name| !object.contains_key(*name)) {
        return Err(Error::Refused);
    }
    Ok(())
}
fn allowed(value: &Value, names: &[&str]) -> bool {
    value.as_str().is_some_and(|value| names.contains(&value))
}
fn status(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .filter(|v| (-127..=255).contains(v))
        .ok_or(Error::Refused)
}
const BOOLEAN_FACTS: [&str; 27] = [
    "namespaceMapped",
    "pidfdMapped",
    "credentialsMatched",
    "privateGo",
    "deadlinesInherited",
    "launcherStarted",
    "nestedClientObserved",
    "launcherStopped",
    "termSeen",
    "workCapExpired",
    "leaderReaped",
    "echild",
    "stdoutEof",
    "stderrEof",
    "stdoutClosed",
    "stderrClosed",
    "capabilitiesClosed",
    "toolsUnchanged",
    "wrapperReaped",
    "wrapperNaturalExit",
    "initControlFlagsRestored",
    "runControlFlagsRestored",
    "chldRegistrationClosed",
    "wrapperStdoutEof",
    "wrapperStderrEof",
    "wrapperStdoutClosed",
    "wrapperStderrClosed",
];
const HASH_FACTS: [&str; 6] = [
    "stdoutSha256",
    "stderrSha256",
    "opsSha256",
    "unshareSha256",
    "launcherSha256",
    "timeoutSha256",
];
const OBSERVATIONS: [&str; 24] = [
    "sourceCommitMatched",
    "sourceShallowBoundaryVerified",
    "sourceRolesUnchanged",
    "sourceTreeClean",
    "dirtyReadmeObserved",
    "naturalTlsPrepared",
    "naturalMaterialsUnchanged",
    "freshLauncherTlsObserved",
    "applicationReadyObserved",
    "expectedDatabaseStateObserved",
    "ownerAndLobbyObserved",
    "backupHeaderAndDigestMatched",
    "workloadLockReleased",
    "freshActorInventoryJoined",
    "collectorMarkerJoined",
    "collectorFinalized",
    "observedExpectedDaemonJoins",
    "noObservedUnexpectedMutatingEvents",
    "externalGuardRetained",
    "noObservedExternalPackets",
    "knownFixtureActorsRetired",
    "knownFixtureNetworkRetired",
    "systemToolHashesJoined",
    "daemonEffectsUncertain",
];
const NATIVE_PHASES: [&str; 10] = [
    "admission",
    "sources",
    "ready",
    "go",
    "launcher",
    "client",
    "stopped",
    "settle",
    "result",
    "complete",
];
const NATIVE_FAILURES: [&str; 10] = [
    "input",
    "policy",
    "changed",
    "io",
    "close",
    "deadline",
    "interrupt",
    "terminate",
    "bounds",
    "command",
];
fn native(value: &Value) -> Result<()> {
    exact(
        value,
        &[
            "schemaVersion",
            "experiment",
            "trial",
            "passed",
            "phase",
            "failure",
            "facts",
        ],
    )?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["experiment"] != "maintain-launcher-prerequisite"
        || !allowed(
            &value["trial"],
            &[
                "normal",
                "cancel-owned-launcher",
                "timeout-stopped-launcher",
            ],
        )
        || !allowed(&value["phase"], &NATIVE_PHASES)
        || value["passed"].as_bool().is_none()
        || (!value["failure"].is_null() && !allowed(&value["failure"], &NATIVE_FAILURES))
        || value["passed"].as_bool() != Some(value["failure"].is_null())
    {
        return Err(Error::Refused);
    }
    let facts = &value["facts"];
    let mut fields = BOOLEAN_FACTS.to_vec();
    fields.extend(HASH_FACTS);
    fields.extend([
        "adoptedReaped",
        "adoptedRawStatuses",
        "wrapperRawStatus",
        "launcherRawStatus",
        "stdoutBytes",
        "stderrBytes",
        "launcherWorkElapsedNs",
    ]);
    exact(facts, &fields)?;
    for name in BOOLEAN_FACTS {
        if facts[name].as_bool().is_none() {
            return Err(Error::Refused);
        }
    }
    for name in HASH_FACTS {
        if !facts[name].is_null()
            && !facts[name]
                .as_str()
                .is_some_and(|text| options::digest(text, 64))
        {
            return Err(Error::Refused);
        }
    }
    for name in ["stdoutBytes", "stderrBytes"] {
        if facts[name].as_u64().is_none() {
            return Err(Error::Refused);
        }
    }
    let count = facts["adoptedReaped"]
        .as_u64()
        .filter(|n| *n <= u32::MAX.into())
        .ok_or(Error::Refused)?;
    let statuses = facts["adoptedRawStatuses"]
        .as_array()
        .ok_or(Error::Refused)?;
    if statuses.len() != count.min(256) as usize {
        return Err(Error::Refused);
    }
    for raw in statuses {
        status(raw)?;
    }
    for name in ["wrapperRawStatus", "launcherRawStatus"] {
        if !facts[name].is_null() {
            status(&facts[name])?;
        }
    }
    if !facts["launcherWorkElapsedNs"].is_null()
        && facts["launcherWorkElapsedNs"]
            .as_u64()
            .is_none_or(|v| v > 1050 * 1_000_000_000)
    {
        return Err(Error::Refused);
    }
    Ok(())
}
fn expected_native(value: &Value, raw: i64, scenario: &str) -> Result<()> {
    let (trial, expected, failure) = match scenario {
        "positive" => ("normal", 0, None),
        "dirty-source" => ("normal", 1, Some("command")),
        "cancel-owned-launcher" => ("cancel-owned-launcher", 143, Some("terminate")),
        "timeout-stopped-launcher" => ("timeout-stopped-launcher", 124, Some("deadline")),
        _ => return Err(Error::Input),
    };
    if raw != expected
        || value["trial"] != trial
        || value["passed"].as_bool() != Some(raw == 0)
        || value["failure"].as_str() != failure
        || value["phase"] != if raw == 0 { "complete" } else { "result" }
    {
        return Err(Error::Refused);
    }
    let facts = &value["facts"];
    for name in HASH_FACTS {
        if !facts[name]
            .as_str()
            .is_some_and(|text| options::digest(text, 64))
        {
            return Err(Error::Refused);
        }
    }
    for name in ["stdoutBytes", "stderrBytes"] {
        if facts[name].as_u64().is_none_or(|n| n > 2 * 1024 * 1024) {
            return Err(Error::Refused);
        }
    }
    if facts["adoptedReaped"].as_u64().is_none_or(|n| n > 256)
        || status(&facts["wrapperRawStatus"])? != expected
    {
        return Err(Error::Refused);
    }
    let launcher = status(&facts["launcherRawStatus"])?;
    let elapsed = facts["launcherWorkElapsedNs"]
        .as_u64()
        .ok_or(Error::Refused)?;
    for name in BOOLEAN_FACTS {
        let expected = match name {
            "termSeen" => scenario == "cancel-owned-launcher",
            "workCapExpired" => scenario == "timeout-stopped-launcher",
            "nestedClientObserved" | "launcherStopped" => expected == 143 || expected == 124,
            _ => true,
        };
        if facts[name].as_bool() != Some(expected) {
            return Err(Error::Refused);
        }
    }
    if (launcher == 0) != (scenario == "positive")
        || (scenario == "timeout-stopped-launcher" && elapsed < 900 * 1_000_000_000)
    {
        return Err(Error::Refused);
    }
    Ok(())
}
pub(super) fn guest(value: Value, expected: &Value) -> Result<Value> {
    exact(
        &value,
        &[
            "schemaVersion",
            "scope",
            "runId",
            "revision",
            "scenario",
            "passed",
            "failure",
            "checkpoint",
            "finalStatus",
            "cleanupPassed",
            "nativeRawExit",
            "nativeFrame",
            "observations",
            "sourcePins",
        ],
    )?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "MaintainLauncherPrerequisiteGuest"
        || value["passed"].as_bool().is_none()
        || value["cleanupPassed"].as_bool().is_none()
        || !allowed(
            &value["checkpoint"],
            &[
                "input",
                "source-import",
                "payload",
                "render",
                "database-setup",
                "caddy-natural",
                "ready",
                "native",
                "native-result",
                "daemon-observe",
                "daemon-actor-identity",
                "daemon-inventory",
                "daemon-validation-input",
                "daemon-event-shape",
                "daemon-actor-classification",
                "daemon-rename",
                "daemon-action-history",
                "daemon-network-history",
                "daemon-pg-history",
                "daemon-final-traces",
                "database-observe",
                "tls-observe",
                "application-observe",
                "source-observe",
                "capture-live",
                "capture-bounds",
                "capture-eof",
                "capture-stop",
                "capture-wait",
                "capture-status",
                "capture-short",
                "capture-close",
                "capture-data-arp",
                "capture-data-ipv4",
                "capture-data-ipv4-icmp",
                "capture-data-ipv4-igmp",
                "capture-data-ipv4-tcp",
                "capture-data-ipv4-udp",
                "capture-data-ipv4-dhcp",
                "capture-data-ipv4-ntp",
                "capture-data-ipv4-dns",
                "capture-data-ipv6",
                "capture-data-icmpv6-router-advertisement",
                "capture-data-other",
                "capture-data-unknown",
                "cleanup",
                "complete",
            ],
        )
        || (!value["failure"].is_null()
            && !allowed(
                &value["failure"],
                &["io", "deadline", "cancelled", "refused", "native-status"],
            ))
        || !value["finalStatus"]
            .as_u64()
            .is_some_and(|n| [0, 1, 124, 130, 143].contains(&n))
    {
        return Err(Error::Refused);
    }
    for name in ["runId", "revision", "scenario", "sourcePins"] {
        if value[name] != expected[name] {
            return Err(Error::Refused);
        }
    }
    if !value["nativeRawExit"].is_null() {
        status(&value["nativeRawExit"])?;
    }
    if !value["nativeFrame"].is_null() {
        native(&value["nativeFrame"])?;
    }
    exact(&value["observations"], &OBSERVATIONS)?;
    for name in OBSERVATIONS {
        if value["observations"][name].as_bool().is_none() {
            return Err(Error::Refused);
        }
    }
    if value["passed"] == true {
        if !value["failure"].is_null()
            || value["checkpoint"] != "complete"
            || value["finalStatus"].as_u64() != Some(0)
            || value["cleanupPassed"] != true
            || value["nativeFrame"].is_null()
        {
            return Err(Error::Refused);
        }
        let scenario = value["scenario"].as_str().ok_or(Error::Refused)?;
        let frame = &value["nativeFrame"];
        expected_native(frame, status(&value["nativeRawExit"])?, scenario)?;
        if frame["facts"]["opsSha256"] != expected["binary"]["sha256"]
            || frame["facts"]["launcherSha256"]
                != expected["sourcePins"]["ops/ansible/files/deploy-public.sh"]
        {
            return Err(Error::Refused);
        }
        for name in OBSERVATIONS {
            let wanted = match name {
                "freshLauncherTlsObserved"
                | "applicationReadyObserved"
                | "ownerAndLobbyObserved"
                | "backupHeaderAndDigestMatched" => scenario == "positive",
                "sourceTreeClean" => scenario != "dirty-source",
                "dirtyReadmeObserved" => scenario == "dirty-source",
                "daemonEffectsUncertain" => false,
                _ => true,
            };
            if value["observations"][name].as_bool() != Some(wanted) {
                return Err(Error::Refused);
            }
        }
    }
    Ok(value)
}
/// A closed guest observation is evidence even when it cannot complete the prerequisite.
pub(super) struct OpsReceipt {
    value: Value,
}
impl OpsReceipt {
    pub fn cases(&self) -> &Value {
        &self.value["cases"]
    }
    pub fn install(&self) -> &Value {
        &self.value["install"]
    }
    pub fn failure(&self) -> Option<&str> {
        self.value["failure"].as_str()
    }
    pub fn require_success(&self) -> Result<()> {
        if self.value["passed"] == true {
            Ok(())
        } else {
            Err(Error::Refused)
        }
    }
}

/// Admit the frozen keeper's bounded ordered prefix before enforcing positive completion.
pub(super) fn admit_ops(value: Value, run_id: &str, status: i32) -> Result<OpsReceipt> {
    exact(
        &value,
        &[
            "schemaVersion",
            "scope",
            "runId",
            "cases",
            "install",
            "passed",
            "failure",
        ],
    )?;
    let cases = value["cases"].as_array().ok_or(Error::Refused)?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != "OpsDebianGuestReceipt"
        || value["runId"] != run_id
        || value["passed"].as_bool().is_none()
        || cases.len() > catalog::OPS_CASES.len()
    {
        return Err(Error::Refused);
    }
    for (row, (name, expected)) in cases.iter().zip(catalog::OPS_CASES) {
        exact(row, &["name", "exitStatus", "passed"])?;
        if row["name"] != name
            || row["exitStatus"].as_u64().is_none_or(|status| status > 255)
            || row["passed"].as_bool().is_none()
            || (row["passed"] == true && row["exitStatus"].as_i64() != Some(expected))
        {
            return Err(Error::Refused);
        }
    }
    exact(&value["install"], &["firstChanged", "secondChanged"])?;
    if ["firstChanged", "secondChanged"].iter().any(|name| {
        let value = &value["install"][name];
        !value.is_null() && value.as_bool().is_none()
    }) {
        return Err(Error::Refused);
    }
    if value["passed"] == true {
        if status != 0
            || cases.len() != catalog::OPS_CASES.len()
            || cases.iter().any(|row| row["passed"] != true)
            || value["install"]["firstChanged"] != true
            || value["install"]["secondChanged"] != false
            || !value["failure"].is_null()
        {
            return Err(Error::Refused);
        }
    } else if status == 0
        || !allowed(
            &value["failure"],
            &[
                "guard",
                "transfer",
                "boot",
                "stage",
                "cases",
                "cleanup",
                "cancelled",
                "deadline",
                "unavailable",
            ],
        )
    {
        return Err(Error::Refused);
    }
    Ok(OpsReceipt { value })
}
pub(super) fn executable(value: &Value, sha256: &str, bytes: u64) -> Result<()> {
    let booleans = [
        "littleEndian",
        "noInterpreter",
        "noNeeded",
        "pieFlag",
        "bindNow",
        "nonExecutableStack",
        "noWritableExecutableLoad",
        "relroPresent",
        "noRpath",
        "noRunpath",
        "noTextRelocations",
        "noLoaderHooks",
    ];
    let mut names = booleans.to_vec();
    names.extend([
        "sha256",
        "bytes",
        "architecture",
        "elfClass",
        "fileType",
        "programHeaders",
        "neededCount",
    ]);
    exact(value, &names)?;
    if value["sha256"] != sha256
        || value["bytes"].as_u64() != Some(bytes)
        || value["architecture"] != "x86_64"
        || value["elfClass"].as_u64() != Some(64)
        || !allowed(&value["fileType"], &["executable", "dynamic"])
        || booleans.iter().any(|name| value[name].as_bool().is_none())
        || value["programHeaders"]
            .as_u64()
            .is_none_or(|n| n == 0 || n > 65535)
        || value["neededCount"].as_u64().is_none_or(|n| n > 65535)
    {
        return Err(Error::Refused);
    }
    for name in ["littleEndian", "noInterpreter", "noNeeded", "noLoaderHooks"] {
        if value[name] != true {
            return Err(Error::Refused);
        }
    }
    if value["neededCount"].as_u64() != Some(0) {
        return Err(Error::Refused);
    }
    Ok(())
}
