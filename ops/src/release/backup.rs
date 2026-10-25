//! Complete backup operations; existing installed Python callers stay separate.
/// Isolated verification of one recorded release backup on the fixed Linux host.
pub mod verify {
    pub use super::super::attestation::restore::run;
}
