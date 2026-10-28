//! Always-joined file work clipped to the original export phase and signal owner.

use super::{failure, keep, plan::Plan};
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
    pub clock: Arc<dyn super::super::acquire::AcquisitionClock>,
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
        super::super::acquire::AcquisitionCheckpoint::from_owner(
            self.clock.clone(),
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
) -> Result<T> {
    executor.ready().await?;
    let start = plan.now()?;
    if start >= end {
        return Err(failure(124));
    }
    let abort = Arc::new(AtomicBool::new(false));
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start,
        end,
        abort: abort.clone(),
    };
    let mut task = tokio::task::spawn_blocking(move || operation(checkpoint));
    let mut primary = None;
    let joined = loop {
        tokio::select! {biased;
            result=&mut task=>break result.map_err(|_|failure(1)),
            _=tokio::time::sleep(Duration::from_millis(10))=>{
                if let Err(error)=executor.ready().await{keep(&mut primary,error)}
                match plan.now(){Ok(now)if now>=end=>keep(&mut primary,failure(124)),
                    Err(error)=>keep(&mut primary,error),_=>{}}
                if primary.is_some(){abort.store(true,Ordering::SeqCst)}
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
    plan.check()?;
    Ok(value)
}
