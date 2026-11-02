//! Same-socket credential renewal preserves membership and absolute expiry.

use super::*;
use crate::auth::access_tokens;
use crate::auth::types::AuthError;
use crate::signaling::protocol::AuthenticationToken;
use chrono::{DateTime, Utc};

const RENEWAL_WINDOW: Duration = Duration::from_secs(60);
const MAX_RENEWALS_PER_WINDOW: u8 = 3;
const RENEWAL_VALIDATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Connection-local access to the existing authentication concurrency budget.
#[derive(Clone)]
pub(crate) struct RenewalAuthenticator {
    concurrency: Arc<tokio::sync::Semaphore>,
}

impl RenewalAuthenticator {
    pub(crate) fn new(concurrency: Arc<tokio::sync::Semaphore>) -> Self {
        Self { concurrency }
    }
}

/// Renewals cannot change identity, account version or session, revive
/// expiry, or move expiry backwards.
fn renewal_matches(current: &Principal, candidate: &Principal, now: DateTime<Utc>) -> bool {
    current.expires_at > now
        && candidate.expires_at >= current.expires_at
        && candidate.user == current.user
        && candidate.auth_version == current.auth_version
        && candidate.sid == current.sid
}

pub(super) struct RenewalBudget {
    window_started: Instant,
    attempts: u8,
}

impl RenewalBudget {
    pub(super) fn new() -> Self {
        Self {
            window_started: Instant::now(),
            attempts: 0,
        }
    }

    pub(super) fn allow(&mut self, now: Instant) -> bool {
        if now.duration_since(self.window_started) >= RENEWAL_WINDOW {
            self.window_started = now;
            self.attempts = 0;
        }
        if self.attempts >= MAX_RENEWALS_PER_WINDOW {
            return false;
        }
        self.attempts += 1;
        true
    }
}

pub(super) enum RenewalOutcome {
    Renewed(Principal),
    Rejected,
    /// The database could not establish whether the new token is current.
    /// Keep the previous principal; the connection owns the bounded uncertainty policy.
    Unavailable,
    /// Shared validation capacity is occupied; no database check was attempted.
    Busy,
    /// Transport loss/drain does not independently invalidate credentials.
    Interrupted,
    Close,
}

/// Consume already-delivered revocations before committing a renewed principal.
/// A lagged/closed channel cannot establish current credentials for this commit.
pub(super) fn revocations_current(
    revocations: &mut tokio::sync::broadcast::Receiver<(UserId, i64)>,
    current: &Principal,
) -> bool {
    // Bound synchronous work even if other accounts change credentials rapidly.
    for _ in 0..256 {
        match revocations.try_recv() {
            Ok((user, minimum_version)) => {
                if user == current.user && current.auth_version < minimum_version {
                    return false;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return true,
            Err(_) => return false,
        }
    }
    false
}

/// Resolve the new token through the same validation and shared admission
/// budget as authenticated HTTP. A token of the wrong shape is refused before
/// any query or permit. Waiting never extends the previous token lifetime.
pub(super) async fn renew_authentication(
    authenticator: &RenewalAuthenticator,
    current: &Principal,
    token: &AuthenticationToken,
    pool: Option<&sqlx::PgPool>,
    revocations: &mut tokio::sync::broadcast::Receiver<(UserId, i64)>,
    drain: &crate::shutdown::DrainSignal,
    sender: &mpsc::Sender<crate::OutboundJson>,
) -> RenewalOutcome {
    if current.expires_at <= Utc::now() || !revocations_current(revocations, current) {
        return RenewalOutcome::Close;
    }
    if drain.is_draining() || sender.is_closed() {
        return RenewalOutcome::Interrupted;
    }
    let Some(pool) = pool else {
        return RenewalOutcome::Close;
    };
    if !access_tokens::valid_shape(token.expose()) {
        return RenewalOutcome::Rejected;
    }
    let Ok(_permit) = authenticator.concurrency.clone().try_acquire_owned() else {
        return RenewalOutcome::Busy;
    };
    // Keep the fractional second remaining before the accepted expiry, so
    // validation can never run past it.
    let remaining = (current.expires_at - Utc::now())
        .to_std()
        .unwrap_or_default();
    if remaining.is_zero() {
        return RenewalOutcome::Close;
    }
    let deadline = tokio::time::Instant::now() + RENEWAL_VALIDATION_TIMEOUT.min(remaining);
    validate_renewal(
        current,
        access_tokens::validate_access_token(pool, token.expose()),
        revocations,
        drain,
        sender,
        deadline,
    )
    .await
}

/// Keep transport/revocation/expiry races separate from the database result.
/// Only a token the database resolves to this socket's own account, version
/// and session, expiring no earlier, renews it. A token it refuses is rejected
/// and the accepted principal stays, still bound by its own expiry and checks.
/// A local validation deadline is temporary unavailability unless the accepted
/// token has itself expired.
async fn validate_renewal(
    current: &Principal,
    validation: impl std::future::Future<Output = Result<Principal, AuthError>>,
    revocations: &mut tokio::sync::broadcast::Receiver<(UserId, i64)>,
    drain: &crate::shutdown::DrainSignal,
    sender: &mpsc::Sender<crate::OutboundJson>,
    deadline: tokio::time::Instant,
) -> RenewalOutcome {
    if current.expires_at <= Utc::now() || !revocations_current(revocations, current) {
        return RenewalOutcome::Close;
    }
    let candidate = {
        tokio::pin!(validation);
        loop {
            tokio::select! {
                biased;
                _ = drain.wait() => return RenewalOutcome::Interrupted,
                _ = sender.closed() => return RenewalOutcome::Interrupted,
                _ = tokio::time::sleep_until(deadline) => break None,
                notice = revocations.recv() => match notice {
                    Ok((user, minimum_version)) if user != current.user || current.auth_version >= minimum_version => {},
                    _ => return RenewalOutcome::Close,
                },
                candidate = &mut validation => break Some(candidate),
            }
        }
    };
    if current.expires_at <= Utc::now() || !revocations_current(revocations, current) {
        return RenewalOutcome::Close;
    }
    if drain.is_draining() || sender.is_closed() {
        return RenewalOutcome::Interrupted;
    }
    if tokio::time::Instant::now() >= deadline {
        return RenewalOutcome::Unavailable;
    }
    match candidate {
        None | Some(Err(AuthError::DatabaseError(_))) => RenewalOutcome::Unavailable,
        Some(Ok(candidate)) if renewal_matches(current, &candidate, Utc::now()) => {
            RenewalOutcome::Renewed(candidate)
        }
        Some(_) => RenewalOutcome::Rejected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn principal(expires_at: DateTime<Utc>) -> Principal {
        Principal {
            user: UserId::random(),
            sid: Uuid::new_v4(),
            auth_version: 0,
            expires_at,
        }
    }

    fn later(seconds: i64) -> DateTime<Utc> {
        Utc::now() + chrono::TimeDelta::seconds(seconds)
    }

    fn token(text: &str) -> AuthenticationToken {
        serde_json::from_value(serde_json::json!(text)).unwrap()
    }

    /// The shape the server issues: 43 base64url characters encoding 32 bytes.
    fn well_formed_token() -> AuthenticationToken {
        token(&"A".repeat(43))
    }

    struct ValidationFixture {
        current: Principal,
        candidate: Principal,
        notice_sender: tokio::sync::broadcast::Sender<(UserId, i64)>,
        revocations: tokio::sync::broadcast::Receiver<(UserId, i64)>,
        drain: crate::shutdown::DrainSignal,
        sender: mpsc::Sender<crate::OutboundJson>,
        receiver: mpsc::Receiver<crate::OutboundJson>,
    }

    impl ValidationFixture {
        fn new() -> Self {
            let current = principal(later(60));
            let mut candidate = current.clone();
            candidate.expires_at += chrono::TimeDelta::seconds(60);
            let (notice_sender, revocations) = tokio::sync::broadcast::channel(1);
            let (sender, receiver) = mpsc::channel(1);
            Self {
                current,
                candidate,
                notice_sender,
                revocations,
                drain: crate::shutdown::DrainSignal::default(),
                sender,
                receiver,
            }
        }

        async fn validate(
            &mut self,
            validation: impl std::future::Future<Output = Result<Principal, AuthError>>,
            deadline: tokio::time::Instant,
        ) -> RenewalOutcome {
            validate_renewal(
                &self.current,
                validation,
                &mut self.revocations,
                &self.drain,
                &self.sender,
                deadline,
            )
            .await
        }

        async fn renew(
            &mut self,
            token: &AuthenticationToken,
            pool: Option<&sqlx::PgPool>,
        ) -> RenewalOutcome {
            let authenticator = RenewalAuthenticator::new(Arc::new(tokio::sync::Semaphore::new(1)));
            renew_authentication(
                &authenticator,
                &self.current,
                token,
                pool,
                &mut self.revocations,
                &self.drain,
                &self.sender,
            )
            .await
        }

        async fn renew_without_database(&mut self) -> RenewalOutcome {
            self.renew(&well_formed_token(), None).await
        }
    }

    #[tokio::test]
    async fn renewal_interruption_precedes_missing_database_but_not_known_invalidation() {
        let mut fixture = ValidationFixture::new();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Close
        ));

        fixture.receiver.close();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Interrupted
        ));
        fixture.current.expires_at = Utc::now();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Close
        ));

        let mut fixture = ValidationFixture::new();
        fixture.drain.begin_draining();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Interrupted
        ));
        fixture
            .notice_sender
            .send((fixture.current.user, fixture.current.auth_version + 1))
            .unwrap();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Close
        ));
    }

    #[tokio::test]
    async fn renewal_refuses_a_malformed_token_before_any_query() {
        // Over this pool only an outcome other than `Unavailable` was decided
        // before any query.
        let pool = crate::signaling::fixtures::unreachable_pool().await;
        let mut fixture = ValidationFixture::new();
        // Empty, structured like a signed token, or longer than any token.
        for malformed in [
            String::new(),
            "header.payload.signature".into(),
            "A".repeat(129),
        ] {
            assert!(matches!(
                fixture.renew(&token(&malformed), Some(&pool)).await,
                RenewalOutcome::Rejected
            ));
        }
        // A well-formed token reaches the (failing) database.
        assert!(matches!(
            fixture.renew(&well_formed_token(), Some(&pool)).await,
            RenewalOutcome::Unavailable
        ));
    }

    #[tokio::test]
    async fn renewal_commits_only_current_database_validation() {
        let mut fixture = ValidationFixture::new();
        let old_expiry = fixture.current.expires_at;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(Err(AuthError::DatabaseError("fixture".into()))),
                    deadline
                )
                .await,
            RenewalOutcome::Unavailable
        ));
        assert_eq!(fixture.current.expires_at, old_expiry);

        let candidate = fixture.candidate.clone();
        let outcome = fixture
            .validate(std::future::ready(Ok(candidate)), deadline)
            .await;
        let RenewalOutcome::Renewed(renewed) = outcome else {
            panic!("only a current account check should renew the principal");
        };
        assert_eq!(renewed.expires_at, fixture.candidate.expires_at);
        assert_eq!(renewed.user, fixture.current.user);
        assert_eq!(fixture.current.expires_at, old_expiry);
    }

    #[tokio::test]
    async fn renewal_with_a_token_the_database_refuses_keeps_the_accepted_principal() {
        let mut fixture = ValidationFixture::new();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(Err(AuthError::InvalidToken)),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Rejected
        ));
    }

    #[tokio::test]
    async fn renewal_validation_deadline_is_unavailable_not_revocation() {
        let mut fixture = ValidationFixture::new();
        let old_expiry = fixture.current.expires_at;
        // An already-elapsed deadline avoids timing-sensitive database fixtures.
        assert!(matches!(
            fixture
                .validate(std::future::pending(), tokio::time::Instant::now())
                .await,
            RenewalOutcome::Unavailable
        ));
        assert_eq!(fixture.current.expires_at, old_expiry);
    }

    #[tokio::test]
    async fn renewal_ready_current_result_cannot_bypass_validation_deadline() {
        let mut fixture = ValidationFixture::new();
        let candidate = fixture.candidate.clone();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(Ok(candidate)),
                    tokio::time::Instant::now(),
                )
                .await,
            RenewalOutcome::Unavailable
        ));
    }

    #[tokio::test]
    async fn renewal_accepted_token_expiry_wins_ready_validation_and_timeout() {
        for database_failure in [false, true] {
            let mut fixture = ValidationFixture::new();
            fixture.current.expires_at = Utc::now();
            let result = if database_failure {
                Err(AuthError::DatabaseError("fixture".into()))
            } else {
                Ok(fixture.candidate.clone())
            };
            assert!(matches!(
                fixture
                    .validate(std::future::ready(result), tokio::time::Instant::now())
                    .await,
                RenewalOutcome::Close
            ));
        }
    }

    #[tokio::test]
    async fn renewal_revocation_queued_during_validation_wins_success() {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notice_sender.clone();
        let revoked_user = fixture.current.user;
        let revoked_version = fixture.current.auth_version + 1;
        let candidate = fixture.candidate.clone();
        let validation = async move {
            notices.send((revoked_user, revoked_version)).unwrap();
            Ok(candidate)
        };
        assert!(matches!(
            fixture
                .validate(
                    validation,
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Close
        ));
    }

    #[tokio::test]
    async fn renewal_lagged_revocations_win_database_unavailability() {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notice_sender.clone();
        let validation = async move {
            notices.send((UserId::random(), 1)).unwrap();
            notices.send((UserId::random(), 1)).unwrap();
            Err(AuthError::DatabaseError("fixture".into()))
        };
        assert!(matches!(
            fixture
                .validate(
                    validation,
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Close
        ));
    }

    #[tokio::test]
    async fn renewal_drain_and_transport_loss_remain_interruptions() {
        let mut fixture = ValidationFixture::new();
        fixture.drain.begin_draining();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(Err(AuthError::DatabaseError("fixture".into()))),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Interrupted
        ));

        let mut fixture = ValidationFixture::new();
        fixture.receiver.close();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(Err(AuthError::DatabaseError("fixture".into()))),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Interrupted
        ));
    }

    #[test]
    fn renewal_preserves_identity_and_expiry_without_requiring_a_new_membership() {
        let now = Utc::now();
        let current = principal(now + chrono::TimeDelta::seconds(100));
        let mut renewed = current.clone();
        renewed.expires_at = now + chrono::TimeDelta::seconds(200);
        assert!(renewal_matches(&current, &renewed, now));
        assert!(
            renewal_matches(&renewed, &renewed, current.expires_at),
            "duplicate renewal is idempotent"
        );
        assert!(
            !renewal_matches(&renewed, &current, now),
            "late renewal cannot shorten expiry"
        );
        assert!(
            !renewal_matches(&current, &renewed, current.expires_at),
            "expiry is not revived"
        );
        renewed.user = UserId::random();
        assert!(!renewal_matches(&current, &renewed, now));
        renewed.user = current.user;
        renewed.auth_version += 1;
        assert!(!renewal_matches(&current, &renewed, now));
    }

    #[test]
    fn renewal_refuses_a_token_from_another_session() {
        let now = Utc::now();
        let current = principal(now + chrono::TimeDelta::seconds(100));
        let mut candidate = current.clone();
        candidate.sid = Uuid::new_v4();
        assert!(
            !renewal_matches(&current, &candidate, now),
            "a token issued with another refresh session cannot renew this socket"
        );
        candidate.sid = current.sid;
        assert!(
            renewal_matches(&current, &candidate, now),
            "the same token from the socket's own session still renews"
        );
    }

    #[test]
    fn renewal_budget_is_small_and_resets_only_at_its_deadline() {
        let mut budget = RenewalBudget::new();
        let start = budget.window_started;
        for _ in 0..MAX_RENEWALS_PER_WINDOW {
            assert!(budget.allow(start));
        }
        assert!(!budget.allow(start + RENEWAL_WINDOW - Duration::from_nanos(1)));
        assert!(budget.allow(start + RENEWAL_WINDOW));
    }

    #[test]
    fn renewal_observes_queued_account_revocation() {
        let current = principal(later(100));
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        sender.send((UserId::random(), 1)).unwrap();
        assert!(revocations_current(&mut receiver, &current));
        sender
            .send((current.user, current.auth_version + 1))
            .unwrap();
        assert!(!revocations_current(&mut receiver, &current));
    }

    #[test]
    fn renewal_does_not_trust_closed_revocation_channels() {
        let current = principal(later(100));
        let (sender, mut receiver) = tokio::sync::broadcast::channel(1);
        drop(sender);
        assert!(!revocations_current(&mut receiver, &current));
    }
}
