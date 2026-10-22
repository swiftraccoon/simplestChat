//! Docker metadata also uses the existing combined pipe's explicit finish path.

use super::engine::{Policy, Scope};
use super::{files::Artifacts, plan::failure};
use crate::{
    ci::{
        bounded_log,
        command::{Command, Program},
        process::Executor,
    },
    tooling::Result,
};
use std::{sync::Arc, time::Instant};

/// One same-signal Native executor underneath this bounded file adapter. Mixed
/// metadata/diagnostic bytes refuse the strict metadata decoder; none are echoed.
pub(super) struct Capture<'a, E> {
    pub inner: &'a mut E,
    pub artifacts: &'a Artifacts,
    pub started: Instant,
}
fn available(started: Instant, request: &Command) -> Result<std::time::Duration> {
    let policy = Policy::validation();
    let limit = if request.cleanup {
        policy.watchdog()
    } else {
        policy.watchdog() - policy.reserve()
    };
    limit
        .checked_sub(started.elapsed())
        .and_then(|left| left.checked_sub(request.grace + std::time::Duration::from_secs(20)))
        .filter(|left| !left.is_zero())
        .ok_or_else(|| Scope::Validation.expired())
}
impl<E: Executor> Executor for Capture<'_, E> {
    async fn run(&mut self, mut request: Command) -> Result<Vec<u8>> {
        if request.program != Program::Docker
            || request.combined_log.is_some()
            || request.stdout.is_some()
        {
            return Err(failure());
        }
        if !request.cleanup {
            self.inner.ready().await?;
        }
        request.timeout = request.timeout.min(available(self.started, &request)?);
        let maximum = request.capture.take().ok_or_else(failure)?;
        let (file, log) = self.artifacts.log(maximum)?;
        request.combined_log = Some(Arc::new(bounded_log::Log::start(file, maximum)?));
        // Prefix/log admission can block in the filesystem; recheck before the
        // only dispatch and explicitly finish an unforwarded pipe on refusal.
        match available(self.started, &request) {
            Ok(left) => request.timeout = request.timeout.min(left),
            Err(error) => {
                if let Some(log) = request.combined_log.take()
                    && let Ok(log) = Arc::try_unwrap(log)
                {
                    let _ = log.finish().await;
                }
                return Err(error);
            }
        }
        let request_cleanup = request.cleanup;
        let result = self.inner.run(request).await;
        // The original file owner has finished, including setup/spawn failures.
        // Read only its admitted inode through the retained namespace.
        let read = self.artifacts.read_log(&log);
        result?;
        let bytes = read?;
        let policy = Policy::validation();
        let limit = if request_cleanup {
            policy.watchdog()
        } else {
            policy.watchdog() - policy.reserve()
        };
        if self.started.elapsed() >= limit {
            return Err(Scope::Validation.expired());
        }
        Ok(bytes)
    }
    async fn ready(&mut self) -> Result<()> {
        self.inner.ready().await
    }
    fn cleanup_passed(&self) -> bool {
        self.inner.cleanup_passed()
    }
    async fn replay(&mut self, bytes: &[u8], stderr: bool, deadline: Instant) -> Result<()> {
        self.inner.replay(bytes, stderr, deadline).await
    }
}
