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
    /// Transport loss/drain does not independently invalidate credentials.
    Interrupted,
    Close,
}

/// Consume already-delivered revocations before committing renewed claims.
/// A lagged/closed channel cannot establish current credentials for this commit.
fn revocations_current(
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
    if !revocations_current(revocations, current) {
        return RenewalOutcome::Close;
    }
    let Ok(_permit) = authenticator.concurrency.clone().try_acquire_owned() else {
        return RenewalOutcome::Rejected;
    };
    let Some(candidate) = authenticator.candidate(current, token) else {
        return RenewalOutcome::Rejected;
    };
    let deadline = tokio::time::Instant::now()
        + RENEWAL_VALIDATION_TIMEOUT.min(Duration::from_secs(
            (current.exp as u64).saturating_sub(unix_seconds()),
        ));
    let valid = {
        let validation = account_credentials_current(pool, &candidate);
        tokio::pin!(validation);
        loop {
            tokio::select! {
                biased;
                _ = drain.wait() => return RenewalOutcome::Interrupted,
                _ = sender.closed() => return RenewalOutcome::Interrupted,
                _ = tokio::time::sleep_until(deadline) => return RenewalOutcome::Close,
                notice = revocations.recv() => match notice {
                    Ok((subject, minimum_version)) if subject != current.sub || current.auth_version >= minimum_version => {},
                    _ => return RenewalOutcome::Close,
                },
                valid = &mut validation => break valid,
            }
        }
    };
    if drain.is_draining() {
        return RenewalOutcome::Interrupted;
    }
    if !valid
        || !revocations_current(revocations, current)
        || !renewal_matches(current, &candidate, unix_seconds())
    {
        return RenewalOutcome::Close;
    }
    RenewalOutcome::Renewed(candidate)
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
