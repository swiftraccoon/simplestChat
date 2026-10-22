#![forbid(unsafe_code)]

//! Offline planning, bounded diagnostic readers and explicit capacity measurement.
//! Importing these modules starts no runtime, connection, engine or workload;
//! only an explicit `capacity run` command constructs the measurement runtime.

pub mod bench;
pub mod capacity;
mod capacity_run;
pub(crate) mod owned_process;
pub mod report;

mod evidence;
pub use evidence::{LatencyStats, MeasurementMetrics, PlannedSubscriptionEdge, RunMetadata};

mod plan;
pub use plan::{
    MAX_PLANNED_CLIENTS, MAX_PLANNED_TARGETS_PER_KIND, PlannedClient, PlannedTargets,
    SubscriptionPlan,
};
