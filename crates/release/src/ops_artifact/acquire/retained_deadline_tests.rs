//! Original-clock admission before any retained-copy effect; no network runs.
use super::super::{
    fixture::{Clock as FixtureClock, Fake, Fixture},
    plan::{self, Context},
};
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

fn after_metadata(clock: Arc<dyn plan::Clock>) -> Context {
    let context = Context::new(clock);
    for index in 0..4 {
        context.command(index).unwrap();
        context.complete_command(index).unwrap();
    }
    context
}

#[test]
fn local_transfer_admits_exact_remaining_debt_and_refuses_one_nanosecond_late() {
    for late in [Duration::ZERO, Duration::from_nanos(1)] {
        let clock = Arc::new(FixtureClock::new());
        let context = after_metadata(clock.clone());
        let remaining = context.end().duration_since(plan::Clock::now(&*clock));
        let debt = context.tail().unwrap();
        clock.advance(remaining - debt + late);
        let result = context.retained_transfer_window();
        if late.is_zero() {
            let (start, end) = result.unwrap();
            assert_eq!(context.end().duration_since(start), debt);
            assert_eq!(end.duration_since(start), plan::CAPS[4]);
            assert_eq!(context.end().duration_since(end), debt - plan::CAPS[4]);
            assert_eq!(context.retained_transfer_window().unwrap_err().code, 1);
        } else {
            assert_eq!(result.unwrap_err().code, 124);
            assert_eq!(context.complete_retained_transfer().unwrap_err().code, 1);
        }
    }
}

struct JumpClock(Mutex<(Instant, Option<Instant>)>);
impl plan::Clock for JumpClock {
    fn now(&self) -> Instant {
        let mut state = self.0.lock().unwrap();
        let observed = state.0;
        if let Some(next) = state.1.take() {
            state.0 = next;
        }
        observed
    }
}

#[tokio::test]
async fn pause_between_admission_and_worker_cannot_extend_the_original_deadline() {
    let fixture = Fixture::new(0);
    let mut executor = Fake::new(&fixture);
    let clock = Arc::new(JumpClock(Mutex::new((Instant::now(), None))));
    let context = after_metadata(clock.clone());
    let whole_end = context.end();
    let first = whole_end - context.tail().unwrap();
    let resumed = whole_end + Duration::from_secs(1);
    *clock.0.lock().unwrap() = (first, Some(resumed));
    // A second admission observation used as a new start would grant a local
    // end beyond the original whole deadline. The worker's first checkpoint
    // must refuse that resumed time before any effect is admitted.
    assert!(resumed + plan::CAPS[4] > whole_end);
    let effected = Arc::new(AtomicBool::new(false));
    let worker_effected = effected.clone();
    let result = super::work(&context, &mut executor, move |checkpoint| {
        checkpoint.check()?;
        worker_effected.store(true, Ordering::SeqCst);
        Ok(())
    })
    .await;
    assert_eq!(result.unwrap_err().code, 124);
    assert!(!effected.load(Ordering::SeqCst));
    assert_eq!(executor.calls, 0, "no gh role represents a local transfer");
}
