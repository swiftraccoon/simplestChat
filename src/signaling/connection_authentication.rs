//! Same-socket credential renewal preserves membership and absolute expiry.

use super::*;
use crate::auth::jwt;
use crate::signaling::protocol::AuthenticationToken;

const RENEWAL_WINDOW: Duration = Duration::from_secs(60);
const MAX_RENEWALS_PER_WINDOW: u8 = 3;
const RENEWAL_VALIDATION_TIMEOUT: Duration = Duration::from_secs(2);

/// Connection-local access to the existing authentication concurrency budget.
/// The signing secret is deliberately absent from `Debug` implementations.
#[derive(Clone)]
pub struct RenewalAuthenticator {
    secret: String,
    concurrency: Arc<tokio::sync::Semaphore>,
}

impl RenewalAuthenticator {
    pub(crate) fn new(secret: String, concurrency: Arc<tokio::sync::Semaphore>) -> Self {
        Self {
            secret,
            concurrency,
        }
    }

    fn candidate(&self, current: &Claims, token: &AuthenticationToken) -> Option<Claims> {
        if token.expose().is_empty() || token.expose().len() > 4_096 {
            return None;
        }
        let candidate = jwt::validate_token(token.expose(), &self.secret).ok()?;
        renewal_matches(current, &candidate, unix_seconds()).then_some(candidate)
    }
}

/// Renewals cannot change identity, revive expiry, or move expiry backwards.
fn renewal_matches(current: &Claims, candidate: &Claims, now: u64) -> bool {
    current.exp as u64 > now
        && candidate.exp >= current.exp
        && candidate.sub == current.sub
        && candidate.auth_version == current.auth_version
}

pub(super) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |duration| duration.as_secs())
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
    Renewed(Claims),
    Rejected,
    /// The database could not establish whether accepted credentials are current.
    /// Keep the previous claims; the connection owns the bounded uncertainty policy.
    Unavailable,
    /// Shared validation capacity is occupied; no database check was attempted.
    Busy,
    /// Transport loss/drain does not independently invalidate credentials.
    Interrupted,
    Close,
}

/// Consume already-delivered revocations before committing renewed claims.
/// A lagged/closed channel cannot establish current credentials for this commit.
pub(super) fn revocations_current(
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    current: &Claims,
) -> bool {
    // Bound synchronous work even if other accounts change credentials rapidly.
    for _ in 0..256 {
        match revocations.try_recv() {
            Ok((subject, minimum_version)) => {
                if subject == current.sub && current.auth_version < minimum_version {
                    return false;
                }
            }
            Err(tokio::sync::broadcast::error::TryRecvError::Empty) => return true,
            Err(_) => return false,
        }
    }
    false
}

/// Validate using the same database policy and shared admission budget as an
/// authenticated handshake. Waiting never extends the previous token lifetime.
pub(super) async fn renew_authentication(
    authenticator: &RenewalAuthenticator,
    current: &Claims,
    token: &AuthenticationToken,
    pool: Option<&sqlx::PgPool>,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    sender: &mpsc::Sender<Arc<String>>,
) -> RenewalOutcome {
    if current.exp as u64 <= unix_seconds() || !revocations_current(revocations, current) {
        return RenewalOutcome::Close;
    }
    if drain.is_draining() || sender.is_closed() {
        return RenewalOutcome::Interrupted;
    }
    if pool.is_none() {
        return RenewalOutcome::Close;
    }
    let Some(candidate) = authenticator.candidate(current, token) else {
        return if current.exp as u64 <= unix_seconds() {
            RenewalOutcome::Close
        } else {
            RenewalOutcome::Rejected
        };
    };
    let Ok(_permit) = authenticator.concurrency.clone().try_acquire_owned() else {
        return RenewalOutcome::Busy;
    };
    // Preserve the fractional second remaining before absolute JWT expiry.
    // Subtracting integer Unix seconds could otherwise extend validation past it.
    let remaining = std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(current.exp as u64))
        .and_then(|expiry| expiry.duration_since(std::time::SystemTime::now()).ok())
        .unwrap_or_default();
    if remaining.is_zero() {
        return RenewalOutcome::Close;
    }
    let deadline = tokio::time::Instant::now() + RENEWAL_VALIDATION_TIMEOUT.min(remaining);
    validate_renewal(
        current,
        &candidate,
        account_credentials_current(pool, &candidate, deadline.into_std()),
        revocations,
        drain,
        sender,
        deadline,
    )
    .await
}

/// Keep transport/revocation/expiry races separate from the database result.
/// Only a current result may commit new claims, and a local validation deadline
/// is temporary unavailability unless the accepted token has itself expired.
async fn validate_renewal(
    current: &Claims,
    candidate: &Claims,
    validation: impl std::future::Future<Output = CredentialStatus>,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    sender: &mpsc::Sender<Arc<String>>,
    deadline: tokio::time::Instant,
) -> RenewalOutcome {
    if !renewal_matches(current, candidate, unix_seconds())
        || !revocations_current(revocations, current)
    {
        return RenewalOutcome::Close;
    }
    let valid = {
        tokio::pin!(validation);
        loop {
            tokio::select! {
                biased;
                _ = drain.wait() => return RenewalOutcome::Interrupted,
                _ = sender.closed() => return RenewalOutcome::Interrupted,
                _ = tokio::time::sleep_until(deadline) => break CredentialStatus::Unavailable,
                notice = revocations.recv() => match notice {
                    Ok((subject, minimum_version)) if subject != current.sub || current.auth_version >= minimum_version => {},
                    _ => return RenewalOutcome::Close,
                },
                valid = &mut validation => break valid,
            }
        }
    };
    if matches!(valid, CredentialStatus::Revoked)
        || !revocations_current(revocations, current)
        || !renewal_matches(current, candidate, unix_seconds())
    {
        return RenewalOutcome::Close;
    }
    if drain.is_draining() || sender.is_closed() {
        return RenewalOutcome::Interrupted;
    }
    if tokio::time::Instant::now() >= deadline {
        return RenewalOutcome::Unavailable;
    }
    match valid {
        CredentialStatus::Current => RenewalOutcome::Renewed(candidate.clone()),
        CredentialStatus::Unavailable => RenewalOutcome::Unavailable,
        CredentialStatus::Revoked => RenewalOutcome::Close,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(exp: usize) -> Claims {
        Claims {
            sub: Uuid::new_v4().to_string(),
            name: "Renewal test".into(),
            iss: "simplestchat".into(),
            aud: "simplestchat".into(),
            exp,
            auth_version: 0,
        }
    }

    struct ValidationFixture {
        current: Claims,
        candidate: Claims,
        notice_sender: tokio::sync::broadcast::Sender<(String, i64)>,
        revocations: tokio::sync::broadcast::Receiver<(String, i64)>,
        drain: crate::shutdown::DrainSignal,
        sender: mpsc::Sender<Arc<String>>,
        receiver: mpsc::Receiver<Arc<String>>,
    }

    impl ValidationFixture {
        fn new() -> Self {
            let current = claims(unix_seconds() as usize + 60);
            let mut candidate = current.clone();
            candidate.exp += 60;
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
            validation: impl std::future::Future<Output = CredentialStatus>,
            deadline: tokio::time::Instant,
        ) -> RenewalOutcome {
            validate_renewal(
                &self.current,
                &self.candidate,
                validation,
                &mut self.revocations,
                &self.drain,
                &self.sender,
                deadline,
            )
            .await
        }

        async fn renew_without_database(&mut self) -> RenewalOutcome {
            let secret = "renewal-priority-fixture-secret-at-least-32-bytes";
            let token = jwt::create_token(&self.current.sub, &self.current.name, secret).unwrap();
            let token = serde_json::from_value(serde_json::json!(token)).unwrap();
            let authenticator =
                RenewalAuthenticator::new(secret.into(), Arc::new(tokio::sync::Semaphore::new(1)));
            renew_authentication(
                &authenticator,
                &self.current,
                &token,
                None,
                &mut self.revocations,
                &self.drain,
                &self.sender,
            )
            .await
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
        fixture.current.exp = unix_seconds() as usize;
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
            .send((
                fixture.current.sub.clone(),
                fixture.current.auth_version + 1,
            ))
            .unwrap();
        assert!(matches!(
            fixture.renew_without_database().await,
            RenewalOutcome::Close
        ));
    }

    #[tokio::test]
    async fn renewal_commits_only_current_database_validation() {
        let mut fixture = ValidationFixture::new();
        let old_expiry = fixture.current.exp;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        assert!(matches!(
            fixture
                .validate(std::future::ready(CredentialStatus::Unavailable), deadline)
                .await,
            RenewalOutcome::Unavailable
        ));
        assert_eq!(fixture.current.exp, old_expiry);

        let outcome = fixture
            .validate(std::future::ready(CredentialStatus::Current), deadline)
            .await;
        let RenewalOutcome::Renewed(renewed) = outcome else {
            panic!("only a current account check should renew claims");
        };
        assert_eq!(renewed.exp, fixture.candidate.exp);
        assert_eq!(renewed.sub, fixture.current.sub);
        assert_eq!(fixture.current.exp, old_expiry);
    }

    #[tokio::test]
    async fn renewal_database_revocation_still_closes() {
        let mut fixture = ValidationFixture::new();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(CredentialStatus::Revoked),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Close
        ));
    }

    #[tokio::test]
    async fn renewal_validation_deadline_is_unavailable_not_revocation() {
        let mut fixture = ValidationFixture::new();
        let old_expiry = fixture.current.exp;
        // An already-elapsed deadline avoids timing-sensitive database fixtures.
        assert!(matches!(
            fixture
                .validate(std::future::pending(), tokio::time::Instant::now())
                .await,
            RenewalOutcome::Unavailable
        ));
        assert_eq!(fixture.current.exp, old_expiry);
    }

    #[tokio::test]
    async fn renewal_ready_current_result_cannot_bypass_validation_deadline() {
        let mut fixture = ValidationFixture::new();
        assert!(matches!(
            fixture
                .validate(
                    std::future::ready(CredentialStatus::Current),
                    tokio::time::Instant::now(),
                )
                .await,
            RenewalOutcome::Unavailable
        ));
    }

    #[tokio::test]
    async fn renewal_accepted_token_expiry_wins_ready_validation_and_timeout() {
        for status in [CredentialStatus::Current, CredentialStatus::Unavailable] {
            let mut fixture = ValidationFixture::new();
            fixture.current.exp = unix_seconds() as usize;
            assert!(matches!(
                fixture
                    .validate(std::future::ready(status), tokio::time::Instant::now())
                    .await,
                RenewalOutcome::Close
            ));
        }
    }

    #[tokio::test]
    async fn renewal_revocation_queued_during_validation_wins_success() {
        let mut fixture = ValidationFixture::new();
        let notices = fixture.notice_sender.clone();
        let revoked_subject = fixture.current.sub.clone();
        let revoked_version = fixture.current.auth_version + 1;
        let validation = async move {
            notices.send((revoked_subject, revoked_version)).unwrap();
            CredentialStatus::Current
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
            notices.send((Uuid::new_v4().to_string(), 1)).unwrap();
            notices.send((Uuid::new_v4().to_string(), 1)).unwrap();
            CredentialStatus::Unavailable
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
                    std::future::ready(CredentialStatus::Unavailable),
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
                    std::future::ready(CredentialStatus::Unavailable),
                    tokio::time::Instant::now() + Duration::from_secs(1),
                )
                .await,
            RenewalOutcome::Interrupted
        ));
    }

    #[test]
    fn renewal_preserves_identity_and_expiry_without_requiring_a_new_membership() {
        let current = claims(200);
        let mut renewed = current.clone();
        renewed.exp = 300;
        assert!(renewal_matches(&current, &renewed, 100));
        assert!(
            renewal_matches(&renewed, &renewed, 200),
            "duplicate renewal is idempotent"
        );
        assert!(
            !renewal_matches(&renewed, &current, 100),
            "late renewal cannot shorten expiry"
        );
        assert!(
            !renewal_matches(&current, &renewed, 200),
            "expiry is not revived"
        );
        renewed.sub = Uuid::new_v4().to_string();
        assert!(!renewal_matches(&current, &renewed, 100));
        renewed.sub.clone_from(&current.sub);
        renewed.auth_version += 1;
        assert!(!renewal_matches(&current, &renewed, 100));
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
        let current = claims(200);
        let (sender, mut receiver) = tokio::sync::broadcast::channel(4);
        sender.send((Uuid::new_v4().to_string(), 1)).unwrap();
        assert!(revocations_current(&mut receiver, &current));
        sender
            .send((current.sub.clone(), current.auth_version + 1))
            .unwrap();
        assert!(!revocations_current(&mut receiver, &current));
    }

    #[test]
    fn renewal_does_not_trust_closed_revocation_channels() {
        let current = claims(200);
        let (sender, mut receiver) = tokio::sync::broadcast::channel(1);
        drop(sender);
        assert!(!revocations_current(&mut receiver, &current));
    }

    #[test]
    fn renewal_validator_accepts_an_ordinary_refreshed_token() {
        let secret = "ordinary-renewal-fixture-secret-at-least-32-bytes";
        let mut current = claims(unix_seconds() as usize + 60);
        let token = jwt::create_token(&current.sub, &current.name, secret).unwrap();
        let token: AuthenticationToken = serde_json::from_value(serde_json::json!(token)).unwrap();
        let auth =
            RenewalAuthenticator::new(secret.into(), Arc::new(tokio::sync::Semaphore::new(1)));
        let refreshed = auth.candidate(&current, &token).unwrap();
        assert_eq!(refreshed.sub, current.sub);
        assert!(refreshed.exp > current.exp);
        current.exp = unix_seconds() as usize;
        assert!(auth.candidate(&current, &token).is_none());
    }
}
