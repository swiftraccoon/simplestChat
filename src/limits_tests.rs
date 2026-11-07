//! The refundable budget contract, checked over every type that implements it.

use super::Budget;
use crate::room::window::SlidingWindow;
use crate::signaling::connection::token_bucket::{TOKEN_US, TokenBucket};
use std::time::{Duration, Instant};

/// The properties hold at every capacity from zero to this one.
const MAX_CAPACITY: usize = 32;

/// Every type that implements [`Budget`], named for the failure messages and
/// sized to admit `capacity` reservations at `now` and no more.
fn budgets(capacity: usize, now: Instant) -> [(&'static str, Box<dyn Budget>); 2] {
    let tokens = u64::try_from(capacity).unwrap();
    [
        (
            "sliding window",
            Box::new(SlidingWindow::new(Duration::from_secs(60), capacity)),
        ),
        // One token a second refills nothing while the clock stands at `now`.
        (
            "token bucket",
            Box::new(TokenBucket::full(now, 1, tokens * TOKEN_US)),
        ),
    ]
}

/// Refunds half the reservations, rounded up: a strict subset from a capacity
/// of two, so a refund that handed back more than its own reservation (the
/// whole budget, or every reservation taken at that instant) shows as an extra
/// admission. Refunding all of them would pass either defect.
#[test]
fn every_budget_refunds_what_it_reserved() {
    let now = Instant::now();
    for capacity in 0..=MAX_CAPACITY {
        let refunded = capacity.div_ceil(2);
        for (kind, mut budget) in budgets(capacity, now) {
            for _ in 0..capacity {
                assert!(budget.reserve(now), "{kind} of {capacity}: first round");
            }
            for _ in 0..refunded {
                budget.refund(now);
            }
            for _ in 0..refunded {
                assert!(budget.reserve(now), "{kind} of {capacity}: after refunds");
            }
            assert!(!budget.reserve(now), "{kind} of {capacity}: over-refunded");
        }
    }
}

#[test]
fn budgets_refuse_at_their_limit() {
    let now = Instant::now();
    for capacity in 0..=MAX_CAPACITY {
        for (kind, mut budget) in budgets(capacity, now) {
            // A refund with nothing reserved cannot lift a budget past its limit.
            budget.refund(now);
            for _ in 0..capacity {
                assert!(budget.reserve(now), "{kind} of {capacity}: under the limit");
            }
            assert!(!budget.reserve(now), "{kind} of {capacity}: at the limit");
            assert!(!budget.reserve(now), "{kind} of {capacity}: still at it");
        }
    }
}
