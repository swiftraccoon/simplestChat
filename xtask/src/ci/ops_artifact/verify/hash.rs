//! One awaited filesystem worker; cancellation requests abort, never detaches it.

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
pub(super) struct Checkpoint {
    abort: Arc<AtomicBool>,
    clock: Arc<dyn super::plan::Clock>,
    start: Instant,
    end: Instant,
}
impl Checkpoint {
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
    #[cfg(test)]
    pub fn fixture() -> Self {
        let clock: Arc<dyn super::plan::Clock> = Arc::new(super::plan::NativeClock);
        let start = clock.now();
        Self {
            abort: Arc::new(AtomicBool::new(false)),
            clock,
            start,
            end: start + Duration::from_secs(120),
        }
    }
}

/// The operation owns every acquired descriptor until it explicitly closes it.
/// An abort is cooperative; filesystem and mature ELF-helper stalls are not
/// hard bounded. Even after cancellation this function always joins the worker.
pub(super) async fn work<T: Send + 'static, E: Executor>(
    context: &Context,
    executor: &mut E,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    executor.ready().await?;
    let (start, slot) = context.hash_window()?;
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
    if let Ok(end) = end {
        context
            .hash_used
            .set(context.hash_used.get() + end.saturating_duration_since(start));
    }
    if let Some(error) = primary {
        return Err(error);
    }
    // A known worker failure precedes a subsequently observed pending signal.
    let value = joined??;
    end?;
    executor.ready().await?;
    if context.now()? >= start + slot {
        return Err(failure(124));
    }
    context.admit()?;
    Ok(value)
}
