//! Fixed Ops production, signer-input admission and authenticated retention.
//! Local material pins select bytes; the trusted workflow and fresh verifier
//! supply their separate identity boundaries. Compiler processes are not a
//! sandbox. Retaining an artifact grants no execution or installation authority.

pub(super) mod acquire;
pub(in crate::ci) mod capacity_vps;
mod execute;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod fixture;
pub(super) mod hosted;
mod maintain_runtime;
mod materials;
#[cfg(test)]
mod materials_tests;
mod namespace;
#[cfg(test)]
mod native_tests;
pub(super) mod recipe;
mod registry;
mod request;
mod resolution;
pub(in crate::ci) mod runtime;
pub(crate) mod source;
#[cfg(test)]
mod tests;
pub(in crate::ci) mod verify;

use crate::tooling::{Failure, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

const USAGE: &str = "usage: cargo run --locked -q -p xtask -- ops-artifact \
build|source|registry|verify-retained|acquire|runtime \
--request PATH --request-sha256 SHA256\n\
cargo run --locked -q -p xtask -- ops-artifact hosted --output PATH\n\
cargo run --locked -q -p xtask -- ops-artifact signing-inputs --directory PATH --output PATH\n\
cargo run --locked -q -p xtask -- ops-artifact runtime-diagnostics\n\
xtask ops-artifact maintain-runtime --run-id N --run-attempt N --artifact-id N \
--scenario CASE --output ABS";

pub(super) fn run(arguments: &[String]) -> anyhow::Result<std::process::ExitCode> {
    if let Some((operation, arguments)) = arguments.split_first()
        && operation == "maintain-runtime"
    {
        return Ok(maintain_runtime::run(arguments));
    }
    if arguments == ["--help"] {
        println!("{USAGE}");
        return Ok(std::process::ExitCode::SUCCESS);
    }
    if arguments == ["runtime-diagnostics"] {
        return Ok(runtime::diagnostics());
    }
    if let [operation, flag, output] = arguments
        && operation == "hosted"
        && flag == "--output"
        && namespace::path(std::path::Path::new(output))
    {
        return Ok(completed(
            tokio_runtime()?.block_on(hosted::native(std::path::Path::new(output))),
        ));
    }
    if let [operation, directory_flag, directory, output_flag, output] = arguments
        && operation == "signing-inputs"
        && directory_flag == "--directory"
        && output_flag == "--output"
        && namespace::path(std::path::Path::new(directory))
        && namespace::path(std::path::Path::new(output))
    {
        return Ok(completed(tokio_runtime()?.block_on(
            hosted::signing_native(
                std::path::Path::new(directory),
                std::path::Path::new(output),
            ),
        )));
    }
    let [operation, request_flag, path, sha_flag, selected] = arguments else {
        eprintln!("{USAGE}");
        return Ok(std::process::ExitCode::from(2));
    };
    if !matches!(
        operation.as_str(),
        "build" | "source" | "registry" | "verify-retained" | "acquire" | "runtime"
    ) || request_flag != "--request"
        || sha_flag != "--request-sha256"
        || !namespace::path(std::path::Path::new(path))
        || !digest(selected)
        || (operation == "verify-retained" && !verify::request_path(std::path::Path::new(path)))
        || (operation == "acquire" && !acquire::request_path(std::path::Path::new(path)))
        || (operation == "runtime" && !runtime::request_path(std::path::Path::new(path)))
    {
        eprintln!("{USAGE}");
        return Ok(std::process::ExitCode::from(2));
    }
    let result = tokio_runtime()?.block_on(async {
        if operation == "verify-retained" {
            verify::native(std::path::Path::new(path), selected).await
        } else if operation == "acquire" {
            acquire::native(std::path::Path::new(path), selected).await
        } else if operation == "runtime" {
            runtime::native(std::path::Path::new(path), selected).await
        } else if operation == "source" {
            source::native(std::path::Path::new(path), selected)
                .await
                .map_err(|error| Failure::new(error.code, "Ops source export is unproven."))
        } else if operation == "registry" {
            registry::native(std::path::Path::new(path), selected)
                .await
                .map_err(|error| {
                    Failure::new(error.code, "Ops offline registry hydration is unproven.")
                })
        } else {
            execute::native(std::path::Path::new(path), selected).await
        }
    });
    Ok(completed(result))
}

fn tokio_runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

fn completed(result: Result<()>) -> std::process::ExitCode {
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.message);
            std::process::ExitCode::from(error.code)
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
const DISPATCH_SETUP: Duration = Duration::from_secs(1);
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
    limit: Instant,
}
impl<'a, C: Clock> Deadline<'a, C> {
    pub fn end(&self) -> Instant {
        self.limit
    }
    pub fn new(clock: &'a C) -> Self {
        let start = clock.now();
        Self {
            clock,
            start,
            limit: start + TOTAL,
        }
    }
    /// Embed a stage without extending the enclosing operation's deadline.
    /// The existing two-minute settlement reserve still applies to shorter
    /// stages, so an insufficient outer window refuses before active work.
    pub fn bounded(clock: &'a C, end: Instant) -> Self {
        let mut deadline = Self::new(clock);
        deadline.limit = deadline.limit.min(end);
        deadline
    }
    pub fn check(&self, finalizing: bool) -> Result<()> {
        let entry = self.limit.saturating_duration_since(self.start);
        let limit = if finalizing {
            entry
        } else {
            entry.checked_sub(RESERVE).ok_or_else(|| failure(124))?
        };
        if self.clock.now().saturating_duration_since(self.start) >= limit {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn body(&self, maximum: Duration) -> Result<Duration> {
        self.check(false)?;
        let remaining = self
            .limit
            .checked_sub(RESERVE)
            .and_then(|end| end.checked_duration_since(self.clock.now()))
            .ok_or_else(|| failure(124))?;
        let body = remaining
            .checked_sub(GRACE + SETTLEMENT + DISPATCH_SETUP)
            .ok_or_else(|| failure(124))?
            .min(maximum);
        if body.is_zero() {
            Err(failure(124))
        } else {
            Ok(body)
        }
    }
    /// Latest spawn for an already chosen body. Setup may consume this window
    /// but cannot move it or borrow the original settlement/finalization tail.
    pub fn dispatch_by(&self, body: Duration) -> Result<Instant> {
        self.check(false)?;
        if body.is_zero() {
            return Err(failure(124));
        }
        let latest = self
            .limit
            .checked_sub(RESERVE)
            .and_then(|end| end.checked_sub(body))
            .and_then(|end| end.checked_sub(GRACE + SETTLEMENT))
            .ok_or_else(|| failure(124))?;
        let now = self.clock.now();
        if now < self.start {
            return Err(failure(1));
        }
        if now > latest {
            return Err(failure(124));
        }
        Ok(latest)
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
