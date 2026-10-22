//! One scheduled interruption and conservative same-ID restoration evidence.

use super::{
    engine::{Engine, Inspection, Operation, Request},
    plan::{self, Context},
};
use crate::{owned_process::Signals, test_env::Companion, tooling};
use serde::Serialize;
use std::time::{Duration, Instant};

pub(super) trait Clock {
    fn now(&self) -> Duration;
    async fn sleep(&self, duration: Duration);
}
pub(super) struct RealClock(pub Instant);
impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
    async fn sleep(&self, duration: Duration) {
        tokio::time::sleep(duration).await;
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Waiting,
    Stopped,
    Complete,
}
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Report {
    pub schema_version: u8,
    pub stop_attempted: bool,
    pub stopped_observed: bool,
    pub outage_completed: bool,
    pub restoration_observed: bool,
    pub docker_cli_groups_settled: bool,
    pub harness_status: i32,
    pub passed: bool,
}
pub(super) struct Supervisor<'a, E, C> {
    pub context: &'a Context,
    pub engine: E,
    pub clock: C,
    image: String,
    daemon: String,
    phase: Phase,
    next: Duration,
    finished: bool,
    completion_observed: Option<Duration>,
    pub report: Report,
}
impl<'a, E: Engine, C: Clock> Supervisor<'a, E, C> {
    #[cfg(test)]
    pub(super) fn test_identity(&mut self, image: String, daemon: String) {
        self.image = image;
        self.daemon = daemon;
    }
    pub fn new(context: &'a Context, engine: E, clock: C) -> Self {
        Self {
            context,
            engine,
            clock,
            image: String::new(),
            daemon: String::new(),
            phase: Phase::Waiting,
            next: plan::DELAY,
            finished: false,
            completion_observed: None,
            report: Report {
                schema_version: 1,
                ..Report::default()
            },
        }
    }
    fn budget(&self, deadline: Duration, desired: Duration) -> tooling::Result<Duration> {
        let left = deadline
            .checked_sub(self.clock.now())
            .and_then(|v| v.checked_sub(plan::CLI_SETTLE))
            .filter(|v| !v.is_zero())
            .ok_or_else(|| plan::failed(124))?;
        Ok(left.min(desired))
    }
    async fn call(
        &mut self,
        operation: Operation,
        cleanup: bool,
        deadline: Duration,
        signals: &mut Signals,
    ) -> tooling::Result<Vec<u8>> {
        if matches!(
            operation,
            Operation::Inspect | Operation::Stop | Operation::Start
        ) {
            self.check_daemon(cleanup, deadline, signals).await?;
        }
        self.raw_call(operation, cleanup, deadline, signals).await
    }
    async fn raw_call(
        &mut self,
        operation: Operation,
        cleanup: bool,
        deadline: Duration,
        signals: &mut Signals,
    ) -> tooling::Result<Vec<u8>> {
        let desired =
            Duration::from_secs(if matches!(operation, Operation::Stop | Operation::Start) {
                20
            } else {
                5
            });
        let timeout = self.budget(deadline, desired)?;
        let output = self
            .engine
            .call(
                Request {
                    operation,
                    timeout,
                    cleanup,
                },
                signals,
            )
            .await?;
        if self.clock.now() >= deadline {
            return Err(plan::failed(124));
        }
        Ok(output)
    }
    async fn check_daemon(
        &mut self,
        cleanup: bool,
        deadline: Duration,
        signals: &mut Signals,
    ) -> tooling::Result<()> {
        let bytes = self
            .raw_call(Operation::Daemon, cleanup, deadline, signals)
            .await?;
        let observed: String = serde_json::from_slice(&bytes).map_err(|_| plan::failed(1))?;
        if observed != self.daemon {
            return Err(plan::failed(1));
        }
        Ok(())
    }
    async fn inspect(
        &mut self,
        cleanup: bool,
        deadline: Duration,
        signals: &mut Signals,
    ) -> tooling::Result<Inspection> {
        let bytes = self
            .call(Operation::Inspect, cleanup, deadline, signals)
            .await?;
        Inspection::decode(&bytes, self.context, &self.image)
    }
    pub async fn admit(&mut self, signals: &mut Signals) -> tooling::Result<()> {
        let deadline = self.clock.now() + Duration::from_secs(60);
        let bytes = self
            .raw_call(Operation::Daemon, false, deadline, signals)
            .await?;
        let daemon: String = serde_json::from_slice(&bytes).map_err(|_| plan::failed(1))?;
        // Docker documents UUID and colon-group forms. This is an opaque
        // observed identity, not a globally unique/authenticated digest.
        // https://docs.docker.com/reference/cli/docker/system/info/
        if daemon.is_empty() || daemon.len() > 256 || daemon.chars().any(char::is_control) {
            return Err(plan::failed(1));
        }
        self.daemon = daemon;
        let bytes = self
            .call(Operation::Image, false, deadline, signals)
            .await?;
        let image: String = serde_json::from_slice(&bytes).map_err(|_| plan::failed(1))?;
        self.image = plan::image_id(&image)
            .ok_or_else(|| plan::failed(1))?
            .to_owned();
        if !self.inspect(false, deadline, signals).await?.healthy() || !self.engine.settled() {
            return Err(plan::failed(1));
        }
        Ok(())
    }
    async fn restore(
        &mut self,
        cleanup: bool,
        deadline: Duration,
        signals: &mut Signals,
    ) -> tooling::Result<()> {
        let mut started = false;
        for _ in 0..60 {
            let state = self.inspect(cleanup, deadline, signals).await?;
            if state.healthy() {
                self.report.restoration_observed = true;
                return Ok(());
            }
            if state.may_start() && !started {
                self.call(Operation::Start, cleanup, deadline, signals)
                    .await?;
                started = true;
                continue;
            }
            if !state.may_settle() {
                return Err(plan::failed(1));
            }
            if self.clock.now() + Duration::from_secs(1) + plan::CLI_SETTLE >= deadline {
                return Err(plan::failed(124));
            }
            self.clock.sleep(Duration::from_secs(1)).await;
        }
        Err(plan::failed(124))
    }
}
impl<E: Engine, C: Clock> Companion for Supervisor<'_, E, C> {
    fn workload_ended(&mut self) {
        self.completion_observed
            .get_or_insert_with(|| self.clock.now());
    }
    async fn wait(&self) {
        let next = if self.phase == Phase::Complete {
            plan::WATCHDOG
        } else {
            self.next.min(plan::WATCHDOG)
        };
        self.clock
            .sleep(next.saturating_sub(self.clock.now()))
            .await;
    }
    async fn advance(&mut self, signals: &mut Signals) -> anyhow::Result<()> {
        if self.clock.now() >= plan::WATCHDOG {
            return Err(plan::failed(124).into());
        }
        if let Some(code) = signals.pending().await {
            return Err(plan::failed(code).into());
        }
        let deadline = (self.clock.now() + Duration::from_secs(60)).min(plan::WATCHDOG);
        match self.phase {
            Phase::Waiting => {
                if self.clock.now() < plan::DELAY {
                    return Err(plan::failed(1).into());
                }
                if !self.inspect(false, deadline, signals).await?.healthy() {
                    return Err(plan::failed(1).into());
                }
                self.report.stop_attempted = true;
                self.call(Operation::Stop, false, deadline, signals).await?;
                if !self.inspect(false, deadline, signals).await?.stopped() {
                    return Err(plan::failed(1).into());
                }
                self.report.stopped_observed = true;
                self.phase = Phase::Stopped;
                self.next = self.clock.now() + plan::OUTAGE;
            }
            Phase::Stopped => {
                if self.clock.now() < self.next {
                    return Err(plan::failed(1).into());
                }
                if !self.inspect(false, deadline, signals).await?.stopped() {
                    return Err(plan::failed(1).into());
                }
                self.report.outage_completed = true;
                self.restore(false, deadline, signals).await?;
                self.phase = Phase::Complete;
            }
            Phase::Complete => return Err(plan::failed(124).into()),
        }
        Ok(())
    }
    async fn finish(&mut self, mut status: i32, signals: &mut Signals) -> i32 {
        if status == 0
            && self.completion_observed.unwrap_or_else(|| self.clock.now()) >= plan::WATCHDOG
        {
            status = 124;
        }
        if self.finished {
            return status;
        }
        self.finished = true;
        if self.report.stop_attempted {
            self.report.restoration_observed = false;
            let deadline =
                (self.clock.now() + plan::RESTORE).min(plan::STEP - plan::HARNESS_RESERVE);
            if self.restore(true, deadline, signals).await.is_err() && status == 0 {
                status = 1;
            }
        }
        self.report.docker_cli_groups_settled = self.engine.settled();
        if status == 0
            && (!self.report.outage_completed
                || !self.report.restoration_observed
                || !self.report.docker_cli_groups_settled)
        {
            status = 1;
        }
        if status == 0
            && let Some(code) = signals.pending().await
        {
            status = i32::from(code);
        }
        status
    }
}
