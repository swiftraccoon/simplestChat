//! Cross-product of independent monotonic/epoch clocks and one-use expiry.

use super::*;

#[test]
fn boundary_properties_ticket_validity_requires_both_deadlines_and_exactly_one_use() {
    let now = Instant::now();
    const EPOCH: u64 = 1000;
    for remaining in [0_u64, 1, 2, 29, 30, 31, 60, 900] {
        for monotonic_ms in [0_u64, 999, 1000, 1999, 2000, 29_999, 30_000, 31_000] {
            for wall_delta in [-100_i64, 0, 1, 2, 29, 30, 900] {
                let store = TicketStore::with_capacity(1);
                let claims = Claims {
                    sub: "11111111-1111-4111-8111-111111111111".into(),
                    name: "Synthetic identity".into(),
                    iss: "simplestchat".into(),
                    aud: "simplestchat".into(),
                    exp: (EPOCH + remaining) as usize,
                    auth_version: 7,
                    sid: uuid::Uuid::from_u128(1),
                };
                let issued = store.issue_at(claims, now, EPOCH);
                if remaining == 0 {
                    assert!(matches!(issued, Err(AuthError::TokenExpired)));
                    assert!(store.entries.lock().unwrap().is_empty());
                    continue;
                }
                let issued = issued.unwrap();
                assert_eq!(issued.expires_in, remaining.min(30));
                let at = now + Duration::from_millis(monotonic_ms);
                let epoch = EPOCH.checked_add_signed(wall_delta).unwrap();
                let actual = store.take_at(&issued.ticket, at, epoch);
                let expected = monotonic_ms < remaining.min(30) * 1000 && epoch < EPOCH + remaining;
                assert_eq!(
                    actual.is_ok(),
                    expected,
                    "remaining={remaining}, monotonic_ms={monotonic_ms}, wall_delta={wall_delta}"
                );
                if let Ok(ticket) = actual {
                    assert_eq!(ticket.claims.auth_version, 7);
                    assert_eq!(ticket.claims.sid, uuid::Uuid::from_u128(1));
                }
                assert!(store.entries.lock().unwrap().is_empty());
                assert!(store.take_at(&issued.ticket, at, epoch).is_err());
            }
        }
    }
}
