//! Actual private writes/readback and explicit close error disposition.

use super::super::{
    fixture::{Clock, Fixture, expect_error},
    plan,
};
use super::{Budget, Checkpoint, Original, Output};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

struct ReadbackClock {
    now: Instant,
    count: AtomicUsize,
    path: PathBuf,
    profile: usize,
}
impl plan::Clock for ReadbackClock {
    fn now(&self) -> Instant {
        if self.count.fetch_add(1, Ordering::SeqCst) == 2 {
            assert!(self.path.is_file());
            if self.profile == 1 {
                // This runs after writing/synchronizing and sampling the
                // original writer stamp, before its actual whole readback.
                fs::write(&self.path, b"different copied source bytes\n").unwrap();
            } else if self.profile == 2 {
                return self.now + Duration::from_secs(150);
            }
        }
        self.now
    }
}

#[test]
fn copy_readback_and_aftereffect_close_errors_keep_the_first_failure_and_close_all_peers() {
    for profile in 0..3 {
        let fixture = Fixture::new();
        let budget = Budget::default();
        let clock = Arc::new(Clock::new());
        let start = *clock.0.lock().unwrap();
        let mut checkpoint = Checkpoint {
            clock,
            start,
            end: start + Duration::from_secs(150),
            abort: Arc::new(AtomicBool::new(false)),
        };
        let output = Output::new(&fixture.output, &fixture.caller, &budget, &checkpoint).unwrap();
        let path = fixture.output.join("projected.py");
        checkpoint.clock = Arc::new(ReadbackClock {
            now: start,
            count: AtomicUsize::new(0),
            path: path.clone(),
            profile,
        });
        let attempted = Arc::new(AtomicBool::new(false));
        let closed = attempted.clone();
        let result = Original::create_with(
            output.witness(),
            "projected.py",
            b"original copied source bytes\n",
            &budget,
            &checkpoint,
            move |file| {
                // Complete the real close, then simulate its reported
                // aftereffect failure. No FD is closed twice or leaked.
                let result = file.finish();
                closed.store(true, Ordering::SeqCst);
                result.and(Err(super::super::failure(7)))
            },
        );
        expect_error(result.map(|_| ()), [7, 1, 124][profile]);
        assert!(attempted.load(Ordering::SeqCst));
        assert!(path.is_file());
        assert_eq!(budget.count(), 0);
        let (explicit, fallback) = budget.close_counts();
        assert!(explicit > 0);
        assert_eq!(fallback, 0);
    }
}
