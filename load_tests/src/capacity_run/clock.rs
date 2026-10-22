//! One owner for the monotonic measurement window and report wall clock.

use super::process::{Cancellation, Interrupted};
use anyhow::Result;
use chrono::{SecondsFormat, Utc};
use std::time::{Duration, Instant};

#[async_trait::async_trait]
pub(super) trait Clock: Send + Sync {
    fn now(&self) -> f64;
    fn utc(&self) -> String;
    async fn sleep(&self, seconds: f64, cancellation: &Cancellation) -> Result<()>;
}

pub(super) struct RealClock(Instant);

impl RealClock {
    pub(super) fn new() -> Self {
        Self(Instant::now())
    }
}

#[async_trait::async_trait]
impl Clock for RealClock {
    fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }
    fn utc(&self) -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Secs, false)
    }
    async fn sleep(&self, seconds: f64, cancellation: &Cancellation) -> Result<()> {
        tokio::select! {
            code = cancellation.cancelled() => Err(Interrupted(code).into()),
            _ = tokio::time::sleep(Duration::from_secs_f64(seconds)) => Ok(()),
        }
    }
}
