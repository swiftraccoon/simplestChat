#![forbid(unsafe_code)]

//! Offline planning, bounded diagnostic readers and explicit capacity measurement.
//! Importing these modules starts no runtime, connection, engine or workload;
//! only explicit `capacity run` or `bench` runtime commands construct a
//! measurement runtime and create owned resources.

#[cfg(feature = "full")]
pub mod bench;
#[cfg(feature = "capacity-runtime")]
pub mod capacity;
#[cfg(feature = "capacity-runtime")]
mod capacity_run;
#[cfg(feature = "capacity-runtime")]
pub(crate) mod owned_process;
#[cfg(feature = "full")]
pub mod report;

#[cfg(feature = "full")]
mod evidence;
#[cfg(feature = "full")]
pub use evidence::{LatencyStats, MeasurementMetrics, PlannedSubscriptionEdge, RunMetadata};

#[cfg(feature = "full")]
mod plan;
#[cfg(feature = "full")]
pub use plan::{
    MAX_PLANNED_CLIENTS, MAX_PLANNED_TARGETS_PER_KIND, PlannedClient, PlannedTargets,
    SubscriptionPlan,
};
