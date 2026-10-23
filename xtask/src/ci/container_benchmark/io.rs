//! Private observations and fixed role identities; no general namespace runner.
use crate::{ci::command::Command, tooling::Result};
use serde_json::Value;
use std::{collections::BTreeMap, fs::File, sync::Arc, time::Duration};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Role {
    Server,
    Generator,
}
impl Role {
    pub const ALL: [Self; 2] = [Self::Server, Self::Generator];
    pub fn name(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Generator => "generator",
        }
    }
    pub fn index(self) -> usize {
        match self {
            Self::Server => 0,
            Self::Generator => 1,
        }
    }
}
#[derive(Clone, Copy)]
pub(super) enum Endpoint {
    Ready,
    Metrics,
}
impl Endpoint {
    pub fn path(self) -> &'static str {
        match self {
            Self::Ready => "/ready",
            Self::Metrics => "/metrics",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pub id: String,
    pub pid: u32,
    pub started_at: String,
}
pub(super) trait Namespace {
    fn check(&self, process: &Process) -> Result<()>;
    fn finish(&self) -> Result<()>;
}
pub(super) trait Groups {
    fn sample(&mut self, at: &str, terminal: [bool; 2]) -> Result<Vec<Value>>;
    fn finish(self) -> Result<()>;
}
pub(super) trait Observations {
    type Namespace: Namespace;
    type Groups: Groups;
    fn platform(&self) -> Result<()>;
    fn snapshot(&self) -> Result<BTreeMap<String, Vec<u8>>>;
    fn namespace(&self, process: &Process) -> Result<Arc<Self::Namespace>>;
    fn groups(&self, processes: &[Process; 2]) -> Result<Self::Groups>;
    fn curl(
        &self,
        namespace: &Arc<Self::Namespace>,
        endpoint: Endpoint,
        input: File,
        timeout: Duration,
        cleanup: bool,
    ) -> Result<Command>;
}

/// Reservations cover this owner's retained files and helper/probe allowances;
/// they are not a limit on descriptors inside the daemon or standard library.
#[derive(Clone, Default)]
pub(super) struct Budget(Arc<std::sync::atomic::AtomicUsize>);
pub(super) struct Permit {
    budget: Budget,
    count: usize,
}
impl Budget {
    pub fn reserve(&self, count: usize) -> Result<Permit> {
        use std::sync::atomic::Ordering;
        self.0
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                n.checked_add(count).filter(|n| *n <= 128)
            })
            .map_err(|_| super::failure(1))?;
        Ok(Permit {
            budget: self.clone(),
            count,
        })
    }
    #[cfg(test)]
    pub fn count(&self) -> usize {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}
impl Drop for Permit {
    fn drop(&mut self) {
        self.budget
            .0
            .fetch_sub(self.count, std::sync::atomic::Ordering::SeqCst);
    }
}
