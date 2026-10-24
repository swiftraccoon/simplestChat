//! Always-joined blocking file work sharing the enclosing clock and signal owner.

use super::{
    failure, keep,
    plan::{self, Plan},
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
    pub clock: Arc<dyn plan::Clock>,
    pub start: Instant,
    pub end: Instant,
    pub abort: Arc<AtomicBool>,
}
impl Checkpoint {
    pub fn check(&self) -> Result<()> {
        let now = self.clock.now();
        if now < self.start {
            return Err(failure(1));
        }
        if self.abort.load(Ordering::SeqCst) || now >= self.end {
            Err(failure(124))
        } else {
            Ok(())
        }
    }
    pub fn acquired(&self) -> super::super::acquire::AcquisitionCheckpoint {
        struct Clock(Arc<dyn plan::Clock>);
        impl super::super::acquire::AcquisitionClock for Clock {
            fn now(&self) -> Instant {
                self.0.now()
            }
        }
        super::super::acquire::AcquisitionCheckpoint::from_owner(
            Arc::new(Clock(self.clock.clone())),
            self.start,
            self.end,
            self.abort.clone(),
        )
    }
}

pub(super) async fn work<T: Send + 'static, E: Executor>(
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
    settle: impl FnOnce(T) -> Result<()> + Send + 'static,
) -> Result<T> {
    let mut primary = executor.ready().await.err();
    let start = match plan.now() {
        Ok(now) => now,
        Err(error) => {
            keep(&mut primary, error);
            plan.clock.now()
        }
    };
    if start >= end {
        keep(&mut primary, failure(124));
    }
    let abort = Arc::new(AtomicBool::new(primary.is_some()));
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start,
        end,
        abort: abort.clone(),
    };
    let mut worker = tokio::task::spawn_blocking(move || operation(checkpoint));
    let result = loop {
        tokio::select! { biased;
            result = &mut worker => break result.map_err(|_| failure(1)),
            _ = tokio::time::sleep(Duration::from_millis(10)) => {
                if let Err(error) = executor.ready().await { keep(&mut primary, error); }
                match plan.now() {
                    Ok(now) if now >= end => keep(&mut primary, failure(124)),
                    Err(error) => keep(&mut primary, error),
                    _ => {}
                }
                if primary.is_some() { abort.store(true, Ordering::SeqCst); }
            }
        }
    };
    if let Some(error) = primary {
        if let Ok(Ok(value)) = result {
            let _ = tokio::task::spawn_blocking(move || settle(value)).await;
        }
        return Err(error);
    }
    let value = result??;
    let pending = executor.ready().await;
    let timely = plan.now().and_then(|now| {
        if now >= end {
            Err(failure(124))
        } else {
            plan.check()
        }
    });
    if let Err(error) = pending.and(timely) {
        let _ = tokio::task::spawn_blocking(move || settle(value)).await;
        return Err(error);
    }
    Ok(value)
}
