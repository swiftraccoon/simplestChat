//! Release metadata contracts; none of these checks authenticate signatures.
pub mod archive;
pub mod assets;
pub mod manifest;
pub mod preflight;
pub mod trust;

pub mod build;

pub mod attestation;

pub mod verify;

pub mod fetch;

pub mod receive;

mod workload;

pub mod deploy;

pub mod stage;

pub mod backup;
