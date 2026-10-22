//! Selected offline reader commands with an independently checked fixed output
//! protocol. Divergent reader versions fail closed, without exposing raw output.

use super::{
    commands,
    process::{Command, Host, Sink},
    provenance,
};
use anyhow::{Result, ensure};
use serde_json::Value;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub struct Request<'a> {
    pub mode: &'a str,
    pub path: &'a Path,
    pub input: &'a Value,
    pub admitted: bool,
    pub expected_participants: u32,
}

#[async_trait::async_trait]
pub trait Reports: Send + Sync {
    /// Both owned workload processes must already be stopped. Exit1 retains a
    /// valid incomplete report; mismatches and process failures return one fixed
    /// error, never received fields, private input or operating-system details.
    async fn read(&self, request: Request<'_>) -> Result<Value>;
}

pub struct NativeReports<'a> {
    pub host: &'a dyn Host,
    pub generator: PathBuf,
    pub generator_sha256: String,
    pub directory: PathBuf,
    pub environment: BTreeMap<OsString, OsString>,
}

fn normalize(value: &mut Value) {
    match value {
        Value::Number(number) => {
            *number = serde_json::Number::from_f64(number.as_f64().unwrap())
                .expect("serde_json numbers are representable finite doubles");
        }
        Value::Array(values) => values.iter_mut().for_each(normalize),
        Value::Object(values) => values.values_mut().for_each(normalize),
        _ => {}
    }
}

/// Object order is immaterial and JSON's 1/1.0 spellings share the JS Number
/// domain. Exact recursive equality rejects every unknown field at every depth.
pub(super) fn checked(mut received: Value, expected: Value, code: i32) -> Result<Value> {
    ensure!(code == 0 || code == 1, "benchmark_report_command_failed");
    normalize(&mut received);
    let mut normalized = expected.clone();
    normalize(&mut normalized);
    ensure!(
        received == normalized && (received["coverage"]["complete"] == true) == (code == 0),
        "benchmark_report_command_failed"
    );
    Ok(expected)
}

impl NativeReports<'_> {
    async fn execute(&self, request: Request<'_>) -> Result<Value> {
        ensure!(
            ["diagnostics", "lifecycle", "media", "receiver-stall"].contains(&request.mode),
            "benchmark_report_command_failed"
        );
        provenance::verify_executable(&self.generator, &self.generator_sha256, "Generator")?;
        let input = if request.mode == "diagnostics" {
            Vec::new()
        } else {
            serde_json::to_vec(request.input)?
        };
        let maximum = if request.mode == "lifecycle" {
            64 * 1024
        } else {
            12 * 1024 * 1024
        };
        ensure!(input.len() <= maximum, "benchmark_report_command_failed");
        let expected = crate::report::benchmark_report(
            request.mode,
            request.path,
            request.input,
            request.admitted,
            request.expected_participants,
        )?;
        let mut arguments = vec![
            "report".into(),
            request.mode.into(),
            request
                .path
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("benchmark_report_command_failed"))?
                .into(),
        ];
        if request.mode == "lifecycle" {
            arguments.extend([
                "--expected-participants".into(),
                request.expected_participants.to_string(),
            ]);
        } else if ["media", "receiver-stall"].contains(&request.mode) && !request.admitted {
            arguments.push("--results-unavailable".into());
        }
        let stdout_limit = match request.mode {
            "diagnostics" => 2 * 1024 * 1024,
            "lifecycle" => 16 * 1024 * 1024,
            _ => 128 * 1024 * 1024,
        };
        let output = super::process::run(
            self.host,
            Command {
                executable: self.generator.clone(),
                arguments,
                directory: self.directory.clone(),
                environment: commands::clean(&self.environment),
                timeout: Duration::from_secs(30),
                stdout_limit,
                stderr_limit: 64 * 1024,
                input,
                stdout: Sink::Capture,
                stderr: Sink::Capture,
                // This verified post-stop reader owns no workload and must retain
                // failure evidence even when a signal interrupted the workload.
                cleanup: true,
            },
        )
        .await?;
        ensure!(
            output.exit.signal.is_none(),
            "benchmark_report_command_failed"
        );
        let report = serde_json::from_slice(&output.stdout)?;
        checked(report, expected, output.exit.code.unwrap_or(-1))
    }
}

#[async_trait::async_trait]
impl Reports for NativeReports<'_> {
    async fn read(&self, request: Request<'_>) -> Result<Value> {
        self.execute(request)
            .await
            .map_err(|_| anyhow::anyhow!("benchmark_report_command_failed"))
    }
}
