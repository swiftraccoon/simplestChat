//! Borrowed launcher phases cannot restart or enlarge the original transaction.
use super::*;

fn owner(start: Instant) -> Owner {
    Owner::new(start, Duration::from_secs(1800), Duration::from_secs(60)).unwrap()
}

#[test]
fn launcher_borrows_and_restores_the_same_original_interval() {
    let start = Instant::now();
    let mut owner = owner(start);
    owner.observe_at(start + Duration::from_secs(20));
    let entered = owner.begin_maintain_launcher(
        start + Duration::from_secs(100),
        start + Duration::from_secs(160),
    );
    let bounds = (owner.start(), owner.body_end(), owner.end());
    owner.observe_at(start + Duration::from_secs(90));
    let body = owner.check(Window::Body);
    owner.end_maintain_launcher();
    let original = (owner.start(), owner.body_end(), owner.end());
    let finished = owner.finish();
    assert_eq!(entered, Ok(()));
    assert_eq!(
        bounds,
        (
            start,
            start + Duration::from_secs(100),
            start + Duration::from_secs(160)
        )
    );
    assert_eq!(body, Ok(()));
    assert_eq!(
        original,
        (
            start,
            start + Duration::from_secs(1740),
            start + Duration::from_secs(1800)
        )
    );
    assert_eq!(finished, Ok(()));
}

#[test]
fn late_launcher_finalization_remains_failed_after_return_to_parent_body() {
    let start = Instant::now();
    let mut owner = owner(start);
    owner
        .begin_maintain_launcher(
            start + Duration::from_secs(100),
            start + Duration::from_secs(160),
        )
        .unwrap();
    owner.observe_at(start + Duration::from_secs(160));
    let cleanup = owner.cleanup_before(start + Duration::from_secs(1800));
    let final_work = owner.final_work();
    owner.end_maintain_launcher();
    let restored = owner.body_end();
    let body = owner.check(Window::Body);
    let finished = owner.finish();
    assert_eq!(cleanup, Err(Cause::Deadline));
    assert_eq!(final_work, Err(Cause::Deadline));
    assert_eq!(restored, start + Duration::from_secs(1740));
    assert_eq!(body, Err(Cause::Deadline));
    assert_eq!(finished, Err(Cause::Deadline));
}

#[test]
fn launcher_cannot_overlap_another_body_phase_or_consume_parent_reserve() {
    for overlap in [false, true] {
        let start = Instant::now();
        let mut owner = owner(start);
        if overlap {
            owner
                .begin_stage_validator(start + Duration::from_secs(400))
                .unwrap();
        }
        let attempted = owner.begin_maintain_launcher(
            start + Duration::from_secs(100),
            start + Duration::from_secs(if overlap { 160 } else { 1741 }),
        );
        owner.end_stage_validator();
        let end = owner.end();
        let finished = owner.finish();
        assert_eq!(attempted, Err(Cause::Deadline));
        assert_eq!(end, start + Duration::from_secs(1800));
        assert_eq!(finished, Err(Cause::Deadline));
    }
}
