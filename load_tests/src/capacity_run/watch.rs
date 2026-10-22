//! Periodic cgroup, worker and namespace observations with a guarded CPU window.

use super::{
    context::Context,
    engine::Engine,
    parsers::{self, CpuStat, HostTimes},
    types::{Plan, StepResult},
};
use anyhow::{Result, bail};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub(super) struct Mark {
    pub(super) at: f64,
    pub(super) server: CpuStat,
    pub(super) generator: CpuStat,
    pub(super) host: HostTimes,
}
pub(super) type Marks = (Option<Mark>, Option<Mark>, Option<Mark>);

pub(super) async fn take_mark(
    context: &Context<'_>,
    server: &str,
    generator: &str,
) -> Result<Mark> {
    Ok(Mark {
        at: context.clock.now(),
        server: parsers::cpu_stat(
            &context
                .engine
                .read(server, "/sys/fs/cgroup/cpu.stat")
                .await?,
        )?,
        generator: parsers::cpu_stat(
            &context
                .engine
                .read(generator, "/sys/fs/cgroup/cpu.stat")
                .await?,
        )?,
        host: parsers::proc_stat(&context.engine.read(server, "/proc/stat").await?)?,
    })
}

pub(super) fn apply_marks(result: &mut StepResult, start: Mark, end: Mark) {
    let seconds = (end.at - start.at).max(1.0);
    result.server_cores = (end.server.usage_usec - start.server.usage_usec) as f64 / 1e6 / seconds;
    result.server_throttled = parsers::throttled(start.server, end.server);
    result.generator_cores =
        (end.generator.usage_usec - start.generator.usage_usec) as f64 / 1e6 / seconds;
    result.generator_throttled = parsers::throttled(start.generator, end.generator);
    let total = end.host.total - start.host.total;
    if total > 0 {
        result.host_steal = (end.host.steal - start.host.steal) as f64 / total as f64;
        result.host_busy = 1.0 - (end.host.idle - start.host.idle) as f64 / total as f64;
    }
}

pub(super) async fn scrape(
    engine: &dyn Engine,
    server: &str,
    token: &str,
) -> BTreeMap<String, f64> {
    engine
        .control(server, "/metrics", token)
        .await
        .map(|value| parsers::prometheus(&value))
        .unwrap_or_default()
}

pub(super) async fn wait_ready(context: &Context<'_>, server: &str) -> Result<()> {
    let deadline = context.clock.now() + 90.0;
    while context.clock.now() < deadline {
        if context.engine.control(server, "/health", "").await.is_ok() {
            return Ok(());
        }
        context.clock.sleep(0.5, context.cancellation).await?;
    }
    bail!("the server did not become healthy")
}

pub(super) struct Watch {
    window: (f64, f64),
    workers: i64,
    pub(super) setup: Option<Mark>,
    pub(super) start: Option<Mark>,
    pub(super) end: Option<Mark>,
    peaks: BTreeMap<i64, f64>,
    sums: BTreeMap<i64, f64>,
    scrapes: i64,
    next_scrape: f64,
    next_liveness: f64,
    next_memory: f64,
}
impl Watch {
    pub(super) fn new(window: (f64, f64), workers: i64) -> Self {
        Self {
            window,
            workers,
            setup: None,
            start: None,
            end: None,
            peaks: BTreeMap::new(),
            sums: BTreeMap::new(),
            scrapes: 0,
            next_scrape: 0.0,
            next_liveness: 0.0,
            next_memory: 0.0,
        }
    }
    pub(super) fn record(&mut self, metrics: &BTreeMap<String, f64>, result: &mut StepResult) {
        if metrics
            .get("simplestchat_cpu_saturated")
            .copied()
            .unwrap_or(0.0)
            >= 1.0
        {
            result.server_saturated_scrapes += 1;
        }
        let loads = parsers::worker_loads(metrics);
        if loads.len() < self.workers as usize {
            return;
        }
        self.scrapes += 1;
        for (worker, value) in loads {
            let peak = self.peaks.entry(worker).or_insert(0.0);
            *peak = peak.max(value);
            *self.sums.entry(worker).or_insert(0.0) += value;
        }
    }
    pub(super) fn finish(&self, result: &mut StepResult) {
        result.worker_load_peak = self.peaks.clone();
        result.load_scrapes = self.scrapes;
        if self.scrapes != 0 {
            result.worker_load_mean = self
                .sums
                .iter()
                .map(|(worker, sum)| (*worker, sum / self.scrapes as f64))
                .collect();
        }
    }
    async fn tick(
        &mut self,
        context: &Context<'_>,
        names: (&str, &str),
        token: &str,
        result: &mut StepResult,
    ) -> Result<bool> {
        let now = context.clock.now();
        let (server, generator) = names;
        if now >= self.next_liveness {
            self.next_liveness = now + 5.0;
            if !context.engine.running(generator).await? {
                return Ok(false);
            }
            if !context.engine.running(server).await? {
                result.server_exit = "the server stopped during the run".into();
                return Ok(false);
            }
        }
        if now > self.window.1 + 180.0 {
            result.error = "the generator did not finish before its deadline".into();
            return Ok(false);
        }
        if now >= self.next_memory {
            self.next_memory = now + 10.0;
            for (name, peak) in [
                (server, &mut result.memory_peak_bytes),
                (generator, &mut result.generator_memory_peak_bytes),
            ] {
                if let Ok(value) = context
                    .engine
                    .read(name, "/sys/fs/cgroup/memory.current")
                    .await
                {
                    let value = value.trim();
                    if !value.is_empty()
                        && value.bytes().all(|byte| byte.is_ascii_digit())
                        && let Ok(value) = value.parse::<i64>()
                    {
                        *peak = (*peak).max(value);
                    }
                }
            }
        }
        let marks: Result<()> = async {
            if self.setup.is_none() && now < self.window.0 {
                self.setup = Some(take_mark(context, server, generator).await?);
            }
            if self.start.is_none() && now >= self.window.0 {
                self.start = Some(take_mark(context, server, generator).await?);
                self.next_scrape = now + 10.0;
            }
            if self.end.is_none() && self.start.is_some() && now >= self.window.1 {
                self.end = Some(take_mark(context, server, generator).await?);
            }
            Ok(())
        }
        .await;
        if let Err(error) = marks
            && context.engine.running(server).await?
            && context.engine.running(generator).await?
        {
            return Err(error);
        }
        if self.start.is_some() && self.end.is_none() && now >= self.next_scrape {
            self.next_scrape = now + 10.0;
            self.record(&scrape(context.engine, server, token).await, result);
        }
        Ok(true)
    }
}

pub(super) async fn monitor(
    context: &Context<'_>,
    plan: Plan,
    names: (&str, &str),
    token: &str,
    result: &mut StepResult,
    marks: &mut Marks,
) -> Result<()> {
    let mut watch = Watch::new(plan.window(context.clock.now()), context.shape.workers);
    let observed: Result<()> = async {
        while watch.tick(context, names, token, result).await? {
            context.clock.sleep(1.0, context.cancellation).await?;
        }
        Ok(())
    }
    .await;
    // A failed read retains every complete prior scrape; it cannot become a pass.
    watch.finish(result);
    *marks = (watch.setup, watch.start, watch.end);
    observed
}
