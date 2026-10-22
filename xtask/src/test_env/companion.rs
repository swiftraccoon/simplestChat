//! An in-process companion shares the test command's existing signal owner.
//! Timer actions run while the workload group remains reserved; no task escapes.

use crate::owned_process::Signals;

pub(crate) trait Companion {
    /// Wait only for an in-memory event/timer. Do not acquire a subprocess here.
    async fn wait(&self);
    /// The selected action may borrow the same cancellation registration.
    async fn advance(&mut self, signals: &mut Signals) -> anyhow::Result<()>;
    /// Record the first terminal observation before retirement consumes reserve.
    /// This synchronous callback must not acquire resources or block.
    fn workload_ended(&mut self) {}
    /// Called after workload retirement, before server cleanup, including errors.
    async fn finish(&mut self, status: i32, signals: &mut Signals) -> i32;
}

pub(crate) struct NoCompanion;
impl Companion for NoCompanion {
    async fn wait(&self) {
        std::future::pending::<()>().await;
    }
    async fn advance(&mut self, _: &mut Signals) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&mut self, status: i32, _: &mut Signals) -> i32 {
        status
    }
}
