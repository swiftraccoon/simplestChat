//! Small public projection; the host adapter has no caller-selected paths.

use simplestchat_ops::release::preflight::check;
use std::{io::Write, process::ExitCode};

pub(super) fn run() -> ExitCode {
    let (value, code) = match check() {
        Ok(receipt) => (serde_json::to_value(receipt), ExitCode::SUCCESS),
        Err(failure) => (
            Ok(serde_json::json!({
                "schemaVersion": 1, "passed": false, "failure": failure.code(),
            })),
            ExitCode::FAILURE,
        ),
    };
    let Ok(mut bytes) = value.and_then(|value| serde_json::to_vec(&value)) else {
        return ExitCode::FAILURE;
    };
    bytes.push(b'\n');
    if std::io::stdout().lock().write_all(&bytes).is_err() {
        ExitCode::FAILURE
    } else {
        code
    }
}
