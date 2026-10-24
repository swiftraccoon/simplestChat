//! One cooperative file worker, always joined before its owners can retire.

use super::{failure, plan::Context};
use crate::{ci::process::Executor, tooling::Result};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(in crate::ci::ops_artifact) struct Checkpoint {
    abort: Arc<AtomicBool>,
    clock: Arc<dyn super::plan::Clock>,
    start: Instant,
    end: Instant,
}
impl Checkpoint {
    pub(in crate::ci::ops_artifact) fn from_owner(
        clock: Arc<dyn super::plan::Clock>,
        start: Instant,
        end: Instant,
        abort: Arc<AtomicBool>,
    ) -> Self {
        Self {
            abort,
            clock,
            start,
            end,
        }
    }
    #[cfg(test)]
    pub fn fixture() -> Self {
        let clock: Arc<dyn super::plan::Clock> = Arc::new(super::plan::NativeClock);
        let start = clock.now();
        Self {
            abort: Arc::new(AtomicBool::new(false)),
            clock,
            start,
            end: start + super::plan::ARCHIVE,
        }
    }
    pub fn check(&self) -> Result<()> {
        let now = self.clock.now();
        if now < self.start {
            return Err(failure(1));
        }
        if self.abort.load(Ordering::SeqCst) || now >= self.end {
            return Err(failure(124));
        }
        Ok(())
    }
    pub fn retained(&self) -> super::super::verify::RetainedCheckpoint {
        struct Clock(Arc<dyn super::plan::Clock>);
        impl super::super::verify::RetainedClock for Clock {
            fn now(&self) -> Instant {
                self.0.now()
            }
        }
        super::super::verify::RetainedCheckpoint::from_owner(
            Arc::new(Clock(self.clock.clone())),
            self.start,
            self.end,
            self.abort.clone(),
        )
    }
}
pub(super) async fn work<T: Send + 'static, E: Executor>(
    context: &Context,
    executor: &mut E,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    executor.ready().await?;
    let (start, slot) = context.archive_window()?;
    let abort = Arc::new(AtomicBool::new(false));
    let checkpoint = Checkpoint {
        abort: abort.clone(),
        clock: context.clock.clone(),
        start,
        end: start + slot,
    };
    let mut worker = tokio::task::spawn_blocking(move || operation(checkpoint));
    let mut primary = None;
    let joined = loop {
        tokio::select! {
            biased;
            result = &mut worker => break result.map_err(|_| failure(1)),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Err(error) = executor.ready().await { super::keep(&mut primary, error); }
                match context.now() {
                    Ok(now) if now >= start + slot => super::keep(&mut primary, failure(124)),
                    Err(error) => super::keep(&mut primary, error),
                    _ => {}
                }
                if primary.is_some() { abort.store(true, Ordering::SeqCst); }
            }
        }
    };
    let end = context.now();
    let accounted = match &end {
        Ok(end) => context.complete_archive(end.saturating_duration_since(start)),
        Err(_) => Ok(()),
    };
    if let Some(error) = primary {
        return Err(error);
    }
    let value = joined??;
    end?;
    accounted?;
    executor.ready().await?;
    if context.now()? >= start + slot {
        return Err(failure(124));
    }
    context.admit()?;
    Ok(value)
}
