//! Benchmark facade for the capacity tool's established private file policy.
//! Every output is new; final links, special files and byte growth are refused.

use anyhow::{Result, ensure};
use serde::Serialize;
use serde_json::Value;
use std::fs::File;
use std::path::Path;

/// Create exactly one new0700leaf. Unlike capacity's default output planner,
/// benchmark callers must supply an existing parent directory.
pub fn directory(path: &Path) -> Result<()> {
    ensure!(
        path.parent().is_some_and(Path::is_dir),
        "Benchmark output parent unavailable"
    );
    crate::capacity_run::files::directory(path)
}

pub fn private_file(path: &Path) -> Result<File> {
    crate::capacity_run::files::private_file(path)
}

pub fn new_file(path: &Path, bytes: &[u8]) -> Result<()> {
    crate::capacity_run::artifacts::new_file(path, bytes)
}

/// The caller chooses a smaller artifact-specific ceiling.144MiB is the fixed
/// established archive ceiling, never a user-selected unbounded read.
pub fn read_regular(path: &Path, maximum: u64) -> Result<Vec<u8>> {
    ensure!(
        (1..=144 * 1024 * 1024).contains(&maximum),
        "Invalid artifact byte bound"
    );
    crate::capacity_run::artifacts::read_regular(path, maximum)
}

pub fn read_json(path: &Path, maximum: u64) -> Result<Value> {
    serde_json::from_slice(&read_regular(path, maximum)?)
        .map_err(|_| anyhow::anyhow!("Invalid benchmark artifact JSON"))
}

/// Snapshot a live owned log's initial prefix. A writer may append while it is
/// read; later bytes are deliberately excluded rather than mistaken for a
/// changed stopped artifact. This is only readiness evidence, never a report.
pub fn live_log_prefix(path: &Path) -> Result<Vec<u8>> {
    use std::io::Read;
    let mut file = crate::capacity_run::artifacts::regular(path, 1024 * 1024)?;
    let length = file.metadata()?.len();
    ensure!(length <= 1024 * 1024, "Owned live log prefix limit");
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take(length)
        .read_to_end(&mut bytes)?;
    nix::unistd::close(file)?;
    Ok(bytes)
}

pub fn json(path: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    new_file(path, &bytes)
}

/// Replace only the fixed owned-resource intent journal through the established
/// descriptor-bound fsync/rename protocol. Result artifacts remain exclusive.
pub fn replace_ownership(directory: &Path, value: &impl Serialize) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    crate::capacity_run::files::replace(directory, "ownership.json", &bytes)
}
