//! Fresh online verification of one caller-retained, flat Ops artifact.
//! No artifact transport, signing, candidate execution or installation occurs.

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
