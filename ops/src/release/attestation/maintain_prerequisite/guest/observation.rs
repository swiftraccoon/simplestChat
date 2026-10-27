//! Closed real observations and native forty-fact validation; no fabricated pass facts.
use super::{
    Error, Result,
    protocol::{self, Request},
};
use serde_json::{Value, json};
use std::collections::BTreeMap;
pub(super) const NAMES: [&str; 24] = [
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
pub(super) const BOOLEANS: [&str; 27] = [
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
pub(super) const HASHES: [&str; 6] = [
    "stdoutSha256",
    "stderrSha256",
    "opsSha256",
    "unshareSha256",
    "launcherSha256",
    "timeoutSha256",
];

pub(super) struct Observations {
    values: BTreeMap<String, bool>,
}
impl Observations {
    pub fn new() -> Self {
        Self {
            values: NAMES.iter().map(|v| ((*v).to_owned(), false)).collect(),
        }
    }
    pub fn set(&mut self, name: &str, value: bool) -> Result<()> {
        let target = self.values.get_mut(name).ok_or(Error::Input)?;
        *target = value;
        Ok(())
    }
    pub fn get(&self, name: &str) -> Result<bool> {
        self.values.get(name).copied().ok_or(Error::Input)
    }
    pub fn as_value(&self) -> Value {
        json!(self.values)
    }
    pub fn validate_success(&self, scenario: &str) -> Result<()> {
        for (name, value) in &self.values {
            let expected = match name.as_str() {
                "sourceTreeClean" => scenario != "dirty-source",
                "dirtyReadmeObserved" => scenario == "dirty-source",
                "freshLauncherTlsObserved"
                | "applicationReadyObserved"
                | "ownerAndLobbyObserved"
                | "backupHeaderAndDigestMatched" => scenario == "positive",
                "daemonEffectsUncertain" => false,
                _ => true,
            };
            if *value != expected {
                return Err(Error::Refused);
            }
        }
        Ok(())
    }
}
pub(super) fn native_frame(bytes: &[u8]) -> Result<Value> {
    let value = protocol::decode(bytes)?;
    protocol::exact(
        &value,
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
        || value["passed"].as_bool().is_none()
        || !allowed(
            &value["phase"],
            &[
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
            ],
        )
        || (!value["failure"].is_null()
            && !allowed(
                &value["failure"],
                &[
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
                ],
            ))
        || value["passed"].as_bool() != Some(value["failure"].is_null())
    {
        return Err(Error::Refused);
    }
    let facts = &value["facts"];
    let mut keys = BOOLEANS.to_vec();
    keys.extend(HASHES);
    keys.extend([
        "adoptedReaped",
        "adoptedRawStatuses",
        "wrapperRawStatus",
        "launcherRawStatus",
        "stdoutBytes",
        "stderrBytes",
        "launcherWorkElapsedNs",
    ]);
    protocol::exact(facts, &keys)?;
    if BOOLEANS.iter().any(|n| facts[n].as_bool().is_none())
        || HASHES.iter().any(|n| {
            !facts[n].is_null() && !facts[n].as_str().is_some_and(|v| protocol::digest(v, 64))
        })
        || ["stdoutBytes", "stderrBytes"]
            .iter()
            .any(|n| facts[n].as_u64().is_none())
    {
        return Err(Error::Refused);
    }
    let count = facts["adoptedReaped"]
        .as_u64()
        .filter(|v| *v <= u32::MAX.into())
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
            .is_none_or(|v| v > 1_050_000_000_000)
    {
        return Err(Error::Refused);
    }
    Ok(value)
}
fn allowed(value: &Value, names: &[&str]) -> bool {
    value.as_str().is_some_and(|v| names.contains(&v))
}
fn status(value: &Value) -> Result<i64> {
    value
        .as_i64()
        .filter(|v| (-127..=255).contains(v))
        .ok_or(Error::Refused)
}
pub(super) fn require_native(
    value: &Value,
    raw: i32,
    request: &Request,
    hashes: &BTreeMap<String, String>,
) -> Result<()> {
    let (expected, failure) = match request.scenario.as_str() {
        "positive" => (0, None),
        "dirty-source" => (1, Some("command")),
        "cancel-owned-launcher" => (143, Some("terminate")),
        "timeout-stopped-launcher" => (124, Some("deadline")),
        _ => return Err(Error::Input),
    };
    if raw != expected
        || value["trial"] != request.trial()
        || value["passed"].as_bool() != Some(raw == 0)
        || value["failure"].as_str() != failure
        || value["phase"] != if raw == 0 { "complete" } else { "result" }
    {
        return Err(Error::Refused);
    }
    let facts = &value["facts"];
    for name in HASHES {
        if !facts[name]
            .as_str()
            .is_some_and(|v| protocol::digest(v, 64))
        {
            return Err(Error::Refused);
        }
    }
    for name in ["stdoutBytes", "stderrBytes"] {
        if facts[name].as_u64().is_none_or(|v| v > 2 * 1024 * 1024) {
            return Err(Error::Refused);
        }
    }
    if facts["adoptedReaped"].as_u64().is_none_or(|v| v > 256)
        || status(&facts["wrapperRawStatus"])? != i64::from(expected)
    {
        return Err(Error::Refused);
    }
    let launcher = status(&facts["launcherRawStatus"])?;
    let elapsed = facts["launcherWorkElapsedNs"]
        .as_u64()
        .ok_or(Error::Refused)?;
    for name in BOOLEANS {
        let expected = match name {
            "termSeen" => request.scenario == "cancel-owned-launcher",
            "workCapExpired" => request.scenario == "timeout-stopped-launcher",
            "nestedClientObserved" | "launcherStopped" => raw == 143 || raw == 124,
            _ => true,
        };
        if facts[name].as_bool() != Some(expected) {
            return Err(Error::Refused);
        }
    }
    if (launcher == 0) != (request.scenario == "positive")
        || (request.scenario == "timeout-stopped-launcher" && elapsed < 900_000_000_000)
    {
        return Err(Error::Refused);
    }
    if hashes.len() != 4
        || [
            "opsSha256",
            "launcherSha256",
            "unshareSha256",
            "timeoutSha256",
        ]
        .iter()
        .any(|n| !hashes.contains_key(*n))
    {
        return Err(Error::Input);
    }
    for (name, hash) in hashes {
        if facts[name] != *hash {
            return Err(Error::Changed);
        }
    }
    Ok(())
}
