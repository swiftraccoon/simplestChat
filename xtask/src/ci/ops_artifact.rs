//! A caller-authorized, fixed-recipe producer of unsigned local Ops candidates.
//! Material pins select bytes, not authenticated source/toolchain/runtime facts.
//! Cargo and compiler subprocesses are not a sandbox or a network isolation layer.

mod execute;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod fixture;
mod materials;
mod namespace;
#[cfg(test)]
mod native_tests;
pub(super) mod recipe;
mod request;
mod resolution;
pub(crate) mod source;
#[cfg(test)]
mod tests;

use crate::tooling::{Failure, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ops-artifact build|source \
--request PATH --request-sha256 SHA256";

pub(super) fn run(arguments: &[String]) -> anyhow::Result<std::process::ExitCode> {
    if arguments == ["--help"] {
        println!("{USAGE}");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    let [operation, request_flag, path, sha_flag, selected] = arguments else {
        eprintln!("{USAGE}");
        return Ok(std::process::ExitCode::from(2));
    };
    if !matches!(operation.as_str(), "build" | "source")
        || request_flag != "--request"
        || sha_flag != "--request-sha256"
        || !namespace::path(std::path::Path::new(path))
        || !digest(selected)
    {
        eprintln!("{USAGE}");
        return Ok(std::process::ExitCode::from(2));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        if operation == "source" {
            source::native(std::path::Path::new(path), selected)
                .await
                .map_err(|error| Failure::new(error.code, "Ops source export is unproven."))
        } else {
            execute::native(std::path::Path::new(path), selected).await
        }
    });
    match result {
        Ok(()) => Ok(std::process::ExitCode::SUCCESS),
        Err(error) => {
            eprintln!("{}", error.message);
            Ok(std::process::ExitCode::from(error.code))
        }
    }
}

pub(super) const TARGET: &str = "x86_64-unknown-linux-musl";
pub(super) const HOST: &str = "x86_64-unknown-linux-gnu";
pub(super) const VERSION: &str = "1.99.0";
const TOTAL: Duration = Duration::from_secs(20 * 60);
const RESERVE: Duration = Duration::from_secs(2 * 60);
const SETTLEMENT: Duration = Duration::from_secs(20);
const GRACE: Duration = Duration::from_secs(5);
const LOG_LIMIT: u64 = 64 * 1024 * 1024;

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Ops candidate production is unproven.")
}
pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
pub(super) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(super) fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}
pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) trait Clock {
    fn now(&self) -> Instant;
}
pub(super) struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// Scheduling limits are watchdogs. Filesystem, scheduling and reap stalls are
/// not bounded by this policy; no cold-build or hard wall-clock claim is made.
pub(super) struct Deadline<'a, C> {
    pub clock: &'a C,
    start: Instant,
}
impl<'a, C: Clock> Deadline<'a, C> {
    pub fn end(&self) -> Instant {
        self.start + TOTAL
    }
    pub fn new(clock: &'a C) -> Self {
        Self {
            clock,
            start: clock.now(),
        }
    }
    pub fn check(&self, finalizing: bool) -> Result<()> {
        let limit = if finalizing { TOTAL } else { TOTAL - RESERVE };
        if self.clock.now().saturating_duration_since(self.start) >= limit {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn body(&self, maximum: Duration) -> Result<Duration> {
        self.check(false)?;
        let remaining = (TOTAL - RESERVE)
            .saturating_sub(self.clock.now().saturating_duration_since(self.start));
        let body = remaining
            .checked_sub(GRACE + SETTLEMENT)
            .ok_or_else(|| failure(124))?;
        if body.is_zero() {
            Err(failure(124))
        } else {
            Ok(body.min(maximum))
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Observation {
    schema_version: u8,
    kind: &'static str,
    request_sha256: String,
    request_bytes: usize,
    source: materials::Identity,
    builder_kit: materials::Identity,
    artifact: simplestchat_ops::distribution::manifest::assembly::PreparedArtifact,
    claims: &'static str,
}
