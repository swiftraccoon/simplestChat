//! One inherited capability, one outstanding fixed phase request. Partial reads
//! live in Session across polling selects; an admitted mutation is never dropped.

use super::{Failure, Plan, Services};
use crate::bench::{clock::Clock, process::Control};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub(super) const PHASES: [&str; 6] = [
    "baseline",
    "lossy",
    "constrained",
    "severe",
    "recovery",
    "lossyJoin",
];
const FRAME: usize = 1024;

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Request {
    schema_version: u8,
    run_id: String,
    sequence: u8,
    pub phase: String,
    action: String,
    #[serde(deserialize_with = "super::present")]
    profile: Option<String>,
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
    channel: Option<Box<dyn Control>>,
    buffer: Vec<u8>,
    partial_since: Option<f64>,
    sequence: u8,
    finished: bool,
    acknowledged: bool,
    eof: bool,
    steps: u8,
    finalizing: bool,
}
impl Session {
    pub fn new(channel: Box<dyn Control>) -> Self {
        Self {
            channel: Some(channel),
            buffer: Vec::new(),
            partial_since: None,
            sequence: 0,
            finished: false,
            acknowledged: false,
            eof: false,
            steps: 7,
            finalizing: false,
        }
    }
    pub fn tcp(channel: Box<dyn Control>) -> Self {
        Self {
            steps: 4,
            ..Self::new(channel)
        }
    }
    pub fn passed(&self) -> bool {
        self.sequence == self.steps && self.finished && self.acknowledged && self.buffer.is_empty()
    }
    /// A single cancellation-safe read; callers can poll child status between
    /// reads. A partial frame has its own2s deadline, independent of phase idle.
    pub async fn poll(&mut self, plan: &Plan, clock: &dyn Clock) -> Result<Option<Request>> {
        if let Some(start) = self.partial_since {
            ensure!(clock.elapsed_ms() - start < 2000.0, Failure(124));
        }
        if self.eof {
            return Ok(None);
        }
        let mut bytes = [0; FRAME + 1];
        let count = self.channel.as_mut().unwrap().read(&mut bytes).await?;
        if count == 0 {
            ensure!(
                self.finished && self.acknowledged && self.buffer.is_empty(),
                "Adaptive control peer closed"
            );
            self.eof = true;
            return Ok(None);
        }
        ensure!(!self.finished, "Adaptive control already finished");
        if self.partial_since.is_none() {
            self.partial_since = Some(clock.elapsed_ms());
        }
        ensure!(
            self.buffer.len() + count <= FRAME,
            "Adaptive control frame limit"
        );
        self.buffer.extend_from_slice(&bytes[..count]);
        let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') else {
            return Ok(None);
        };
        ensure!(end + 1 == self.buffer.len(), "Extra adaptive control bytes");
        let request: Request = serde_json::from_slice(&self.buffer[..end])
            .map_err(|_| anyhow::anyhow!("Invalid adaptive control request"))?;
        ensure!(
            serde_json::to_vec(&request)? == self.buffer[..end],
            "Noncanonical adaptive control request"
        );
        ensure!(
            request.schema_version == 1
                && request.run_id == plan.run_id
                && request.sequence == self.sequence + 1
                && request.sequence <= self.steps,
            "Invalid adaptive control sequence"
        );
        let finalizing = request.phase == "finish";
        if plan.mode.tcp() {
            let expected = ["block", "baseline", "finish", "unblock"];
            ensure!(
                finalizing && !self.finalizing
                    || self.finalizing && request.phase == "unblock"
                    || !self.finalizing
                        && expected
                            .get(usize::from(self.sequence))
                            .is_some_and(|phase| *phase == request.phase),
                "Out-of-order TCP phase"
            );
            ensure!(
                request.profile.is_none()
                    && request.action
                        == match request.phase.as_str() {
                            "block" => "block-udp",
                            "unblock" => "unblock-udp",
                            "baseline" | "finish" => "clear",
                            _ => anyhow::bail!("Invalid TCP phase"),
                        },
                "Invalid TCP phase action"
            );
        } else {
            ensure!(
                finalizing
                    || PHASES
                        .get(usize::from(self.sequence))
                        .is_some_and(|phase| *phase == request.phase),
                "Out-of-order adaptive phase"
            );
            let clear = matches!(request.phase.as_str(), "baseline" | "recovery" | "finish");
            ensure!(
                if clear {
                    request.action == "clear" && request.profile.is_none()
                } else {
                    request.action == "apply" && request.profile.as_ref() == Some(&request.phase)
                },
                "Invalid adaptive phase action"
            );
        }
        self.sequence = request.sequence;
        self.finalizing |= finalizing;
        self.finished = if plan.mode.tcp() {
            request.phase == "unblock"
        } else {
            finalizing
        };
        self.acknowledged = false;
        self.buffer.clear();
        self.partial_since = None;
        Ok(Some(request))
    }
    /// The whole acknowledgement write has one2s bound. After uncertainty no
    /// caller retries it or assumes the peer received a successful transition.
    pub async fn reply(&mut self, plan: &Plan, status: u8) -> Result<()> {
        let mut bytes = serde_json::to_vec(&Reply {
            schema_version: 1,
            run_id: &plan.run_id,
            sequence: self.sequence,
            passed: status == 0,
            status,
        })?;
        bytes.push(b'\n');
        ensure!(bytes.len() <= FRAME, "Adaptive response limit");
        tokio::time::timeout(
            Duration::from_secs(2),
            self.channel.as_mut().unwrap().write(&bytes),
        )
        .await
        .map_err(|_| Failure(124))??;
        self.acknowledged = status == 0;
        Ok(())
    }
    /// After Node retirement, consume the terminal EOF before admitting success.
    /// Extra bytes cannot hide behind a valid final acknowledgement or receipt.
    pub async fn settle(&mut self) -> Result<()> {
        ensure!(self.passed(), "Adaptive control sequence incomplete");
        if !self.eof {
            let mut byte = [0; 1];
            let count = tokio::time::timeout(
                Duration::from_secs(2),
                self.channel.as_mut().unwrap().read(&mut byte),
            )
            .await
            .map_err(|_| Failure(124))??;
            ensure!(count == 0, "Extra adaptive control bytes");
            self.eof = true;
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<()> {
        self.channel.take().unwrap().finish()
    }
}

pub(super) fn profile(phase: &str) -> Result<super::super::netem::Profile> {
    let (loss, delay, jitter, rate, direction) = match phase {
        "lossy" => ("5", "50", "10", "0", "downlink"),
        "constrained" => ("0", "30", "0", "400", "downlink"),
        "severe" => ("0", "30", "0", "150", "downlink"),
        "lossyJoin" => ("5", "50", "10", "0", "both"),
        _ => anyhow::bail!("Invalid adaptive profile"),
    };
    super::super::netem::Profile::parse(
        &[
            ("IMPAIR_UDP_PORT".into(), "41100".into()),
            ("IMPAIR_WORKERS".into(), "1".into()),
            ("IMPAIR_LOSS".into(), loss.into()),
            ("IMPAIR_DELAY_MS".into(), delay.into()),
            ("IMPAIR_JITTER_MS".into(), jitter.into()),
            ("IMPAIR_RATE_KBIT".into(), rate.into()),
            ("IMPAIR_DIRECTION".into(), direction.into()),
        ]
        .into(),
    )
}

pub(super) fn services<'a>(services: &'a Services<'a>) -> super::super::controller::Services<'a> {
    super::super::controller::Services {
        host: services.host,
        clock: services.clock,
        ports: services.ports,
        readiness: services.readiness,
        authority: services.authority,
    }
}
