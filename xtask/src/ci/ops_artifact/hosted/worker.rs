//! Filesystem work is cooperatively aborted and always joined while the same
//! Native signal owner remains responsive. This is no OS-I/O stall guarantee.

use super::{
    failure,
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
    clock: Arc<dyn plan::Clock>,
    start: Instant,
    end: Instant,
    abort: Arc<AtomicBool>,
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
}
pub(super) async fn work<T: Send + 'static, E: Executor>(
    plan: &Plan,
    executor: &mut E,
    end: Instant,
    operation: impl FnOnce(Checkpoint) -> Result<T> + Send + 'static,
    settle: impl FnOnce(T) -> Result<()> + Send + 'static,
) -> Result<T> {
    // The closure may already own a File. Even admission failure must execute
    // its aborted settlement path; dropping the closure cannot check close.
    let mut primary = executor.ready().await.err();
    let start = match plan.now() {
        Ok(start) => start,
        Err(error) => {
            if primary.is_none() {
                primary = Some(error);
            }
            plan.clock.now()
        }
    };
    if start >= end && primary.is_none() {
        primary = Some(failure(124));
    }
    let abort = Arc::new(AtomicBool::new(false));
    let checkpoint = Checkpoint {
        clock: plan.clock.clone(),
        start,
        end,
        abort: abort.clone(),
    };
    if primary.is_some() {
        abort.store(true, Ordering::SeqCst);
    }
    let mut worker = tokio::task::spawn_blocking(move || operation(checkpoint));
    let joined = loop {
        tokio::select! {biased;
            value=&mut worker=>break value.map_err(|_|failure(1)),
            _=tokio::time::sleep(Duration::from_millis(10))=>{
                if let Err(error)=executor.ready().await && primary.is_none() {primary=Some(error);}
                match plan.now(){
                    Ok(now) if now>=end && primary.is_none()=>primary=Some(failure(124)),
                    Err(error) if primary.is_none()=>primary=Some(error),
                    _=>{}
                }
                if primary.is_some(){abort.store(true,Ordering::SeqCst);}
            }
        }
    };
    if let Some(error) = primary {
        if let Ok(Ok(value)) = joined {
            let _ = tokio::task::spawn_blocking(move || settle(value)).await;
        }
        return Err(error);
    }
    let value = joined??;
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
