//! Fixed native launcher prerequisite proof; no production caller is changed.
#[cfg(feature = "maintain-guest")]
pub use super::attestation::maintain_prerequisite::deploy_fixture_docker;
pub use super::attestation::maintain_prerequisite::run;
