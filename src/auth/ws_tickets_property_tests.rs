//! Cross-product of independent monotonic/wall clocks and one-use expiry.

use super::*;

#[test]
fn boundary_properties_ticket_validity_requires_both_deadlines_and_exactly_one_use() {
    let now = Instant::now();
    const EPOCH: i64 = 1000;
    let at = |seconds: i64| DateTime::from_timestamp(seconds, 0).unwrap();
    for remaining in [0_i64, 1, 2, 29, 30, 31, 60, 900] {
        for monotonic_ms in [0_u64, 999, 1000, 1999, 2000, 29_999, 30_000, 31_000] {
            for wall_delta in [-100_i64, 0, 1, 2, 29, 30, 900] {
                let store = TicketStore::with_capacity(NonZeroUsize::MIN);
                let principal = Principal {
                    user: crate::ids::UserId::from(uuid::Uuid::from_u128(2)),
                    sid: uuid::Uuid::from_u128(1),
                    auth_version: 7,
                    expires_at: at(EPOCH + remaining),
                };
                let issued = store.issue_at(principal, now, at(EPOCH));
                if remaining == 0 {
                    assert!(matches!(issued, Err(AuthError::TokenExpired)));
                    assert!(store.entries.lock().is_empty());
                    continue;
                }
                let issued = issued.unwrap();
                assert_eq!(issued.expires_in, remaining.min(30) as u64);
                let at_monotonic = now + Duration::from_millis(monotonic_ms);
                let wall = EPOCH + wall_delta;
                let actual = store.take_at(issued.ticket.expose_secret(), at_monotonic, at(wall));
                let expected =
                    monotonic_ms < remaining.min(30) as u64 * 1000 && wall < EPOCH + remaining;
                assert_eq!(
                    actual.is_ok(),
                    expected,
                    "remaining={remaining}, monotonic_ms={monotonic_ms}, wall_delta={wall_delta}"
                );
                if let Ok(ticket) = actual {
                    assert_eq!(ticket.principal.auth_version, 7);
                    assert_eq!(ticket.principal.sid, uuid::Uuid::from_u128(1));
                }
                assert!(store.entries.lock().is_empty());
                assert!(
                    store
                        .take_at(issued.ticket.expose_secret(), at_monotonic, at(wall))
                        .is_err()
                );
            }
        }
    }
}
