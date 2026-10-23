//! Fixed hosted production and isolated signer admission. Neither entry runs
//! a retained candidate or treats local observations as attestation authority.

mod archive;
#[cfg(test)]
mod archive_tests;
mod catalog;
mod context;
mod coordinator;
mod fetch;
mod files;
#[cfg(test)]
mod files_tests;
#[cfg(test)]
mod fixture;
mod kit;
#[cfg(test)]
mod native_tests;
mod plan;
mod provenance;
mod shim;
mod signing;
#[cfg(test)]
mod signing_tests;
#[cfg(test)]
mod tests;
pub(in crate::ci) mod tools;
mod worker;
#[cfg(test)]
mod worker_tests;

use crate::tooling::{Failure, Result};
use std::{ffi::OsString, path::Path};

pub(super) fn failure(code: u8) -> Failure {
    Failure::new(code, "Hosted Ops artifact production is unproven.")
}

/// The caller compares this with the ORIGINAL measured CC role digest before
/// opting into hosted-only compiler variables. Generic kit recipes stay intact.
pub(super) fn cc_sha256() -> String {
    super::sha(shim::CC)
}
pub(super) fn cc_environment(run: &Path) -> Result<Vec<(OsString, OsString)>> {
    Ok(shim::environment(&run.join("kit"))?
        .into_iter()
        .map(|(name, value)| (name.into(), value))
        .collect())
}

pub(super) async fn native(output: &Path) -> Result<()> {
    coordinator::native(output).await
}
pub(super) async fn signing_native(directory: &Path, output: &Path) -> Result<()> {
    coordinator::signing_native(directory, output).await
}
