//! One source for elapsed sampling time and retained wall-clock anchors.

use chrono::{SecondsFormat, Utc};
use std::time::{Duration, Instant};

#[async_trait::async_trait]
pub trait Clock: Send + Sync {
    fn elapsed_ms(&self) -> f64;
    fn utc(&self) -> String;
    async fn sleep(&self, duration: Duration);
}

pub struct RealClock(Instant);

impl Default for RealClock {
    fn default() -> Self {
        Self(Instant::now())
    }
}

#[async_trait::async_trait]
impl Clock for RealClock {
    fn elapsed_ms(&self) -> f64 {
        self.0.elapsed().as_secs_f64() * 1000.0
    }
    fn utc(&self) -> String {
        Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
    }
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
