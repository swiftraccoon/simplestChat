//! Fixed public results; input frames, logs and arbitrary dependency errors are private.

use super::config::Ports;
use crate::bench::process::Exit;
use serde::Serialize;
use serde_json::{Value, json};

pub(super) const RECORDER_BYTES: u64 = ((crate::report::diagnostics::MAX_RECORDS + 1)
    * (crate::report::diagnostics::LINE_BYTES + 1)) as u64;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Shutdown {
    pub ready: bool,
    pub peer_close_code: u16,
    pub joined: bool,
    pub server_restarting: bool,
    pub close_code: u16,
    pub exit_code: i32,
    pub shutdown_ms: f64,
    pub ports: Ports,
}
pub(super) fn diagnostic(
    shutdown: Option<&Shutdown>,
    exit: Option<&Exit>,
    report: Option<&Value>,
    issues: &[&'static str],
    output: &std::path::Path,
    workload: bool,
) -> Value {
    let coverage = report.map(|v| &v["coverage"]);
    let complete = coverage.is_some_and(|v| v["complete"] == true);
    let join = report
        .and_then(|v| v["operations"].as_array())
        .is_some_and(|rows| {
            rows.iter().any(|v| {
                v["operation"] == "join_room"
                    && v["outcome"] == "ok"
                    && v["count"].as_u64().is_some_and(|n| n > 0)
            })
        });
    let lock = report
        .and_then(|v| v["stages"].as_array())
        .is_some_and(|rows| {
            rows.iter().any(|v| {
                v["operation"] == "join_room"
                    && v["stage"] == "room_lock_wait"
                    && matches!(v["outcome"].as_str(), Some("ok" | "completed"))
                    && v["count"].as_u64().is_some_and(|n| n > 0)
            })
        });
    let server = exit.is_some_and(Exit::success);
    json!({"schemaVersion":1,"passed":workload&&server&&complete&&join&&lock&&issues.is_empty(),
        "workloadPassed":workload,"serverShutdownPassed":server,
        "diagnosticCoverageComplete":complete,"evidence":{"joinRoom":join,"roomLockWait":lock},
        "records":report.and_then(|v|v["records"].as_u64()).unwrap_or(0),
        "serverExit":exit,"shutdownMs":shutdown.map(|v|v.shutdown_ms),
        "diagnosticCoverage":coverage,"artifactIssues":issues,"artifactDirectory":output})
}
