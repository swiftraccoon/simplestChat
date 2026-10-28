//! Offline server attestation preparation and cached verification operations.

mod bindings;
pub(super) mod control;
pub(crate) use control::signals::Signals as CompletionFlags;
mod files;
pub mod prepare;
pub mod verify;

pub(super) mod acquire;
pub(super) mod fetch;

pub(super) mod receive;

#[cfg(test)]
#[path = "attestation/verify/fixture.rs"]
mod verification_fixture;

pub(super) mod stage;

pub(super) mod deploy;

pub(super) mod restore;

pub(super) mod maintain;
pub(super) mod maintain_prerequisite;
