//! Fixed-root application deployment; signatures and caller upgrades stay separate.

pub use super::attestation::deploy::run;
pub(crate) use super::attestation::deploy::{Relay, activate};
