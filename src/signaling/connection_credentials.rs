//! Bounded credential continuity for previously authenticated connections.
//!
//! Database uncertainty is not authoritative account revocation. A connection
//! may retain its already accepted claims for one short, monotonic window while
//! verification is unavailable. This policy never authenticates a handshake or
//! extends a token's accepted expiry; callers include that expiry in every wait.

use crate::auth::jwt::validate_current_claims;
use crate::auth::types::{AuthError, Claims};
use std::future::Future;
use std::time::{Duration, Instant};

/// Maximum uncertainty after the first observed database failure or timeout.
const CREDENTIAL_UNAVAILABLE_WINDOW: Duration = Duration::from_secs(15);

/// The account database either verifies the claims, rejects them, or cannot
/// establish their current validity within the caller's validation deadline.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum CredentialStatus {
    Current,
    Revoked,
    Unavailable,
}

/// Connection-owned policy, carried unchanged into retained reconnect grace.
///
/// Only a successful check before the uncertainty deadline clears that window.
/// Expiration and authoritative rejection are terminal for this connection;
/// neither a late result nor a later successful check can revive its claims.
#[derive(Clone, Debug, Default)]
pub(super) struct CredentialContinuity {
    unavailable_deadline: Option<Instant>,
    revalidation_required: bool,
    terminal: bool,
}

impl CredentialContinuity {
    /// Apply an authoritative result or a bounded validation failure. `true`
    /// permits retaining the existing claims, never accepting different claims.
    pub(super) fn observe(&mut self, status: CredentialStatus, now: Instant) -> bool {
        if self.expired(now) {
            self.terminal = true;
            return false;
        }

        match status {
            CredentialStatus::Current => {
                self.unavailable_deadline = None;
                self.revalidation_required = false;
                true
            }
            CredentialStatus::Revoked => {
                self.terminal = true;
                false
            }
            CredentialStatus::Unavailable if self.revalidation_required => {
                // Lost revocation notifications require proof of current
                // credentials, not the ordinary database-uncertainty allowance.
                self.terminal = true;
                false
            }
            CredentialStatus::Unavailable => {
                self.unavailable_deadline
                    .get_or_insert(now + CREDENTIAL_UNAVAILABLE_WINDOW);
                true
            }
        }
    }

    /// Absolute cap to include in receive, validation, renewal and grace waits.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.unavailable_deadline
    }

    /// Whether this connection can no longer retain its accepted credentials.
    /// A result at the exact uncertainty deadline is already too late.
    pub(super) fn expired(&self, now: Instant) -> bool {
        self.terminal
            || self
                .unavailable_deadline
                .is_some_and(|deadline| now >= deadline)
    }

    /// Force the next result to establish current credentials after a receiver
    /// loses revocation notices. This never clears an existing deadline.
    pub(super) fn require_revalidation(&mut self) {
        self.revalidation_required = true;
    }

    /// Whether the connection is retaining claims after database uncertainty.
    /// Callers can use transitions for bounded, credential-free diagnostics.
    pub(super) fn unavailable(&self) -> bool {
        self.unavailable_deadline.is_some() && !self.terminal
    }
}

/// Verify account state within a caller-owned absolute deadline. Missing
/// database configuration cannot establish authentication and remains terminal.
/// Callers cap the deadline by their query budget, accepted JWT expiry and any
/// existing uncertainty deadline before invoking this helper.
pub(super) async fn account_credentials_current(
    pool: Option<&sqlx::PgPool>,
    claims: &Claims,
    deadline: Instant,
) -> CredentialStatus {
    let Some(pool) = pool else {
        return CredentialStatus::Revoked;
    };
    classify_validation(validate_current_claims(pool, claims), deadline).await
}

/// Keep result classification independent of PostgreSQL for deterministic
/// policy tests. No database error text or credentials leave this boundary.
async fn classify_validation(
    validation: impl Future<Output = Result<(), AuthError>>,
    deadline: Instant,
) -> CredentialStatus {
    if Instant::now() >= deadline {
        return CredentialStatus::Unavailable;
    }
    let result = tokio::time::timeout_at(deadline.into(), validation).await;
    match result {
        // A ready future can be polled before a ready timeout. Do not accept a
        // nominal success that finishes at or beyond the caller's hard deadline.
        Ok(Ok(())) if Instant::now() < deadline => CredentialStatus::Current,
        Ok(Ok(())) => CredentialStatus::Unavailable,
        Ok(Err(AuthError::DatabaseError(_))) | Err(_) => CredentialStatus::Unavailable,
        // An authoritative rejection must never become a retryable outcome,
        // even if it completes at the deadline.
        Ok(Err(_)) => CredentialStatus::Revoked,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::{pending, ready};

    #[test]
    fn uncertainty_starts_once_and_repeated_failures_do_not_extend_it() {
        let now = Instant::now();
        let mut continuity = CredentialContinuity::default();
        assert!(!continuity.unavailable());
        assert!(!continuity.expired(now));
        assert!(continuity.deadline().is_none());

        assert!(continuity.observe(CredentialStatus::Unavailable, now));
        let deadline = now + CREDENTIAL_UNAVAILABLE_WINDOW;
        assert_eq!(continuity.deadline(), Some(deadline));
        assert!(continuity.unavailable());
        assert!(continuity.observe(
            CredentialStatus::Unavailable,
            deadline - Duration::from_nanos(1)
        ));
        assert_eq!(continuity.deadline(), Some(deadline));
        assert!(continuity.expired(deadline));
    }

    #[test]
    fn successful_revalidation_clears_uncertainty_before_its_deadline() {
        let now = Instant::now();
        let mut continuity = CredentialContinuity::default();
        assert!(continuity.observe(CredentialStatus::Unavailable, now));
        assert!(continuity.observe(CredentialStatus::Current, now + Duration::from_secs(14)));
        assert!(!continuity.unavailable());
        assert!(continuity.deadline().is_none());
        assert!(!continuity.expired(now + CREDENTIAL_UNAVAILABLE_WINDOW));

        let next_failure = now + Duration::from_secs(20);
        assert!(continuity.observe(CredentialStatus::Unavailable, next_failure));
        assert_eq!(
            continuity.deadline(),
            Some(next_failure + CREDENTIAL_UNAVAILABLE_WINDOW)
        );
    }

    #[test]
    fn no_result_can_revive_an_expired_uncertainty_window() {
        let now = Instant::now();
        for status in [
            CredentialStatus::Current,
            CredentialStatus::Revoked,
            CredentialStatus::Unavailable,
        ] {
            for late_by in [Duration::ZERO, Duration::from_secs(1)] {
                let mut continuity = CredentialContinuity::default();
                assert!(continuity.observe(CredentialStatus::Unavailable, now));
                let expired_at = now + CREDENTIAL_UNAVAILABLE_WINDOW + late_by;
                assert!(!continuity.observe(status, expired_at));
                assert!(continuity.expired(expired_at));
                assert!(!continuity.observe(
                    CredentialStatus::Current,
                    expired_at + Duration::from_secs(1)
                ));
            }
        }
    }

    #[test]
    fn authoritative_revocation_is_terminal_with_or_without_uncertainty() {
        let now = Instant::now();
        for unavailable_first in [false, true] {
            let mut continuity = CredentialContinuity::default();
            if unavailable_first {
                assert!(continuity.observe(CredentialStatus::Unavailable, now));
            }
            assert!(!continuity.observe(CredentialStatus::Revoked, now));
            assert!(continuity.expired(now));
            continuity.require_revalidation();
            assert!(!continuity.observe(CredentialStatus::Current, now));
            assert!(!continuity.observe(CredentialStatus::Unavailable, now));
        }
    }

    #[test]
    fn grace_transfer_retains_the_original_uncertainty_deadline() {
        let now = Instant::now();
        let mut active = CredentialContinuity::default();
        assert!(active.observe(CredentialStatus::Unavailable, now));
        let mut grace = active.clone();
        assert!(grace.observe(CredentialStatus::Unavailable, now + Duration::from_secs(10)));
        assert_eq!(grace.deadline(), active.deadline());
        assert!(!grace.observe(
            CredentialStatus::Current,
            now + CREDENTIAL_UNAVAILABLE_WINDOW
        ));
    }

    #[test]
    fn lost_revocation_notices_require_successful_revalidation() {
        let now = Instant::now();
        for unavailable_first in [false, true] {
            let mut continuity = CredentialContinuity::default();
            if unavailable_first {
                assert!(continuity.observe(CredentialStatus::Unavailable, now));
            }
            let original_deadline = continuity.deadline();
            continuity.require_revalidation();
            assert_eq!(continuity.deadline(), original_deadline);
            assert!(!continuity.expired(now));

            let mut failed = continuity.clone();
            assert!(!failed.observe(CredentialStatus::Unavailable, now));
            assert!(failed.expired(now));
            assert!(!failed.observe(CredentialStatus::Current, now));

            assert!(continuity.observe(CredentialStatus::Current, now));
            assert!(continuity.deadline().is_none());
            assert!(continuity.observe(CredentialStatus::Unavailable, now));
        }
    }

    #[tokio::test]
    async fn missing_database_configuration_remains_terminal() {
        let claims = Claims {
            sub: "credential-policy-fixture".into(),
            name: "Credential policy fixture".into(),
            iss: "simplestchat".into(),
            aud: "simplestchat".into(),
            exp: usize::MAX,
            auth_version: 0,
        };
        assert_eq!(
            account_credentials_current(None, &claims, Instant::now() + Duration::from_secs(5))
                .await,
            CredentialStatus::Revoked
        );
    }

    #[tokio::test]
    async fn validation_distinguishes_success_database_failure_and_terminal_errors() {
        let deadline = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            classify_validation(ready(Ok(())), deadline).await,
            CredentialStatus::Current
        );
        assert_eq!(
            classify_validation(
                ready(Err(AuthError::DatabaseError(
                    "private fixture error".into()
                ))),
                deadline
            )
            .await,
            CredentialStatus::Unavailable
        );
        for error in [
            AuthError::InvalidInput("fixture"),
            AuthError::InvalidCredentials,
            AuthError::EmailAlreadyExists,
            AuthError::UserNotFound,
            AuthError::InvalidToken,
            AuthError::MissingToken,
            AuthError::TokenExpired,
            AuthError::RateLimited,
            AuthError::ServiceBusy,
            AuthError::RegistrationDisabled,
            AuthError::WebAuthnError("fixture".into()),
            AuthError::NotConfigured,
        ] {
            assert_eq!(
                classify_validation(ready(Err(error)), deadline).await,
                CredentialStatus::Revoked
            );
        }
    }

    #[tokio::test]
    async fn pending_validation_is_bounded_by_the_callers_deadline() {
        assert_eq!(
            classify_validation(pending(), Instant::now() + Duration::from_millis(1)).await,
            CredentialStatus::Unavailable
        );
    }

    #[tokio::test]
    async fn elapsed_deadline_does_not_poll_an_apparently_successful_validation() {
        let validation = std::future::poll_fn(|_| -> std::task::Poll<Result<(), AuthError>> {
            panic!("validation must not start after the caller's deadline")
        });
        assert_eq!(
            classify_validation(validation, Instant::now()).await,
            CredentialStatus::Unavailable
        );
    }
}
