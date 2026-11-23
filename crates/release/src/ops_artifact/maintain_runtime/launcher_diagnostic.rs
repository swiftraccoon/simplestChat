//! Closed launcher PostgreSQL-history refusal record. It grants no event or
//! execution authority.

use super::{Error, PG_REASONS, Result, exact};
use serde_json::Value;

const STAGE: &str = "daemon-pg-history";
const SCOPE: &str = "MaintainLauncherPgHistoryDiagnostic";
const EVENTS: usize = 8;
/// The guest collector's row bound, at which the record's counts saturate.
const ROWS: u64 = 2048;
pub(in super::super) const ACTIONS: [&str; 28] = [
    "attach",
    "commit",
    "copy",
    "create",
    "destroy",
    "detach",
    "die",
    "exec_create",
    "exec_detach",
    "exec_die",
    "exec_start",
    "export",
    "health_status",
    "kill",
    "oom",
    "pause",
    "rename",
    "resize",
    "restart",
    "start",
    "stop",
    "top",
    "unpause",
    "update",
    "prune",
    "archive-path",
    "extract-to-dir",
    "other",
];
pub(in super::super) const COMMANDS: [&str; 3] = ["health", "launcher", "unlisted"];
pub(in super::super) const ACTORS: [&str; 3] = ["postgres", "fixture", "unknown"];
pub(in super::super) const WINDOWS: [&str; 4] = ["before", "inside", "after", "unobserved"];
pub(in super::super) const FAULTS: [&str; 7] = [
    "missing",
    "unadmitted",
    "unlisted-command",
    "duplicate",
    "out-of-order",
    "nonzero-exit",
    "surplus",
];

fn one_of(value: &Value, names: &[&str]) -> bool {
    value.as_str().is_some_and(|value| names.contains(&value))
}

/// Admit a launcher receipt's `pgHistoryDiagnostic`: `null`, or, only on a
/// receipt that failed `refused` at its first failed checkpoint
/// `daemon-pg-history` (so never on a passed one), one closed record. Its
/// fixed keys, closed strings, bounded counts and at most eight events keep it
/// under two kilobytes. Any other value, shape, vocabulary or count is refused.
pub(super) fn validate(value: &Value, checkpoint: &Value, failure: &Value) -> Result<()> {
    if value.is_null() {
        return Ok(());
    }
    if checkpoint != STAGE || failure != "refused" {
        return Err(Error::Refused);
    }
    exact(
        value,
        &[
            "schemaVersion",
            "scope",
            "reason",
            "postgresEvents",
            "events",
            "withheldEvents",
        ],
    )?;
    let events = value["events"].as_array().ok_or(Error::Refused)?;
    let withheld = value["withheldEvents"].as_u64().ok_or(Error::Refused)?;
    if value["schemaVersion"].as_u64() != Some(1)
        || value["scope"] != SCOPE
        || !(value["reason"].is_null() || one_of(&value["reason"], &PG_REASONS))
        || !value["postgresEvents"]
            .as_u64()
            .is_some_and(|count| count <= ROWS)
        || events.len() > EVENTS
        || withheld > ROWS
        || (withheld > 0 && events.len() < EVENTS)
    {
        return Err(Error::Refused);
    }
    events.iter().try_for_each(event)
}

/// One described event: a missing one is never inside the window, and only a
/// nonzero exit carries its exit code, 1 to 255.
fn event(value: &Value) -> Result<()> {
    exact(
        value,
        &[
            "type", "action", "command", "actor", "window", "fault", "exitCode",
        ],
    )?;
    let exit = &value["exitCode"];
    if value["type"] != "container"
        || !one_of(&value["action"], &ACTIONS)
        || !(value["command"].is_null() || one_of(&value["command"], &COMMANDS))
        || !one_of(&value["actor"], &ACTORS)
        || !one_of(&value["window"], &WINDOWS)
        || !one_of(&value["fault"], &FAULTS)
        || (value["window"] == "inside") == (value["fault"] == "missing")
        || !(exit.is_null()
            || value["fault"] == "nonzero-exit"
                && exit.as_u64().is_some_and(|code| (1..=255).contains(&code)))
    {
        return Err(Error::Refused);
    }
    Ok(())
}
