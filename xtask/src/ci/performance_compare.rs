//! One hosted owner for preparation, three builds and the unchanged six-run A/B profile.
//! Compiler/build-script and selected Git/runtime authority remain caller trusted.

mod execute;
pub(in crate::ci) mod files;
#[cfg(test)]
mod native_tests;
#[cfg(test)]
mod oracle_tests;
mod plan;
#[cfg(test)]
mod tests;
mod worktree;

use crate::{
    owned_process::Signals,
    tooling::{self, Failure, Result},
};
use std::{
    path::Path,
    process::ExitCode,
    time::{Duration, Instant},
};

pub(super) trait Clock {
    fn elapsed(&self) -> Duration;
}
struct NativeClock(Instant);
impl Clock for NativeClock {
    fn elapsed(&self) -> Duration {
        self.0.elapsed()
    }
}

struct Deadline<'a, C: Clock> {
    clock: &'a C,
}
impl<C: Clock> Deadline<'_, C> {
    fn body(&self, cleanup: bool, grace: Duration, maximum: Option<Duration>) -> Result<Duration> {
        let limit = if cleanup {
            plan::WATCHDOG
        } else {
            plan::WATCHDOG - plan::RESERVE
        };
        let remaining = limit
            .checked_sub(self.clock.elapsed())
            .and_then(|v| v.checked_sub(grace + plan::SETTLEMENT))
            .filter(|v| !v.is_zero())
            .ok_or_else(|| plan::failure(124))?;
        Ok(maximum.map_or(remaining, |maximum| maximum.min(remaining)))
    }
    fn check(&self, cleanup: bool) -> Result<()> {
        let limit = if cleanup {
            plan::WATCHDOG
        } else {
            plan::WATCHDOG - plan::RESERVE
        };
        if self.clock.elapsed() >= limit {
            Err(plan::failure(124))
        } else {
            Ok(())
        }
    }
}

pub(super) fn run(root: &Path, arguments: &[String]) -> anyhow::Result<ExitCode> {
    if arguments.len() == 1 && matches!(arguments[0].as_str(), "--help" | "-h") {
        println!("usage: cargo run --locked -q -p xtask -- ci performance-compare");
        return Ok(ExitCode::SUCCESS);
    }
    if !arguments.is_empty() {
        return Ok(ExitCode::from(2));
    }
    let environment = tooling::inherited_env();
    let result = (|| -> Result<()> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| plan::failure(1))?;
        runtime.block_on(async {
            let signals = Signals::new()?;
            let clock = NativeClock(Instant::now());
            let plan = plan::Plan::parse(root, environment, std::env::consts::OS)?;
            let mut native = super::process::Native::new(&plan.root, &plan.environment, signals);
            execute::execute(&plan, &mut native, &clock).await
        })
    })();
    match result {
        Ok(()) => Ok(ExitCode::SUCCESS),
        Err(Failure { code, .. }) => {
            eprintln!("Controlled performance comparison failed; inspect its private evidence.");
            Ok(ExitCode::from(code))
        }
    }
}
