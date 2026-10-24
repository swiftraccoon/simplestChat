//! Controller-local acquisition of one exact signed Ops artifact.
//! Retention does not authorize candidate execution, installation or host changes.

mod archive;
#[cfg(test)]
mod archive_tests;
mod execute;
#[cfg(test)]
mod fault_tests;
mod files;
#[cfg(test)]
pub(in crate::ci::ops_artifact) mod fixture;
mod hash;
mod metadata;
#[cfg(test)]
mod native_tests;
mod plan;
mod request;
#[cfg(test)]
mod tests;
pub(in crate::ci) mod tools;

use crate::tooling::{Failure, Result};
pub(super) use execute::AuthenticatedAcquisition;
pub(super) use files::PairPins;
pub(super) use hash::Checkpoint as AcquisitionCheckpoint;
pub(super) use plan::Clock as AcquisitionClock;
use std::{sync::Arc, time::Instant};

/// The caller owns the only clock, signal registration and process executor.
/// The existing acquisition schedule is clipped to this original phase end.
pub(super) struct Window {
    pub clock: Arc<dyn AcquisitionClock>,
    pub end: Instant,
}

pub(super) async fn observe_acquired<E: crate::ci::process::Executor>(
    path: std::path::PathBuf,
    selected: String,
    caller: std::path::PathBuf,
    environment: crate::tooling::Environment,
    budget: super::namespace::Budget,
    window: Window,
    executor: &mut E,
) -> Result<AuthenticatedAcquisition> {
    let context = plan::Context::bounded(window.clock, window.end);
    context.admit()?;
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

pub(super) const ZIP: u64 = 300 * 1024 * 1024;
pub(super) const SMALL: u64 = 64 * 1024;
pub(super) const EXPANDED: u64 = 281 * 1024 * 1024;
pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Ops artifact acquisition is unproven.")
}
pub(super) fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
pub(super) fn keep(primary: &mut Option<Failure>, error: Failure) {
    if primary.is_none() {
        *primary = Some(error);
    }
}

pub(super) fn request_path(path: &std::path::Path) -> bool {
    request::path(path)
}

pub(super) async fn native(path: &std::path::Path, selected: &str) -> Result<()> {
    execute::native(path, selected).await
}
