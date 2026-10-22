//! Workload, evidence and server shutdown remain independent gates.

use serde::Serialize;
use serde_json::Value;

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(value) => *value,
        Value::Number(value) => value.as_f64().is_some_and(|value| value != 0.0),
        Value::String(value) => !value.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Performance {
    pub workload_passed: bool,
    pub server_shutdown_passed: bool,
    pub passed: bool,
}

/// A zero exit alone is insufficient when a signal or spawn error was retained.
pub fn performance(rows: &[Value]) -> Performance {
    let workload_passed = !rows.is_empty() && rows.iter().all(|row| row["workloadPassed"] == true);
    let server_shutdown_passed = !rows.is_empty()
        && rows.iter().all(|row| {
            let exit = &row["serverExit"];
            exit["code"].as_f64() == Some(0.0)
                && !truthy(&exit["signal"])
                && !truthy(&exit["error"])
        });
    Performance {
        workload_passed,
        server_shutdown_passed,
        passed: workload_passed && server_shutdown_passed,
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub workload_passed: bool,
    pub diagnostic_coverage_complete: bool,
    pub media_diagnostic_coverage_complete: bool,
    pub lifecycle_diagnostic_coverage_complete: bool,
    pub server_shutdown_passed: bool,
    pub passed: bool,
}

/// Legacy rows without a stall field remain compatible. New requested reports
/// must be saved and available; unsupported detector coverage stays distinct.
pub fn diagnostic(rows: &[Value]) -> Diagnostic {
    let performance = performance(rows);
    let coverage = |key| {
        !rows.is_empty()
            && rows
                .iter()
                .all(|row| row[key]["requested"] == false || row[key]["complete"] == true)
    };
    let diagnostic_coverage_complete = coverage("diagnosticCoverage");
    let media_diagnostic_coverage_complete = coverage("mediaDiagnosticCoverage");
    let lifecycle_diagnostic_coverage_complete = coverage("lifecycleDiagnosticCoverage");
    let stall_reports_written = rows.iter().all(|row| {
        row.get("receiverStallReport").is_none()
            || row["receiverStallReport"]["requested"] == false
            || row["receiverStallReport"]["written"] == true
                && row["receiverStallReport"]["available"] == true
    });
    Diagnostic {
        workload_passed: performance.workload_passed,
        diagnostic_coverage_complete,
        media_diagnostic_coverage_complete,
        lifecycle_diagnostic_coverage_complete,
        server_shutdown_passed: performance.server_shutdown_passed,
        passed: performance.passed
            && diagnostic_coverage_complete
            && media_diagnostic_coverage_complete
            && lifecycle_diagnostic_coverage_complete
            && stall_reports_written,
    }
}
