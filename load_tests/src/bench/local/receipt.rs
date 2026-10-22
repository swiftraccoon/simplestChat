//! Optional CI receipt over this invocation's owned native groups. The audit
//! decorates the existing host; it neither supervises nor replays process IDs.

mod files;
#[cfg(test)]
mod tests;

use super::{Services, controller};
use crate::bench::{
    options::Options,
    process::{Child, Command, Exit, Host, NativeHost, Retirement, Stream},
};
use anyhow::{Result, ensure};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    ffi::{OsStr, OsString},
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

pub(super) fn run_id(environment: &BTreeMap<OsString, OsString>) -> Result<Option<String>> {
    let Some(value) = environment.get(OsStr::new("BENCH_CI_RUN_ID")) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("Invalid CI run ID"))?;
    ensure!(
        uuid::Uuid::parse_str(value).is_ok_and(|id| id.hyphenated().to_string() == value),
        "Invalid CI run ID"
    );
    Ok(Some(value.into()))
}

#[derive(Default)]
struct Facts {
    spawned: usize,
    retired: usize,
    runs: usize,
    uncertain: bool,
    group_failed: bool,
    streams_failed: bool,
}

pub(super) struct ObservedHost<'a> {
    host: &'a dyn Host,
    facts: Arc<Mutex<Facts>>,
    output: Mutex<Option<files::Output>>,
}
impl<'a> ObservedHost<'a> {
    pub fn new(host: &'a dyn Host) -> Self {
        Self {
            host,
            facts: Arc::default(),
            output: Mutex::default(),
        }
    }
    pub fn bind_output(&self, path: &Path) -> Result<()> {
        let output = files::Output::open(path)?;
        let mut slot = self.output.lock().expect("CI output owner");
        ensure!(slot.is_none(), "CI output already bound");
        *slot = Some(output);
        Ok(())
    }
    pub fn completed_run(&self) {
        self.facts.lock().expect("CI process facts").runs += 1;
    }
    pub(super) fn receipt<'b>(
        &self,
        run_id: &'b str,
        completed: bool,
        passed: bool,
    ) -> Receipt<'b> {
        let facts = self.facts.lock().expect("CI process facts");
        let retired = facts.spawned == facts.retired && !facts.uncertain;
        let groups = retired && !facts.group_failed;
        let streams = retired && !facts.streams_failed;
        Receipt {
            schema_version: 1,
            run_id,
            completed,
            passed: passed && completed && facts.runs == 6 && groups && streams,
            runs: facts.runs,
            group_cleanup_passed: groups,
            streams_closed: streams,
        }
    }
    pub(super) fn publish(&self, receipt: &Receipt<'_>) -> Result<()> {
        let output = self
            .output
            .lock()
            .expect("CI output owner")
            .take()
            .ok_or_else(|| anyhow::anyhow!("CI output unavailable"))?;
        output.publish(receipt)
    }
}

struct ObservedChild {
    child: Box<dyn Child>,
    facts: Arc<Mutex<Facts>>,
    retired: bool,
}
#[async_trait::async_trait]
impl Child for ObservedChild {
    fn pid(&self) -> u32 {
        self.child.pid()
    }
    fn retirement(&self) -> Option<Retirement> {
        self.child.retirement()
    }
    fn take_stdout(&mut self) -> Option<Box<dyn Stream>> {
        let stream = self.child.take_stdout();
        if stream.is_some() {
            self.facts.lock().expect("CI process facts").uncertain = true;
        }
        stream
    }
    async fn poll(&mut self) -> Result<Option<Exit>> {
        self.child.poll().await
    }
    async fn stop(&mut self, grace: Duration) -> Result<Exit> {
        let result = self.child.stop(grace).await;
        let facts = self.child.retirement();
        let mut retained = self.facts.lock().expect("CI process facts");
        if !self.retired {
            self.retired = true;
            retained.retired += 1;
        }
        retained.uncertain |= result.is_err();
        retained.group_failed |= !facts.as_ref().is_some_and(|v| v.group_cleanup_passed);
        retained.streams_failed |= !facts.as_ref().is_some_and(|v| v.streams_closed);
        result
    }
    fn output(&self) -> &[u8] {
        self.child.output()
    }
    fn errors(&self) -> &[u8] {
        self.child.errors()
    }
}
impl Drop for ObservedChild {
    fn drop(&mut self) {
        if !self.retired {
            self.facts.lock().expect("CI process facts").uncertain = true;
        }
    }
}
#[async_trait::async_trait]
impl Host for ObservedHost<'_> {
    fn spawn(&self, command: Command) -> Result<Box<dyn Child>> {
        let child = match self.host.spawn(command) {
            Ok(child) => child,
            Err(error) => {
                // Spawn can fail after native stream admission started. An
                // optional observation must not erase that uncertainty.
                self.facts.lock().expect("CI process facts").uncertain = true;
                return Err(error);
            }
        };
        self.facts.lock().expect("CI process facts").spawned += 1;
        Ok(Box::new(ObservedChild {
            child,
            facts: self.facts.clone(),
            retired: false,
        }))
    }
    async fn cancelled(&self) -> u8 {
        self.host.cancelled().await
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Receipt<'a> {
    schema_version: u8,
    run_id: &'a str,
    completed: bool,
    passed: bool,
    runs: usize,
    group_cleanup_passed: bool,
    streams_closed: bool,
}

async fn pending(
    interrupt: &mut tokio::signal::unix::Signal,
    terminate: &mut tokio::signal::unix::Signal,
) -> u8 {
    tokio::select! {
        biased;
        _ = interrupt.recv() => 130,
        _ = terminate.recv() => 143,
        () = std::future::ready(()) => 0,
    }
}

fn primary(outcome: &Result<serde_json::Value>, cancellation: u8) -> u8 {
    let known = outcome.as_ref().err().and_then(|error| {
        crate::bench::process::failure_status(error)
            .or_else(|| crate::bench::environment::failure_status(error))
    });
    known.unwrap_or_else(|| {
        if cancellation != 0 {
            cancellation
        } else if outcome.as_ref().is_ok_and(|value| value["passed"] == true) {
            0
        } else {
            1
        }
    })
}

/// The signal task returns its streams after all controller finalizers. The
/// caller still observes pending signals around receipt publication/close.
pub(super) async fn execute(
    run_id: &str,
    options: &Options,
    services: &Services<'_>,
    executable: &Path,
    native: &Arc<NativeHost>,
    observed: &ObservedHost<'_>,
) -> Result<(bool, u8)> {
    use tokio::signal::unix::{SignalKind, signal};
    let inputs = files::Inputs::capture(options, executable)?;
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    let (stop, mut stopped) = tokio::sync::oneshot::channel::<()>();
    let owner = native.clone();
    let signals = tokio::spawn(async move {
        loop {
            tokio::select! {
                biased;
                _ = &mut stopped => break,
                _ = interrupt.recv() => owner.cancel(130),
                _ = terminate.recv() => owner.cancel(143),
            }
        }
        (interrupt, terminate)
    });
    let outcome = controller::execute_recorded(options, services, executable, observed).await;
    let _ = stop.send(());
    let (mut interrupt, mut terminate) = signals.await?;
    let inputs = inputs.finish();
    native.cancel(pending(&mut interrupt, &mut terminate).await);
    let mut code = primary(&outcome, native.code());
    if code == 0 && inputs.is_err() {
        code = 1;
    }
    let receipt = observed.receipt(
        run_id,
        outcome.as_ref().is_ok_and(|v| v["completed"] == true),
        code == 0,
    );
    let passed = receipt.passed;
    let published = observed.publish(&receipt);
    native.cancel(pending(&mut interrupt, &mut terminate).await);
    if code == 0 {
        code = native.code();
    }
    if code != 0 {
        return Ok((false, code));
    }
    published?;
    ensure!(passed, "CI process retirement unconfirmed");
    Ok((true, 0))
}
