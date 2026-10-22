//! Fixed-root browser asset transactions. Python remains the installed runtime.
//! A typed same-process workload guard owns serialization; filesystem snapshots
//! are optimistic observations, not exclusion of a malicious root or crash recovery.

#[path = "assets_capture.rs"]
mod capture;
#[path = "assets_commit.rs"]
mod commit;
#[path = "assets_files.rs"]
mod files;
#[path = "assets_lock.rs"]
mod lock;
#[path = "assets_model.rs"]
mod model;
#[cfg(test)]
#[path = "assets_oracle_tests.rs"]
mod oracle_tests;
#[path = "assets_store.rs"]
mod store;
#[cfg(test)]
#[path = "assets_tests.rs"]
mod tests;

pub use lock::Workload;
pub use model::Capture;
pub use store::{Published, Store, Summary};

/// Fixed diagnostic vocabulary; raw paths, tar names and JSON never enter it.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Failure {
    code: &'static str,
}
impl Failure {
    pub fn code(self) -> &'static str {
        self.code
    }
    pub fn busy(self) -> bool {
        self.code == "asset_workload_busy"
    }
}
impl std::fmt::Debug for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code)
    }
}
impl std::error::Error for Failure {}
type Result<T> = std::result::Result<T, Failure>;
fn fail(code: &'static str) -> Failure {
    Failure { code }
}
fn require(condition: bool, code: &'static str) -> Result<()> {
    if condition { Ok(()) } else { Err(fail(code)) }
}

const MAX_FILE: u64 = 8 * 1024 * 1024;
const MAX_TOTAL: u64 = 32 * 1024 * 1024;
const MAX_TAR: u64 = MAX_TOTAL + 1024 * 1024;
const MAX_INDEX: u64 = 256 * 1024;
const MAX_FILES: usize = 256;
const MAX_GENERATIONS: usize = 4;
const MAX_CAPTURES: usize = 2;
const MAX_VERSION: u64 = 128;
const MAX_AGE: u64 = 7 * 86400;
