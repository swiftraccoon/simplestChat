//! Finite counter/clock invariants; logical time advances without sleeping.

use super::*;

#[test]
fn boundary_properties_failure_delays_are_monotone_bounded_and_do_not_wrap() {
    let mut counts: Vec<u32> = (0..=128).collect();
    counts.extend([u32::MAX - 2, u32::MAX - 1, u32::MAX]);
    for free in [0, 3, u32::MAX - 1, u32::MAX] {
        for ceiling in [Duration::ZERO, Duration::from_secs(1), MAX_PAIR_DELAY] {
            let mut previous = Duration::ZERO;
            for &failures in &counts {
                let actual = delay_after(failures, free, ceiling);
                assert!(actual >= previous && actual <= ceiling);
                assert_eq!(actual.is_zero(), failures <= free || ceiling.is_zero());
                previous = actual;
            }
        }
    }
    let now = Instant::now();
    let mut table = FailureTable::new(FREE_PAIR_FAILURES, MAX_PAIR_DELAY);
    table.entries.put(
        0_u8,
        Failures {
            count: u32::MAX - 1,
            blocked_until: now,
            last_failure: now,
        },
    );
    for _ in 0..3 {
        table.record_failure(0, now);
        assert_eq!(table.entries.peek(&0).unwrap().count, u32::MAX);
        assert_eq!(table.wait(&0, now), MAX_PAIR_DELAY);
    }
    for elapsed in [
        Duration::ZERO,
        Duration::from_nanos(1),
        Duration::from_secs(1),
        MAX_PAIR_DELAY,
    ] {
        assert_eq!(
            table.wait(&0, now + elapsed),
            MAX_PAIR_DELAY.saturating_sub(elapsed)
        );
    }
    let forgotten = now + FAILURE_MEMORY;
    assert_eq!(table.wait(&0, forgotten), Duration::ZERO);
    table.record_failure(0, forgotten);
    assert_eq!(table.entries.peek(&0).unwrap().count, 1);
    assert_eq!(table.wait(&0, forgotten), Duration::ZERO);
}

#[test]
fn boundary_properties_window_budgets_hold_until_the_exact_reset_instant() {
    let start = Instant::now();
    let address = "192.0.2.1".parse::<IpAddr>().unwrap();
    let duration = Duration::from_secs(10);
    for allowance in 0..=64 {
        let limiter = WindowLimiter::new(allowance, duration);
        for round in 0..3 {
            let boundary = start + duration * round;
            let admitted = (0..allowance + 2)
                .filter(|_| limiter.allow_at(address, boundary))
                .count();
            assert_eq!(admitted, allowance as usize);
            assert!(!limiter.allow_at(address, boundary + duration - Duration::from_nanos(1)));
            assert_eq!(
                limiter.entries.lock().unwrap().peek(&address).unwrap().used,
                allowance
            );
        }
    }
    let limiter = WindowLimiter::new(u32::MAX, duration);
    limiter.entries.lock().unwrap().put(
        address,
        Window {
            started: start,
            used: u32::MAX - 1,
        },
    );
    assert!(limiter.allow_at(address, start));
    assert!(!limiter.allow_at(address, start));
    assert_eq!(
        limiter.entries.lock().unwrap().peek(&address).unwrap().used,
        u32::MAX
    );
    assert!(limiter.allow_at(address, start + duration));
    assert_eq!(
        limiter.entries.lock().unwrap().peek(&address).unwrap().used,
        1
    );
}
