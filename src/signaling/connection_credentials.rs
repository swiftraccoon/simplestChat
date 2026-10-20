//! Bounded credential continuity for previously authenticated connections.
//!
//! Database uncertainty is not authoritative account revocation. A connection
//! may retain its already accepted claims for one short, monotonic window while
//! verification is unavailable. This policy never authenticates a handshake or
//! extends a token's accepted expiry; callers include that expiry in every wait.

use super::authentication::{
    self, RenewalAuthenticator, RenewalBudget, RenewalOutcome, renew_authentication, unix_seconds,
};
use crate::auth::jwt::validate_current_claims;
use crate::auth::types::{AuthError, Claims};
use crate::shutdown::DrainSignal;
use crate::signaling::protocol::{AuthenticationToken, ServerMessage};
use std::future::Future;
use std::pin::Pin;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, mpsc};
use tracing::{info, warn};

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

/// Owned form for a check that lives across loop iterations as its own
/// `select!` arm, so a slow database never blocks that socket's frame dispatch.
pub(super) async fn account_credentials_current_owned(
    pool: Option<sqlx::PgPool>,
    claims: Claims,
    deadline: Instant,
) -> CredentialStatus {
    account_credentials_current(pool.as_ref(), &claims, deadline).await
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

/// How often an authenticated socket re-checks its account while it stays open.
pub(super) const AUTH_REVALIDATE_INTERVAL: Duration = Duration::from_secs(5);

/// Convert accepted wall-clock expiry once, retaining fractional-second precision.
fn credential_expiry_deadline(exp: u64) -> Instant {
    let now = Instant::now();
    let remaining = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(Duration::ZERO, |elapsed| {
            Duration::from_secs(exp).saturating_sub(elapsed)
        });
    now.checked_add(remaining).unwrap_or(now)
}

/// Revalidation waits are bounded independently of PostgreSQL's statement limit.
/// Revocation notices and drain remain observable while the query is pending.
/// `None` means drain, not invalid credentials. The caller supplies its original
/// accepted expiry (and, for a retained session, its original grace deadline).
pub(super) async fn revalidate_account(
    pool: Option<&sqlx::PgPool>,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
) -> Option<bool> {
    let now = Instant::now();
    if continuity.expired(now) || now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return Some(false);
    }
    let deadline = continuity
        .deadline()
        .map_or(hard_deadline, |limit| limit.min(hard_deadline))
        .min(now + AUTH_REVALIDATE_INTERVAL);
    let validation = account_credentials_current(pool, claims, deadline);
    observe_account_validation(
        validation,
        claims,
        continuity,
        revocations,
        drain,
        hard_deadline,
    )
    .await
}

/// Query budget for one validation attempt, or `None` when the accepted
/// credentials are already invalid before any query is issued.
fn validation_deadline(
    claims: &Claims,
    continuity: &CredentialContinuity,
    hard_deadline: Instant,
) -> Option<Instant> {
    let now = Instant::now();
    if continuity.expired(now) || now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return None;
    }
    Some(
        continuity
            .deadline()
            .map_or(hard_deadline, |limit| limit.min(hard_deadline))
            .min(now + AUTH_REVALIDATE_INTERVAL),
    )
}

/// Applies one completed validation. `None` means drain, not invalid
/// credentials. Shared by the awaited form below and the connection loop's
/// pending-validation arm so both apply identical policy.
fn settle_account_validation(
    result: CredentialStatus,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
    deadline: Instant,
) -> Option<bool> {
    let now = Instant::now();
    if drain.is_draining() {
        return None;
    }
    if now >= hard_deadline || claims.exp as u64 <= unix_seconds() {
        return Some(false);
    }
    let result = if !authentication::revocations_current(revocations, claims) {
        CredentialStatus::Revoked
    } else if result == CredentialStatus::Current && now >= deadline {
        CredentialStatus::Unavailable
    } else {
        result
    };
    Some(retain_credentials(continuity, result, now))
}

/// Keep control-plane races testable without modifying a database or accepting
/// injected validators at the public connection boundary.
pub(super) async fn observe_account_validation(
    validation: impl std::future::Future<Output = CredentialStatus>,
    claims: &Claims,
    continuity: &mut CredentialContinuity,
    revocations: &mut tokio::sync::broadcast::Receiver<(String, i64)>,
    drain: &crate::shutdown::DrainSignal,
    hard_deadline: Instant,
) -> Option<bool> {
    let Some(deadline) = validation_deadline(claims, continuity, hard_deadline) else {
        return Some(false);
    };
    tokio::pin!(validation);
    let result = loop {
        tokio::select! {
            biased;
            _ = drain.wait() => return None,
            _ = tokio::time::sleep_until(deadline.into()) => break CredentialStatus::Unavailable,
            notice = revocations.recv() => match notice {
                Ok((subject, version)) if subject == claims.sub && claims.auth_version < version => break CredentialStatus::Revoked,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break CredentialStatus::Revoked,
                // The in-flight query may predate a lost revocation. It cannot
                // satisfy a fresh-validation requirement after notification loss.
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => break CredentialStatus::Revoked,
                _ => {},
            },
            result = &mut validation => break result,
        }
    };
    settle_account_validation(
        result,
        claims,
        continuity,
        revocations,
        drain,
        hard_deadline,
        deadline,
    )
}

/// Log only state transitions, never per-check credentials or database errors.
fn retain_credentials(
    continuity: &mut CredentialContinuity,
    result: CredentialStatus,
    now: Instant,
) -> bool {
    let was_unavailable = continuity.unavailable();
    let accepted = continuity.observe(result, now);
    if !was_unavailable && continuity.unavailable() && accepted {
        warn!(
            event = "authentication_validation_unavailable",
            "Retaining established credentials within the bounded uncertainty allowance"
        );
    } else if was_unavailable && result == CredentialStatus::Current && accepted {
        info!(
            event = "authentication_validation_restored",
            "Account validation recovered before its deadline"
        );
    }
    accepted
}

/// The accepted credentials of one connection and everything that bounds them:
/// the token's expiry, the continuity allowance, the periodic account check
/// (polled as its own arm of the connection loop, so a slow database delays
/// only the verdict, never frame dispatch) and the renewal budget. A guest
/// socket has no claims and nothing here ever closes it.
pub(super) struct CredentialSession {
    claims: Option<Claims>,
    exp: Option<u64>,
    deadline: Option<Instant>,
    continuity: CredentialContinuity,
    /// The account check in flight, if any; the loop polls it as one arm.
    pub(super) pending: Option<Pin<Box<dyn Future<Output = CredentialStatus> + Send>>>,
    pending_deadline: Instant,
    /// When the next periodic account check is due.
    pub(super) next_check: Instant,
    renewal_budget: RenewalBudget,
    invalidated: bool,
}

impl CredentialSession {
    pub(super) fn new(claims: Option<Claims>) -> Self {
        let exp = claims.as_ref().map(|claims| claims.exp as u64);
        Self {
            claims,
            exp,
            deadline: exp.map(credential_expiry_deadline),
            continuity: CredentialContinuity::default(),
            pending: None,
            pending_deadline: Instant::now(),
            next_check: Instant::now(),
            renewal_budget: RenewalBudget::new(),
            invalidated: false,
        }
    }

    pub(super) fn claims(&self) -> Option<&Claims> {
        self.claims.as_ref()
    }

    /// The accepted token's absolute expiry, when the socket is authenticated.
    pub(super) fn deadline(&self) -> Option<Instant> {
        self.deadline
    }

    /// The continuity policy as it stands, for a retained reconnect grace.
    pub(super) fn continuity(&self) -> CredentialContinuity {
        self.continuity.clone()
    }

    /// Whether the credentials were rejected, revoked or outlived: such a
    /// socket closes without reconnect grace.
    pub(super) fn invalidated(&self) -> bool {
        self.invalidated
    }

    pub(super) fn invalidate(&mut self) {
        self.invalidated = true;
    }

    /// Whether the accepted token lifetime or the validation allowance has run out.
    pub(super) fn expired(&self) -> bool {
        let now = Instant::now();
        self.exp.is_some_and(|exp| exp <= unix_seconds())
            || self.deadline.is_some_and(|deadline| now >= deadline)
            || self.continuity.expired(now)
    }

    fn hard_deadline(&self) -> Instant {
        self.deadline.unwrap_or_else(Instant::now)
    }

    /// How long the next receive may wait: the idle allowance, capped by the
    /// token's expiry and the continuity allowance. The JWT is a connection
    /// credential, not only a handshake credential, so active traffic cannot
    /// keep an authenticated socket alive past it.
    pub(super) fn receive_deadline(&self, idle_remaining: Duration) -> Instant {
        let now_unix = unix_seconds();
        let timeout = self
            .exp
            .map(|exp| Duration::from_secs(exp.saturating_sub(now_unix)))
            .map_or(idle_remaining, |remaining| remaining.min(idle_remaining));
        let deadline = Instant::now() + timeout;
        let deadline = self.deadline.map_or(deadline, |limit| limit.min(deadline));
        self.continuity
            .deadline()
            .map_or(deadline, |limit| limit.min(deadline))
    }

    /// Apply a notice from the account hub. `true` means the socket must close:
    /// its account was revoked at or above its version, or the hub is gone.
    /// Lost notices discard the check in flight (it predates them) and demand
    /// a fresh one.
    pub(super) fn revoked_by(
        &mut self,
        notice: Result<(String, i64), broadcast::error::RecvError>,
    ) -> bool {
        match notice {
            Ok((subject, minimum_version))
                if self.claims.as_ref().is_some_and(|claims| {
                    claims.sub == subject && claims.auth_version < minimum_version
                }) =>
            {
                self.invalidated = true;
                true
            }
            Err(broadcast::error::RecvError::Lagged(_)) => {
                self.pending = None;
                self.continuity.require_revalidation();
                self.next_check = Instant::now();
                false
            }
            Err(broadcast::error::RecvError::Closed) => {
                self.invalidated = true;
                true
            }
            _ => false,
        }
    }

    /// Start the periodic account check; `false` means the accepted
    /// credentials are already invalid and the socket must close.
    pub(super) fn begin_validation(&mut self, pool: Option<sqlx::PgPool>) -> bool {
        let hard_deadline = self.hard_deadline();
        let Some(claims) = self.claims.as_ref() else {
            return true;
        };
        match validation_deadline(claims, &self.continuity, hard_deadline) {
            Some(deadline) => {
                self.pending_deadline = deadline;
                self.pending = Some(Box::pin(account_credentials_current_owned(
                    pool,
                    claims.clone(),
                    deadline,
                )));
                true
            }
            None => {
                self.invalidated = true;
                false
            }
        }
    }

    /// Apply the finished account check; `false` means the loop ends (the
    /// credentials were rejected, or the server is draining).
    pub(super) fn settle(
        &mut self,
        result: CredentialStatus,
        revocations: &mut broadcast::Receiver<(String, i64)>,
        drain: &DrainSignal,
    ) -> bool {
        self.pending = None;
        let hard_deadline = self.hard_deadline();
        let pending_deadline = self.pending_deadline;
        if let Some(claims) = self.claims.as_ref() {
            match settle_account_validation(
                result,
                claims,
                &mut self.continuity,
                revocations,
                drain,
                hard_deadline,
                pending_deadline,
            ) {
                Some(true) => {}
                Some(false) => {
                    self.invalidated = true;
                    return false;
                }
                None => return false,
            }
        }
        self.next_check = Instant::now() + AUTH_REVALIDATE_INTERVAL;
        true
    }

    /// Attempt a credential renewal within this socket's remaining lifetime.
    pub(super) async fn renew(
        &mut self,
        authenticator: Option<&RenewalAuthenticator>,
        token: &AuthenticationToken,
        pool: Option<&sqlx::PgPool>,
        revocations: &mut broadcast::Receiver<(String, i64)>,
        drain: &DrainSignal,
        sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> RenewalOutcome {
        if !self.renewal_budget.allow(Instant::now()) {
            return RenewalOutcome::Rejected;
        }
        let (Some(authenticator), Some(current)) = (authenticator, self.claims.as_ref()) else {
            return RenewalOutcome::Rejected;
        };
        let hard_deadline = self.hard_deadline();
        let deadline = self
            .continuity
            .deadline()
            .map_or(hard_deadline, |limit| limit.min(hard_deadline));
        let renewal = renew_authentication(
            authenticator,
            current,
            token,
            pool,
            revocations,
            drain,
            sender,
        );
        let outcome = tokio::time::timeout_at(deadline.into(), renewal)
            .await
            .unwrap_or(RenewalOutcome::Close);
        if Instant::now() >= deadline {
            RenewalOutcome::Close
        } else {
            outcome
        }
    }

    /// Adopt a renewal's outcome as the reply to send. `None` means the socket
    /// closes: renewed or deferred credentials the continuity allowance refuses,
    /// a close verdict, or an interrupted renewal.
    pub(super) fn apply_renewal(
        &mut self,
        outcome: RenewalOutcome,
        request_id: &str,
    ) -> Option<ServerMessage> {
        let now = Instant::now();
        let unavailable = matches!(outcome, RenewalOutcome::Unavailable);
        Some(match outcome {
            RenewalOutcome::Renewed(claims) => {
                if !retain_credentials(&mut self.continuity, CredentialStatus::Current, now) {
                    self.invalidated = true;
                    return None;
                }
                let expires_at = claims.exp as u64;
                self.exp = Some(expires_at);
                self.deadline = Some(credential_expiry_deadline(expires_at));
                self.claims = Some(claims);
                self.next_check = now + AUTH_REVALIDATE_INTERVAL;
                ServerMessage::AuthenticationRenewed {
                    request_id: request_id.to_owned(),
                    expires_at,
                }
            }
            RenewalOutcome::Rejected => ServerMessage::AuthenticationRenewalFailed {
                request_id: request_id.to_owned(),
            },
            RenewalOutcome::Unavailable | RenewalOutcome::Busy => {
                if unavailable
                    && !retain_credentials(&mut self.continuity, CredentialStatus::Unavailable, now)
                {
                    self.invalidated = true;
                    return None;
                }
                ServerMessage::AuthenticationRenewalDeferred {
                    request_id: request_id.to_owned(),
                    retry_after_ms: 3000,
                    expires_at: self.exp.unwrap_or_default(),
                }
            }
            RenewalOutcome::Close => {
                self.invalidated = true;
                return None;
            }
            RenewalOutcome::Interrupted => return None,
        })
    }

    /// One last account check when a socket closes with its credentials still
    /// accepted; a rejection denies the reconnect grace that would follow.
    pub(super) async fn revalidate_on_disconnect(
        &mut self,
        pool: Option<&sqlx::PgPool>,
        revocations: &mut broadcast::Receiver<(String, i64)>,
        drain: &DrainSignal,
    ) {
        if self.invalidated {
            return;
        }
        let hard_deadline = self.hard_deadline();
        if let Some(claims) = self.claims.as_ref() {
            self.invalidated = revalidate_account(
                pool,
                claims,
                &mut self.continuity,
                revocations,
                drain,
                hard_deadline,
            )
            .await
                == Some(false);
        }
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
            sid: uuid::Uuid::new_v4(),
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
