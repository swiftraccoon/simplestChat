// Load testing infrastructure for SimplestChat

pub mod clients;

pub use clients::{SyntheticClient, MediaConfig, ClientMetrics, MetricsCollector, TestSummary};
