//! Diagnostic observers cannot consume clock authority, change failures or grow
//! output without bound. Whole coordinator traces cover actual close ordering.

use super::*;
use crate::tooling::Failure;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct Clock {
    now: Mutex<Instant>,
    calls: AtomicUsize,
}
impl super::super::plan::Clock for Clock {
    fn now(&self) -> Instant {
        self.calls.fetch_add(1, Ordering::SeqCst);
        *self.now.lock().unwrap()
    }
}
#[test]
fn progress_reads_only_the_original_validated_sample_without_admitting_or_sampling_work() {
    let clock = Arc::new(Clock {
        now: Mutex::new(Instant::now()),
        calls: AtomicUsize::new(0),
    });
    let plan = Plan::new(clock.clone());
    let end = plan.end();
    *clock.now.lock().unwrap() += Duration::from_millis(100);
    plan.check().unwrap();
    let calls = clock.calls.load(Ordering::SeqCst);
    *clock.now.lock().unwrap() += Duration::from_secs(5);
    let mut bytes = Vec::new();
    let mut progress = Progress::new(&plan, &mut bytes);
    progress.enter(Phase::Producer);
    progress.kit(Checkpoint::Import, Edge::Complete, 42, 42);
    let result = Err(Failure::new(124, "PRIVATE path/argv/token payload"));
    progress.refused(&result);
    progress.enter(Phase::Finalization);
    progress.finish(&result);
    assert_eq!(clock.calls.load(Ordering::SeqCst), calls);
    assert_eq!(plan.end(), end);
    assert_eq!(plan.observed_elapsed(), Duration::from_millis(100));
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(
        text.lines()
            .all(|line| line.ends_with("ownerObservedElapsedMs=100"))
    );
    assert!(text.contains("status=124 refusedPhase=producer"));
    assert!(!text.contains("PRIVATE"));
    // Reporting arbitrary labels grants no actual phase transition or clock
    // sample; the original first admission remains the next legal operation.
    plan.begin(0).unwrap();
    assert_eq!(clock.calls.load(Ordering::SeqCst), calls + 1);
}

struct Broken {
    attempted: usize,
    last: Vec<u8>,
}
impl Write for Broken {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.attempted += bytes.len();
        self.last = bytes.to_vec();
        Err(std::io::Error::other("PRIVATE writer failure"))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Err(std::io::Error::other("PRIVATE flush failure"))
    }
}
#[test]
fn failed_writes_spend_the_same_lifetime_ceiling_and_keep_one_reserved_terminal_attempt() {
    let plan = Plan::new(Arc::new(super::super::fixture::Clock::new()));
    let mut sink = Broken {
        attempted: 0,
        last: Vec::new(),
    };
    let mut progress = Progress::new(&plan, &mut sink);
    progress.enter(Phase::Kit);
    for index in 0..10000 {
        progress.kit(Checkpoint::Import, Edge::Begin, index, 10000);
    }
    let result = Err(Failure::new(143, "PRIVATE primary operation error"));
    progress.finish(&result);
    progress.enter(Phase::Source);
    progress.complete();
    progress.finish(&Ok(()));
    assert!(sink.attempted <= MAX_BYTES);
    assert!(sink.attempted > MAX_BYTES - TERMINAL_RESERVE * 2);
    let terminal = std::str::from_utf8(&sink.last).unwrap();
    assert!(terminal.contains("event=finished status=143 refusedPhase=kit"));
    assert!(!terminal.contains("PRIVATE"));
}
