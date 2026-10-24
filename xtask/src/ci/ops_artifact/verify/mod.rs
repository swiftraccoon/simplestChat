//! Fresh online verification of one caller-retained, flat Ops artifact.
//! No artifact transport, signing, candidate execution or installation occurs.

#[cfg(test)]
mod embedding_tests;
mod execute;
#[cfg(test)]
mod fault_tests;
mod files;
#[cfg(test)]
mod fixture;
mod hash;
#[cfg(test)]
mod native_tests;
mod plan;
mod request;
#[cfg(test)]
mod tests;
pub(in crate::ci) mod tools;
#[cfg(test)]
mod worker_tests;

use crate::tooling::{Failure, Result};
pub(super) use files::OutputWitness as RetainedWitness;
pub(super) use hash::Checkpoint as RetainedCheckpoint;
pub(super) use plan::Clock as RetainedClock;
use std::{sync::Arc, time::Instant};

/// The acquisition owner supplies its existing monotonic clock and absolute
/// operation end. Verification reserves its complete entry budget plus the
/// caller's finalization tail before touching the retained artifact.
pub(super) struct Window {
    pub clock: Arc<dyn RetainedClock>,
    pub end: Instant,
}

pub(super) struct RetainedObservation {
    pub bytes: Vec<u8>,
    pub witness: RetainedWitness,
    pub executable: simplestchat_ops::distribution::ExecutableIdentity,
}
impl std::fmt::Debug for RetainedObservation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RetainedOpsVerificationObservation")
    }
}

pub(super) async fn observe_retained<E: crate::ci::process::Executor>(
    path: std::path::PathBuf,
    selected: String,
    caller: std::path::PathBuf,
    environment: crate::tooling::Environment,
    budget: super::namespace::Budget,
    window: Window,
    executor: &mut E,
) -> Result<RetainedObservation> {
    let context = plan::Context::new(window.clock);
    if context
        .end()
        .checked_add(std::time::Duration::from_secs(45))
        .is_none_or(|end| end > window.end)
    {
        return Err(failure(124));
    }
    execute::observation(
        path,
        selected,
        caller,
        environment,
        budget,
        &context,
        executor,
    )
    .await
}

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Ops retained verification is unproven.")
}
pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
pub(super) fn keep(primary: &mut Option<Failure>, error: Failure) {
    if primary.is_none() {
        *primary = Some(error);
    }
}
pub(super) const BUNDLE: &str = "ops-attestation.jsonl";
pub(super) const SMALL: u64 = 64 * 1024;
pub(super) const MAX_BUNDLE: u64 = 8 * 1024 * 1024;
pub(super) const MAX_TOTAL: u64 = 281 * 1024 * 1024;
pub(super) fn request_path(path: &std::path::Path) -> bool {
    request::path(path)
}

pub(super) async fn native(path: &std::path::Path, selected: &str) -> Result<()> {
    execute::native(path, selected).await
}
