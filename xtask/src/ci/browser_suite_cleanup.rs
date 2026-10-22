//! Compatibility edge for the shared reserved process-group cleanup proof.

pub(in crate::ci) use crate::owned_process::finish;
#[cfg(test)]
pub(super) use crate::owned_process::proof;
