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

#[test]
fn material_refusals_keep_original_status_first_position_and_privacy_without_clock_reads() {
    for (kind, name, ordinal, total, code) in [
        (MaterialKind::Catalog, "catalog", 70, 70, 23),
        (MaterialKind::Registry, "registry", 1024, 1024, 124),
    ] {
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
        progress.enter(Phase::Download);
        let before = progress.attempted;
        progress.material_refused(kind, 1, total, &Ok(()));
        assert_eq!(progress.attempted, before);
        let result = Err::<(), _>(Failure::new(
            code,
            "PRIVATE https://example.invalid/archive?token=secret source/name body",
        ));
        progress.material_refused(kind, ordinal, total, &result);
        progress.material_refused(
            MaterialKind::Catalog,
            1,
            1,
            &Err::<(), _>(Failure::new(143, "PRIVATE later refusal")),
        );
        progress.finish(&result);
        progress.material_refused(kind, 1, total, &result);
        assert_eq!(clock.calls.load(Ordering::SeqCst), calls);
        assert_eq!(plan.end(), end);
        assert_eq!(plan.observed_elapsed(), Duration::from_millis(100));
        let text = std::str::from_utf8(&bytes).unwrap();
        let expected = format!(
            concat!(
                "phase=download event=material-refused kind={} ordinal={} ",
                "total={} status={} ownerObservedElapsedMs=100\n"
            ),
            name, ordinal, total, code
        );
        assert!(text.contains(&expected));
        assert_eq!(text.matches("event=material-refused ").count(), 1);
        assert_eq!(text.matches("event=finished ").count(), 1);
        assert!(text.contains(&format!(
            "event=finished status={code} refusedPhase=download "
        )));
        assert!(!text.contains("PRIVATE"));
        assert!(!text.contains("https://"));
        assert!(!text.contains("source/name"));
        assert!(!text.contains("secret"));
    }
}

#[test]
fn material_diagnostics_cannot_replace_an_already_recorded_primary_refusal() {
    let plan = Plan::new(Arc::new(super::super::fixture::Clock::new()));
    let mut bytes = Vec::new();
    let mut progress = Progress::new(&plan, &mut bytes);
    progress.enter(Phase::Source);
    let primary = Err::<(), _>(Failure::new(143, "PRIVATE primary"));
    progress.refused(&primary);
    progress.enter(Phase::Download);
    progress.material_refused(
        MaterialKind::Registry,
        2,
        3,
        &Err::<(), _>(Failure::new(23, "PRIVATE material refusal")),
    );
    progress.finish(&primary);
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(text.contains("kind=registry ordinal=2 total=3 status=23 "));
    assert!(text.contains("event=finished status=143 refusedPhase=source "));
    assert!(!text.contains("PRIVATE"));
}

struct Broken {
    attempted: usize,
    last: Vec<u8>,
}

#[test]
fn closed_transport_labels_are_optional_deduplicated_and_do_not_read_clocks() {
    let clock = Arc::new(Clock {
        now: Mutex::new(Instant::now()),
        calls: AtomicUsize::new(0),
    });
    let plan = Plan::new(clock.clone());
    plan.check().unwrap();
    let calls = clock.calls.load(Ordering::SeqCst);
    let mut bytes = Vec::new();
    let mut progress = Progress::new(&plan, &mut bytes);
    progress.enter(Phase::Download);
    let result = Err::<(), _>(Failure::child_exit(28));
    progress.material_refused(MaterialKind::Catalog, 69, 70, &result);
    progress.transport_timeout(MaterialKind::Catalog, 69, 70, None);
    progress.transport_timeout(
        MaterialKind::Catalog,
        69,
        70,
        Some(Timeout::ConnectUnspecified),
    );
    progress.transport_timeout(MaterialKind::Registry, 1, 2, Some(Timeout::Unknown));
    progress.finish(&result);
    progress.transport_timeout(MaterialKind::Catalog, 1, 2, Some(Timeout::TlsReported));
    assert_eq!(clock.calls.load(Ordering::SeqCst), calls);
    let text = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(text.matches("event=transport-timeout ").count(), 1);
    assert!(text.contains("category=reported-connect-timeout-unspecified "));
    assert!(text.contains("event=finished status=28 refusedPhase=download "));
    assert!(bytes.len() <= MAX_BYTES);
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

#[test]
fn material_refusal_keeps_reserved_terminal_attempt_after_the_same_byte_ceiling_is_spent() {
    let plan = Plan::new(Arc::new(super::super::fixture::Clock::new()));
    let mut sink = Broken {
        attempted: 0,
        last: Vec::new(),
    };
    let mut progress = Progress::new(&plan, &mut sink);
    progress.enter(Phase::Download);
    for ordinal in 1..=10000 {
        progress.kit(Checkpoint::Import, Edge::Begin, ordinal, 10000);
    }
    let result = Err::<(), _>(Failure::new(124, "PRIVATE material failure"));
    progress.material_refused(MaterialKind::Registry, 1024, 1024, &result);
    progress.finish(&result);
    assert!(sink.attempted <= MAX_BYTES);
    assert!(sink.attempted > MAX_BYTES - TERMINAL_RESERVE * 2);
    let terminal = std::str::from_utf8(&sink.last).unwrap();
    assert!(terminal.contains("event=finished status=124 refusedPhase=download "));
    assert!(!terminal.contains("PRIVATE"));
}

#[test]
fn full_accepted_archive_rosters_leave_all_later_probes_build_and_terminal_observable() {
    for total in [69, 70, 96] {
        let clock = Arc::new(super::super::fixture::Clock::new());
        let plan = Plan::new(clock.clone());
        let original_end = plan.end();
        clock.advance(super::super::plan::ENTRY - Duration::from_secs(1));
        plan.check().unwrap();
        let mut bytes = Vec::new();
        let mut progress = Progress::new(&plan, &mut bytes);
        for phase in [Phase::Source, Phase::Download, Phase::Registry] {
            progress.enter(phase);
            progress.complete();
        }
        progress.enter(Phase::Kit);
        for ordinal in 1..=total {
            progress.kit(Checkpoint::Import, Edge::Begin, ordinal, total);
            progress.kit(Checkpoint::Import, Edge::Complete, ordinal, total);
        }
        for edge in [Edge::Begin, Edge::Complete] {
            progress.kit(Checkpoint::Resolve, edge, 1, 1);
        }
        for ordinal in 1..=14 {
            progress.kit(Checkpoint::Probe, Edge::Begin, ordinal, 14);
            progress.kit(Checkpoint::Probe, Edge::Complete, ordinal, 14);
        }
        progress.kit(Checkpoint::NativeBuild, Edge::Begin, 1, 1);
        for ordinal in 1..=6 {
            progress.kit(Checkpoint::NativeCommand, Edge::Begin, ordinal, 6);
            progress.kit(Checkpoint::NativeCommand, Edge::Complete, ordinal, 6);
        }
        progress.kit(Checkpoint::NativeOutputs, Edge::Begin, 1, 1);
        progress.kit(Checkpoint::NativeOutputs, Edge::Complete, 1, 1);
        progress.kit(Checkpoint::NativeBuild, Edge::Complete, 1, 1);
        for checkpoint in [Checkpoint::RegistryCopy, Checkpoint::Measure] {
            progress.kit(checkpoint, Edge::Begin, 1, 1);
            progress.kit(checkpoint, Edge::Complete, 1, 1);
        }
        progress.complete();
        for phase in [Phase::Producer, Phase::Assembly] {
            progress.enter(phase);
            progress.complete();
        }
        progress.finish(&Ok(()));
        assert!(progress.finished());
        assert_eq!(plan.end(), original_end);
        assert!(bytes.len() <= MAX_BYTES);
        let text = std::str::from_utf8(&bytes).unwrap();
        let imports = text
            .lines()
            .filter(|line| line.contains("checkpoint=import "))
            .collect::<Vec<_>>();
        assert_eq!(imports.len(), if total <= 70 { 20 } else { 26 });
        for ordinal in [1, 8, 64, total] {
            for edge in ["begin", "complete"] {
                assert!(text.contains(&format!(
                    "checkpoint=import event={edge} ordinal={ordinal} total={total} "
                )));
            }
        }
        assert!(!text.contains("checkpoint=import event=begin ordinal=2 "));
        for ordinal in 1..=14 {
            for edge in ["begin", "complete"] {
                assert!(text.contains(&format!(
                    "checkpoint=probe event={edge} ordinal={ordinal} total=14 "
                )));
            }
        }
        for ordinal in 1..=6 {
            for edge in ["begin", "complete"] {
                assert!(text.contains(&format!(
                    "checkpoint=native-command event={edge} ordinal={ordinal} total=6 "
                )));
            }
        }
        for checkpoint in [
            "resolve",
            "native-build",
            "native-outputs",
            "registry-copy",
            "measure",
        ] {
            for edge in ["begin", "complete"] {
                assert!(text.contains(&format!(
                    "checkpoint={checkpoint} event={edge} ordinal=1 total=1 "
                )));
            }
        }
        assert!(text.contains("event=finished status=0 refusedPhase=none "));
        assert_eq!(text.matches("event=finished ").count(), 1);
    }
}
