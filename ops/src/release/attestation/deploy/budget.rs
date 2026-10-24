//! Source-derived command debt inside the original body, never a new deadline.

use super::super::control::{Cause, Owner, Result};
use std::time::{Duration, Instant};

pub(super) const MAX_WHOLE: Duration = Duration::from_secs(1800);
pub(super) const TERM: Duration = Duration::from_secs(5);
pub(super) const REAP: Duration = Duration::from_secs(5);
pub(super) const SETTLE: Duration = Duration::from_secs(TERM.as_secs() + REAP.as_secs());
pub(super) const READY_POLL: Duration = Duration::from_millis(500);
pub(super) const QUIET_POLL: Duration = Duration::from_secs(5);
// Three optional diagnostic roles: ps, exact candidate inspection and logs.
const DIAGNOSTICS: u64 = 3 * (30 + 10);
const STOP: u64 = 45 + 10;
const UP: u64 = 60 + 10;
const APPLICATION: u64 = 2 * (30 + 10);
// Each original retry loop can enter a last request after its 0.5s sleep.
// Both loops include that overshoot plus request5 and child settlement10.
const READINESS_MILLIS: u64 = 2 * (30_000 + 500 + 5_000 + 10_000);
const DEPENDENCIES: u64 = 4 * (30 + 10);
pub(super) const RECOVERY: Duration = Duration::from_millis(
    (DIAGNOSTICS + STOP + UP + APPLICATION + DEPENDENCIES) * 1000 + READINESS_MILLIS,
);
pub(super) const FIRST_STOP: Duration = Duration::from_secs(STOP);

pub(super) fn candidate_end(owner: &Owner) -> Result<Instant> {
    owner
        .body_end()
        .checked_sub(RECOVERY)
        .ok_or(Cause::Deadline)
}
pub(super) fn first_stop_fits(owner: &Owner, end: Instant) -> Result<()> {
    if owner
        .now()
        .checked_add(FIRST_STOP)
        .is_none_or(|at| at >= end)
    {
        Err(Cause::Deadline)
    } else {
        Ok(())
    }
}
pub(super) fn ready_ceiling(start: Instant, seconds: Duration) -> Result<Instant> {
    start
        .checked_add(seconds)
        .and_then(|at| at.checked_add(READY_POLL))
        .and_then(|at| at.checked_add(Duration::from_secs(5)))
        .and_then(|at| at.checked_add(SETTLE))
        .ok_or(Cause::Deadline)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_source_roles_leave_recovery_inside_body_and_not_local_reserve() {
        let start = Instant::now();
        let owner = Owner::new(start, Duration::from_secs(1800), Duration::from_secs(60)).unwrap();
        assert_eq!(RECOVERY, Duration::from_secs(576));
        assert_eq!(owner.body_end() - start, Duration::from_secs(1740));
        assert_eq!(
            candidate_end(&owner).unwrap() - start,
            Duration::from_secs(1164)
        );
        assert_eq!(
            ready_ceiling(start, Duration::from_secs(30)).unwrap() - start,
            Duration::from_millis(45_500)
        );
    }
}
