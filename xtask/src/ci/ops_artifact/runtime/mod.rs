//! One authenticated acquisition, original-byte transfer and owned Debian guest.
//!
//! The capability remains private through guest retirement and final replay.
//! This operation never selects a production host or changes installed callers.
//!
//! Affected API guidance: C-NEWTYPE/C-CUSTOM-TYPE distinguish authenticated
//! acquisition from public observations; C-VALIDATE/C-SERDE close request and
//! receipt domains; C-STRUCT-PRIVATE keeps ownership/pins immutable. C-RW-VALUE,
//! C-DTOR-FAIL/C-DTOR-BLOCK require bounded streams, joined workers and explicit
//! all-attempt finalization. C-GOOD-ERR/C-FAILURE/C-DEBUG keep private inputs out
//! of diagnostics. These decisions cover this change, not a retrospective audit.

mod diagnostics;
mod execute;
mod files;
#[cfg(test)]
mod fixture;
#[cfg(test)]
mod native_tests;
mod plan;
mod projection;
#[cfg(test)]
mod projection_tests;
mod receipt;
mod request;
mod sources;
#[cfg(test)]
mod tests;
pub(in crate::ci) mod tools;
mod worker;

use crate::tooling::{Failure, Result};

pub(super) const SMALL: u64 = 64 * 1024;
pub(super) const TRANSFER: u64 = 257 * 1024 * 1024;
pub(super) const CAPTURE: u64 = 2 * 1024 * 1024;

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(
        code,
        "Ops Debian runtime and first installation are unproven.",
    )
}
pub(super) fn io<T>(result: std::io::Result<T>) -> Result<T> {
    result.map_err(|_| failure(1))
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

/// Retain only passive bounded case diagnostics from this CI invocation.
pub(super) fn diagnostics() -> std::process::ExitCode {
    diagnostics::native()
}
