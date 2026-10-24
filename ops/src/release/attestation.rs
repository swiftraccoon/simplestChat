//! Offline server attestation preparation and cached verification operations.

mod bindings;
mod control;
mod files;
pub mod prepare;
pub mod verify;

pub(super) mod acquire;
pub(super) mod fetch;

pub(super) mod receive;

#[cfg(test)]
#[path = "attestation/verify/fixture.rs"]
mod verification_fixture;
