//! Always join cooperative file work, including after interruption or expiry.

use super::{
    failure, keep,
    plan::{Clock, Plan},
};
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
    pub clock: Arc<dyn Clock>,
    pub origin: Instant,
    pub end: Instant,
    pub abort: Arc<AtomicBool>,
}
impl Checkpoint {
    pub fn check(&self) -> Result<()> {
        let now = self.clock.now();
        if now < self.origin {
            return Err(failure(1));
        }
        if self.abort.load(Ordering::SeqCst) || now >= self.end {
            return Err(failure(124));
        }
        Ok(())
    }
}
pub(super) async fn work<T: Send + 'static, E: Executor>(
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
) -> Result<T> {
    executor.ready().await?;
    plan.now()?;
    let abort = Arc::new(AtomicBool::new(false));
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        origin: plan.origin(),
        end,
        abort: abort.clone(),
    };
    checkpoint.check()?;
    let mut task = tokio::task::spawn_blocking(move || operation(checkpoint));
    let mut primary = None;
    let joined = loop {
        tokio::select! { biased;
            result = &mut task => break result.map_err(|_| failure(1)),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Err(error) = executor.ready().await { keep(&mut primary, error); }
                match plan.now() {
                    Ok(now) if now >= end => keep(&mut primary, failure(124)),
                    Err(error) => keep(&mut primary, error), _ => {}
                }
                if primary.is_some() { abort.store(true, Ordering::SeqCst); }
            }
        }
    };
    if let Some(error) = primary {
        return Err(error);
    }
    let value = joined??;
    executor.ready().await?;
    if plan.now()? >= end {
        return Err(failure(124));
    }
    Ok(value)
}
