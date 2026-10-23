//! Tiny owned inputs and inert bootstrap tools. No component or candidate runs.

use super::plan;
use crate::{
    ci::{command::Command, process::Executor},
    tooling::Result,
};
use std::{
    fs,
    os::unix::fs::DirBuilderExt,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

static NEXT: AtomicUsize = AtomicUsize::new(0);
pub(super) struct Fixture {
    pub root: PathBuf,
}
impl Fixture {
    pub fn new() -> Self {
        let root = std::env::temp_dir().canonicalize().unwrap().join(format!(
            "ops-hosted-inert-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::SeqCst)
        ));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        Self { root }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}
pub(super) struct Clock(Mutex<Instant>);
impl Clock {
    pub fn new() -> Self {
        Self(Mutex::new(Instant::now()))
    }
    pub fn advance(&self, by: Duration) {
        let mut now = self.0.lock().unwrap();
        *now += by;
    }
}
impl plan::Clock for Clock {
    fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }
}
pub(super) struct Noop;
impl Executor for Noop {
    async fn run(&mut self, _: Command) -> Result<Vec<u8>> {
        panic!("unexpected inert fixture dispatch")
    }
    async fn ready(&mut self) -> Result<()> {
        Ok(())
    }
}
pub(super) fn expect_error<T>(result: Result<T>, code: u8) {
    match result {
        Err(error) => {
            assert_eq!(error.code, code);
            assert!(!format!("{error:?}").contains("PRIVATE"));
        }
        Ok(_) => panic!("expected refusal"),
    }
}
