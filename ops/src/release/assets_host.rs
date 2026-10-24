//! Fixed host-operation borrowing of its original root and actual flock lease.

use super::{
    Capture, Published, Result,
    lock::Workload,
    scope::{Mode, Scope},
    store::Store,
};
use crate::release::{attestation::control::Owner, workload::Lease};
use std::fs::File;

pub(in crate::release) struct HostStore {
    workload: Workload,
}
impl HostStore {
    /// The root is a duplicate of the parent's checked original public RootDir;
    /// the opaque lease can only originate from the actual acquired flock.
    pub fn borrow(root: File, lease: Lease, uid: u32, owner: &mut Owner) -> Result<Self> {
        let scope = Scope::owned(owner, Mode::Body);
        Workload::borrow(root, lease, uid, &scope).map(|workload| Self { workload })
    }
    pub fn publish(
        &mut self,
        captures: Vec<(String, Capture)>,
        wall_seconds: u64,
        owner: &mut Owner,
    ) -> Result<Published> {
        let scope = Scope::owned(owner, Mode::Body);
        let result = Store::open_scoped(&mut self.workload, &scope)
            .and_then(|mut store| store.publish_at_scoped(captures, wall_seconds, &scope));
        scope.result(result)
    }
    pub fn select(&mut self, revision: &str, owner: &mut Owner, recovery: bool) -> Result<bool> {
        let mode = if recovery { Mode::Recovery } else { Mode::Body };
        let scope = Scope::owned(owner, mode);
        let result = Store::open_scoped(&mut self.workload, &scope)
            .and_then(|mut store| store.select_version_scoped(revision, &scope));
        scope.result(result)
    }
    /// Read/close work uses original N; no body mutation or borrower unlock.
    pub fn finish(self, owner: &mut Owner) -> Result<()> {
        let scope = Scope::owned(owner, Mode::Final);
        self.workload.finish_scoped(&scope)
    }
}

#[cfg(test)]
#[path = "assets_host_tests.rs"]
mod tests;
