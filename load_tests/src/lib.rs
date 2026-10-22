#![forbid(unsafe_code)]

//! Offline planning, bounded diagnostic readers and explicit capacity measurement.
//! Importing these modules starts no runtime, connection, engine or workload;
//! only an explicit `capacity run` command constructs the measurement runtime.

pub mod capacity;
mod capacity_run;
pub mod report;
