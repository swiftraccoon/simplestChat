//! Original-clock export admission and tail accounting; no processes or signals.
//!
//! Interrupt/terminate cases inject already observed typed causes. Actual native
//! signal delivery and retirement belong to the separate process regressions.

use crate::release::build::control::{Failure, Owner, RETIRE_AND_DRAIN, SETUP, TAIL, VERIFICATION};
use std::time::{Duration, Instant};

#[derive(Clone, Copy)]
struct Export {
    seconds: u64,
    whole: u64,
    verification: u64,
    setup_phase: u64,
    setup_body: u64,
    active_phase: u64,
    active_body: u64,
    verification_start: u64,
    verification_phase: u64,
    verification_query: u64,
}

const EXPORTS: [Export; 4] = [
    Export {
        seconds: 60,
        whole: 120,
        verification: 60,
        setup_phase: 46,
        setup_body: 20,
        active_phase: 46,
        active_body: 20,
        verification_start: 46,
        verification_phase: 106,
        verification_query: 76,
    },
    Export {
        seconds: 139,
        whole: 199,
        verification: 139,
        setup_phase: 46,
        setup_body: 20,
        active_phase: 46,
        active_body: 20,
        verification_start: 46,
        verification_phase: 185,
        verification_query: 76,
    },
    Export {
        seconds: 140,
        whole: 200,
        verification: 140,
        setup_phase: 46,
        setup_body: 20,
        active_phase: 46,
        active_body: 20,
        verification_start: 46,
        verification_phase: 186,
        verification_query: 76,
    },
    Export {
        seconds: 300,
        whole: 360,
        verification: 140,
        setup_phase: 120,
        setup_body: 30,
        active_phase: 206,
        active_body: 180,
        verification_start: 206,
        verification_phase: 346,
        verification_query: 236,
    },
];

fn seconds(value: u64) -> Duration {
    Duration::from_secs(value)
}

fn owner(export: Export) -> Owner {
    // Register within the Tokio test runtime, then use one exact virtual entry.
    let mut owner = Owner::new(
        Instant::now(),
        crate::release::build::control::Mode::Export,
        export.seconds,
    )
    .unwrap();
    owner.advance(Duration::ZERO);
    owner
}

fn at(owner: &Owner, value: u64) -> Instant {
    owner.started() + seconds(value)
}

fn verification(export: Export) -> Duration {
    VERIFICATION.min(seconds(export.seconds))
}

fn setup(owner: &mut Owner, verification: Duration) -> Instant {
    owner.phase(SETUP, verification + seconds(14)).unwrap()
}

fn active(owner: &mut Owner, export: Export) -> Instant {
    owner
        .phase(seconds(export.seconds), verification(export) + seconds(14))
        .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn small_export_matrix_admits_positive_bodies_and_keeps_all_forty_tail_seconds() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let reserve = verification(export);
        assert_eq!(reserve, seconds(export.verification));
        assert_eq!(owner.end(), at(&owner, export.whole));

        let setup_phase = setup(&mut owner, reserve);
        assert_eq!(setup_phase, at(&owner, export.setup_phase));
        let setup_body = owner
            .body(setup_phase - RETIRE_AND_DRAIN, seconds(30), reserve)
            .unwrap();
        assert_eq!(setup_body, at(&owner, export.setup_body));
        assert!(setup_body > owner.now());
        assert!(setup_body + RETIRE_AND_DRAIN <= setup_phase);
        assert!(setup_body + reserve + TAIL <= owner.end());

        let active_phase = active(&mut owner, export);
        assert_eq!(active_phase, at(&owner, export.active_phase));
        let active_body = owner
            .body(active_phase, seconds(export.seconds), reserve)
            .unwrap();
        assert_eq!(active_body, at(&owner, export.active_body));
        assert!(active_body > owner.now());
        assert_eq!(active_body + reserve + TAIL, owner.end());
        assert_eq!(owner.primary(), None);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn later_setup_queries_consume_one_original_aggregate_window() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let reserve = verification(export);
        let setup_phase = setup(&mut owner, reserve);
        let last_body = (setup_phase - RETIRE_AND_DRAIN).min(owner.end() - reserve - TAIL);
        let expected_queries = if export.seconds == 300 { 4 } else { 1 };
        for _ in 0..expected_queries {
            let now = owner.now();
            let body = owner
                .body(setup_phase - RETIRE_AND_DRAIN, seconds(30), reserve)
                .unwrap();
            assert!(body > now);
            assert!(body <= last_body);
            assert!(body <= now + seconds(30));
            owner.advance(body - now);
        }
        assert_eq!(owner.now(), last_body);
        assert_eq!(
            owner.body(setup_phase - RETIRE_AND_DRAIN, seconds(30), reserve),
            Err(Failure::Deadline)
        );
        assert_eq!(owner.primary(), Some(Failure::Deadline));
        assert!(owner.cleanup_before(owner.end()).is_ok());
        assert_eq!(
            owner.phase(SETUP, reserve + seconds(14)),
            Err(Failure::Deadline)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn setup_elapsed_time_reduces_active_body_without_resetting_the_entry_end() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let original_end = owner.end();
        owner.advance(seconds(10));
        let active_phase = active(&mut owner, export);
        let active_body = owner
            .body(active_phase, seconds(export.seconds), verification(export))
            .unwrap();
        assert_eq!(active_body, at(&owner, export.active_body));
        assert_eq!(active_body - owner.now(), seconds(export.active_body - 10));
        owner.advance(active_body - owner.now());
        assert_eq!(
            owner.body(active_phase, seconds(export.seconds), verification(export)),
            Err(Failure::Deadline)
        );
        assert_eq!(owner.end(), original_end);
        assert!(owner.cleanup_before(original_end).is_ok());
        assert_eq!(
            owner.phase(seconds(export.seconds), verification(export) + seconds(14)),
            Err(Failure::Deadline)
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn verification_queries_share_the_capped_phase_and_keep_group_and_file_tails() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let original_end = owner.end();
        owner.advance(seconds(export.active_body) + RETIRE_AND_DRAIN);
        assert_eq!(owner.now(), at(&owner, export.verification_start));
        let phase = owner.phase(verification(export), seconds(14)).unwrap();
        assert_eq!(phase, at(&owner, export.verification_phase));
        assert_eq!(phase - owner.now(), verification(export));
        let first = owner
            .body(phase - RETIRE_AND_DRAIN, seconds(30), Duration::ZERO)
            .unwrap();
        assert_eq!(first, at(&owner, export.verification_query));

        // Reuse the original phase for every query, rather than reissuing phase().
        let last_body = phase - RETIRE_AND_DRAIN;
        let count = if export.seconds == 60 { 2 } else { 4 };
        for _ in 0..count {
            let now = owner.now();
            let body = owner
                .body(phase - RETIRE_AND_DRAIN, seconds(30), Duration::ZERO)
                .unwrap();
            assert!(body > now && body <= last_body);
            assert!(body + RETIRE_AND_DRAIN <= phase);
            assert!(body + TAIL <= original_end);
            owner.advance(body - now);
        }
        assert_eq!(owner.now(), last_body);
        assert_eq!(last_body + TAIL, original_end);
        assert_eq!(
            owner.body(phase - RETIRE_AND_DRAIN, seconds(30), Duration::ZERO),
            Err(Failure::Deadline)
        );
        assert!(owner.cleanup_before(original_end).is_ok());
    }
}

#[tokio::test(flavor = "current_thread")]
async fn expired_setup_refuses_late_admission_even_with_whole_tail_remaining() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let phase = setup(&mut owner, verification(export));
        owner.advance(phase - owner.now());
        assert_eq!(owner.before(phase), Err(Failure::Deadline));
        assert!(owner.now() < owner.end());
        assert!(owner.cleanup_before(owner.end()).is_ok());
        assert_eq!(
            owner.body(phase - RETIRE_AND_DRAIN, seconds(30), verification(export)),
            Err(Failure::Deadline)
        );
        assert_eq!(owner.check(), Err(Failure::Deadline));
    }
}

#[tokio::test(flavor = "current_thread")]
async fn typed_cancellation_during_verification_keeps_its_first_observed_cause() {
    for export in EXPORTS {
        for first in [Failure::Interrupt, Failure::Terminate] {
            let mut owner = owner(export);
            owner.advance(seconds(export.verification_start));
            let phase = owner.phase(verification(export), seconds(14)).unwrap();
            owner
                .body(phase - RETIRE_AND_DRAIN, seconds(30), Duration::ZERO)
                .unwrap();
            owner.record(first);
            assert_eq!(owner.before(phase), Err(first));
            assert_eq!(
                owner.body(phase - RETIRE_AND_DRAIN, seconds(30), Duration::ZERO),
                Err(first)
            );
            owner.record(Failure::Close);
            owner.record(if first == Failure::Interrupt {
                Failure::Terminate
            } else {
                Failure::Interrupt
            });
            owner.advance(owner.end() - owner.now());
            assert_eq!(owner.cleanup_before(owner.end()), Err(Failure::Deadline));
            assert_eq!(owner.check(), Err(first));
            assert_eq!(owner.primary(), Some(first));
            assert_eq!(owner.phase(SETUP, Duration::ZERO), Err(first));
        }
    }
}

#[tokio::test(flavor = "current_thread")]
async fn finalization_uses_the_original_end_and_latches_exhaustion_without_extension() {
    for export in EXPORTS {
        let mut owner = owner(export);
        let original = owner.end();
        owner.advance(original - owner.now() - Duration::from_nanos(1));
        assert!(owner.cleanup_before(original + seconds(60)).is_ok());
        owner.advance(Duration::from_nanos(1));
        let failure = owner.cleanup_before(original + seconds(60)).unwrap_err();
        assert_eq!(failure, Failure::Deadline);
        assert_eq!(
            owner.primary(),
            None,
            "cleanup observation is explicitly recorded by its caller"
        );
        owner.record(failure);
        owner.record(Failure::Close);
        assert_eq!(owner.primary(), Some(Failure::Deadline));
        assert_eq!(owner.check(), Err(Failure::Deadline));
        assert_eq!(owner.end(), original);
    }
}
