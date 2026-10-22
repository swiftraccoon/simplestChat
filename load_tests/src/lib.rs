#![forbid(unsafe_code)]

//! Offline capacity planning and bounded load-test/server diagnostic reports.
//! These modules do not construct a runtime, connect to a server or run workloads.

pub mod capacity;
pub mod report;
