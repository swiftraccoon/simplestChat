//! Owned filesystem fixtures; every path is created under this process's unique
//! private directory. The path is canonical so platform /tmp links aren't read.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
static NEXT: AtomicUsize = AtomicUsize::new(0);
pub(crate) struct Fixture(pub PathBuf);
impl Fixture {
    pub(crate) fn new() -> Self {
        let path = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "simplestchat-ops-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
        Self(path)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

/// Set to `1` in a child fixture's environment by the parent test that owns it.
pub(crate) const CHILD_FIXTURE: &str = "SIMPLESTCHAT_CHILD_FIXTURE";

/// Whether this test process is a child fixture launched by its owning parent.
/// A broad `--include-ignored` run reaches the fixture without the variable;
/// the fixture then returns at once instead of failing for want of its parent.
pub(crate) fn launched_by_parent() -> bool {
    std::env::var_os(CHILD_FIXTURE).is_some_and(|value| value == "1")
}
