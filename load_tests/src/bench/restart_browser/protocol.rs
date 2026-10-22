//! One closed request at a time. Reads are cancellation-safe; mutations retain
//! their owner until settled, even after phase admission has been withdrawn.

use crate::bench::{clock::Clock, process::Control};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const PHASES: [&str; 3] = ["stop-first", "start-second", "stop-second"];
const LIMIT: usize = 1024;
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    schema_version: u8,
    run_id: String,
    sequence: u8,
    pub phase: String,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Reply<'a> {
    schema_version: u8,
    run_id: &'a str,
    sequence: u8,
    passed: bool,
    status: u8,
}
pub(super) struct Session {
    control: Option<Box<dyn Control>>,
    buffer: Vec<u8>,
    since: Option<f64>,
    sequence: u8,
    acknowledged: bool,
    eof: bool,
}

#[cfg(test)]
#[path = "protocol_tests.rs"]
mod tests;
impl Session {
    pub fn new(control: Box<dyn Control>) -> Self {
        Self {
            control: Some(control),
            buffer: vec![],
            since: None,
            sequence: 0,
            acknowledged: false,
            eof: false,
        }
    }
    pub async fn poll(&mut self, run: &str, clock: &dyn Clock) -> Result<Option<Request>> {
        if self.eof {
            return std::future::pending().await;
        }
        if let Some(start) = self.since {
            ensure!(
                clock.elapsed_ms() - start < 2000.0,
                super::controller::Failure(124)
            );
        }
        let mut bytes = [0; LIMIT + 1];
        let size = self.control.as_mut().unwrap().read(&mut bytes).await?;
        if let Some(start) = self.since {
            ensure!(
                clock.elapsed_ms() - start < 2000.0,
                super::controller::Failure(124)
            );
        }
        if size == 0 {
            ensure!(self.complete(), "restart_control_closed_before_completion");
            self.eof = true;
            return Ok(None);
        }
        if self.since.is_none() {
            self.since = Some(clock.elapsed_ms());
        }
        ensure!(self.buffer.len() + size <= LIMIT, "restart_control_limit");
        self.buffer.extend_from_slice(&bytes[..size]);
        let Some(end) = self.buffer.iter().position(|v| *v == b'\n') else {
            return Ok(None);
        };
        ensure!(end + 1 == self.buffer.len(), "restart_control_extra");
        let request: Request = serde_json::from_slice(&self.buffer[..end])?;
        ensure!(
            serde_json::to_vec(&request)? == self.buffer[..end],
            "restart_control_noncanonical"
        );
        ensure!(
            request.schema_version == 1
                && request.run_id == run
                && request.sequence == self.sequence + 1
                && PHASES.get(self.sequence as usize) == Some(&request.phase.as_str()),
            "restart_control_order"
        );
        self.sequence = request.sequence;
        self.acknowledged = false;
        self.buffer.clear();
        self.since = None;
        Ok(Some(request))
    }
    pub async fn reply(&mut self, run: &str, status: u8) -> Result<()> {
        let mut bytes = serde_json::to_vec(&Reply {
            schema_version: 1,
            run_id: run,
            sequence: self.sequence,
            passed: status == 0,
            status,
        })?;
        bytes.push(b'\n');
        // A timed-out partial write is never retried or called acknowledged.
        self.control
            .as_mut()
            .unwrap()
            .write_bounded(&bytes, Duration::from_secs(2))
            .await?;
        self.acknowledged = status == 0;
        Ok(())
    }
    pub fn complete(&self) -> bool {
        self.sequence == 3 && self.acknowledged && self.buffer.is_empty() && self.since.is_none()
    }
    pub async fn settle(&mut self, clock: &dyn Clock) -> Result<()> {
        ensure!(self.complete(), "restart_control_incomplete");
        if self.eof {
            return Ok(());
        }
        let started = clock.elapsed_ms();
        ensure!(
            started.is_finite() && started >= 0.0,
            "invalid_restart_clock"
        );
        let mut byte = [0; 1];
        let count = tokio::time::timeout(
            Duration::from_secs(2),
            self.control.as_mut().unwrap().read(&mut byte),
        )
        .await
        .map_err(|_| super::controller::Failure(124))??;
        let elapsed = clock.elapsed_ms() - started;
        ensure!(
            elapsed.is_finite() && elapsed >= 0.0,
            "invalid_restart_clock"
        );
        ensure!(elapsed < 2000.0, super::controller::Failure(124));
        ensure!(count == 0, "restart_control_extra");
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.control.take().unwrap().finish()
    }
}
