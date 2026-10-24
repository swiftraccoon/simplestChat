//! One explicitly selected private-VPS operation. Parsed records are data;
//! authenticated acquisition, retained files and the original invocation own
//! execution authority. No candidate runs on the controller.

mod archive;
#[cfg(test)]
mod controller_tests;
mod execute;
mod files;
mod inventory;
mod options;
mod plan;
pub(in crate::ci) mod prepare;
mod projection;
mod report;
mod sources;
#[cfg(test)]
pub(in crate::ci) mod tests;
pub(in crate::ci) mod tools;
mod transport;
mod worker;

use crate::tooling::{Failure, Result};

const MAX_JSON: u64 = 8 * 1024 * 1024;
const MAX_STATUS: u64 = 65536;
const MAX_STDERR: u64 = 2 * 1024 * 1024;
const MAX_ARCHIVE: u64 = 512 * 1024 * 1024;
const MAX_TRANSFER: u64 = 281 * 1024 * 1024;
pub(in crate::ci) const USAGE: &str = options::USAGE;

fn failure(code: u8) -> Failure {
    Failure::new(code, "Private VPS capacity operation is unproven.")
}
fn io<T>(value: std::io::Result<T>) -> Result<T> {
    value.map_err(|_| failure(1))
}
fn keep(primary: &mut Option<Failure>, error: Failure) {
    if primary.is_none() {
        *primary = Some(error);
    }
}

pub(in crate::ci) async fn native(arguments: &[String]) -> Result<()> {
    execute::native(arguments).await
}
