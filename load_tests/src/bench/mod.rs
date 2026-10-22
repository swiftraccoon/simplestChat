//! Shared comparison policy and bounded owned observation APIs.
//! Importing this library starts no runtime, process, listener or workload.

pub mod artifacts;
pub mod clock;
pub mod commands;
pub mod comparison;
pub mod media_sampler;
pub mod metrics;
pub mod options;
pub mod process;
pub mod provenance;
pub mod reports;
pub mod resources;
pub mod sampling;
pub mod subscription;
pub mod summary;
pub mod verdict;

#[cfg(test)]
mod media_sampler_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod reports_tests;
#[cfg(test)]
mod subscription_tests;
#[cfg(test)]
mod summary_tests;

#[cfg(test)]
mod process_tests;
