//! All complete slots derive from one entry; saved-run lookup can only tighten it.

use super::{Result, failure, options};
use std::{
    cell::Cell,
    sync::Arc,
    time::{Duration, Instant},
};

pub(in crate::ci::ops_artifact::capacity_vps) trait Clock:
    Send + Sync
{
    fn now(&self) -> Instant;
}
pub(super) struct NativeClock;
impl Clock for NativeClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Phase {
    Admission,
    Acquisition,
    Stage,
    Preflight,
    Prepare,
    Build,
    Run,
    Recover,
    Collect,
    Archive,
    Extract,
    Retire,
    Finalize,
    Replay,
    Fence,
}
pub(super) struct Plan {
    pub clock: Arc<dyn Clock>,
    pub start: Instant,
    last: Cell<Instant>,
    end: Cell<Instant>,
    phases: Vec<(Phase, Duration)>,
    next: Cell<usize>,
    runtime: Cell<u64>,
}
impl Plan {
    pub fn new(clock: Arc<dyn Clock>, options: &options::Options, start: Instant) -> Self {
        let runtime = options.runtime();
        let mut phases = vec![
            (Phase::Admission, 150),
            (Phase::Acquisition, 1440),
            (Phase::Stage, 360),
            (Phase::Preflight, 90),
        ];
        if options.prepare {
            phases.push((Phase::Prepare, 1800));
        }
        if options.build {
            phases.push((Phase::Build, 6540));
        }
        phases.push((Phase::Run, runtime + 750));
        if options.mode == options::Mode::Recover {
            phases.push((Phase::Recover, 660));
        }
        phases.extend([
            (Phase::Collect, 90),
            (Phase::Archive, 360),
            (Phase::Extract, 600),
            (Phase::Retire, 90),
            (Phase::Finalize, 120),
            (Phase::Replay, 30),
            (Phase::Fence, 30),
        ]);
        let phases: Vec<_> = phases
            .into_iter()
            .map(|(p, s)| (p, Duration::from_secs(s)))
            .collect();
        let total: Duration = phases.iter().map(|(_, d)| *d).sum();
        Self {
            clock,
            start,
            last: Cell::new(start),
            end: Cell::new(start + total + Duration::from_secs(300)),
            phases,
            next: Cell::new(0),
            runtime: Cell::new(runtime),
        }
    }
    pub fn now(&self) -> Result<Instant> {
        let now = self.clock.now();
        if now < self.start || now < self.last.get() {
            return Err(failure(1));
        }
        self.last.set(now);
        Ok(now)
    }
    pub fn end(&self) -> Instant {
        self.end.get()
    }
    pub fn check(&self) -> Result<()> {
        if self.now()? >= self.end() {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    fn remaining(&self) -> Duration {
        self.phases[self.next.get()..]
            .iter()
            .map(|(p, d)| {
                if *p == Phase::Run {
                    Duration::from_secs(self.runtime.get() + 750)
                } else {
                    *d
                }
            })
            .sum()
    }
    pub fn begin(&self, phase: Phase) -> Result<Instant> {
        let Some((selected, slot)) = self.phases.get(self.next.get()) else {
            return Err(failure(1));
        };
        if *selected != phase {
            return Err(failure(1));
        }
        let now = self.now()?;
        if self.end().saturating_duration_since(now) < self.remaining() {
            return Err(failure(124));
        }
        Ok(now
            + if phase == Phase::Run {
                Duration::from_secs(self.runtime.get() + 750)
            } else {
                *slot
            })
    }
    pub fn complete(&self, phase: Phase, end: Instant) -> Result<()> {
        if self.phases.get(self.next.get()).map(|v| v.0) != Some(phase) {
            return Err(failure(1));
        }
        if self.now()? >= end {
            return Err(failure(124));
        }
        self.next.set(self.next.get() + 1);
        self.check()
    }
    pub fn tighten(&self, runtime: u64) -> Result<()> {
        if !(120..=21600).contains(&runtime) || runtime > self.runtime.get() {
            return Err(failure(1));
        }
        let delta = self.runtime.get() - runtime;
        self.end.set(
            self.end()
                .checked_sub(Duration::from_secs(delta))
                .ok_or_else(|| failure(124))?,
        );
        self.runtime.set(runtime);
        self.check()
    }
    pub fn dispatch(
        &self,
        end: Instant,
        maximum: Duration,
        grace: Duration,
    ) -> Result<(Duration, Instant)> {
        let now = self.now()?;
        let tail = grace + Duration::from_secs(20);
        let remaining = end.saturating_duration_since(now);
        if remaining <= tail + Duration::from_secs(5) {
            return Err(failure(124));
        }
        let body = maximum.min(remaining - tail - Duration::from_secs(5));
        let latest = end.checked_sub(body + tail).ok_or_else(|| failure(124))?;
        Ok((body, latest))
    }
}
