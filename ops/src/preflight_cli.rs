//! Small public projection; the host adapter has no caller-selected paths.

use simplestchat_ops::release::preflight::{ExpectedHelpers, Failure, check};
use std::{ffi::OsStr, io::Write, process::ExitCode};

pub(super) fn run(raw: Option<&OsStr>) -> ExitCode {
    let result = raw
        .map(|value| value.to_str().ok_or(Failure::Unavailable))
        .transpose()
        .and_then(ExpectedHelpers::decode)
        .and_then(|expected| check(&expected));
    let (value, code) = match result {
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
