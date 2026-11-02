//! Opaque access tokens.
//!
//! A sign-in or refresh issues a token: 32 random bytes, sent once as 43
//! unpadded base64url characters. The server keeps only the hex SHA-256 digest
//! of that text, computed in Rust with the rule refresh tokens use
//! (`session::hash_token`), so the bearer itself never reaches PostgreSQL. The
//! digest lives in `session_access_tokens` with the session it belongs to, the
//! account version it was issued at and its expiry.
//!
//! A session holds several tokens at once: browser tabs share one session (the
//! refresh cookie) but each keeps and refreshes its own token. Validation
//! resolves a bearer through its row, its live session and the account's
//! current version, so ending the session (logout, revocation, a password
//! change, recovery, cleanup) refuses every token of it at once, in the same
//! statement that deletes the session. Clients learn a token's expiry from the
//! response that carries it and never read anything from the token.

use super::{
    routes, session,
    types::{AuthError, Principal},
};
use crate::ids::UserId;
use crate::secret::SecretString;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use chrono::{DateTime, Utc};
use rand::{TryRng, rngs::SysRng};
use sqlx::{PgConnection, PgPool};
use std::time::Duration;
use uuid::Uuid;

/// How long an access token is accepted after it is issued, at most: a token
/// issued in its session's last fifteen minutes expires with the session.
pub(crate) const ACCESS_TOKEN_LIFETIME: Duration = Duration::from_secs(15 * 60);

/// Live tokens one session may hold; issuing beyond it deletes the oldest. A
/// tab holds at most two at a time (its current token and, for the last fifth
/// of that token's life, the one before it, because the client refreshes at
/// four fifths), so 32 covers eight tabs of one browser twice over and leaves
/// the rest for reloads, retries and the refresh every other tab makes when
/// one signs in or out. With `MAX_ACTIVE_SESSIONS_PER_USER` sessions an
/// account holds at most 1,024 rows.
pub(crate) const MAX_ACCESS_TOKENS_PER_SESSION: i64 = 32;

const TOKEN_BYTES: usize = 32;
const ENCODED_TOKEN_LEN: usize = 43;

/// Whether `token` has the only shape this server issues: 43 unpadded
/// base64url characters encoding 32 bytes. Everything else, including an
/// oversized or non-base64url bearer, is refused before any query.
pub(crate) fn valid_shape(token: &str) -> bool {
    token.len() == ENCODED_TOKEN_LEN
        && URL_SAFE_NO_PAD
            .decode(token)
            .is_ok_and(|bytes| bytes.len() == TOKEN_BYTES)
}

/// Issues an access token for session `sid` inside the caller's transaction,
/// which must already hold the account's row lock or have created the account
/// (every sign-in and refresh does, in the users-then-sessions order), and
/// returns the token with its expiry.
///
/// The row records the account version read in this same transaction, so a
/// token can never carry another version than the account had when it was
/// issued. Its expiry is `ACCESS_TOKEN_LIFETIME` after issue on the database
/// clock, the clock validation compares it with, but never later than the
/// session's own expiry, so the expiry the response states is the instant the
/// token stops being accepted. The same transaction then deletes the session's
/// expired tokens and, beyond `MAX_ACCESS_TOKENS_PER_SESSION` live ones, the
/// oldest. Nothing is visible until the caller commits.
///
/// # Errors
/// `AuthError::InvalidToken` when no such session exists (no row is written),
/// the `AuthError::DatabaseError` built by `routes::database_error` when a
/// query fails, and an `AuthError::DatabaseError` when the system random
/// generator cannot supply the token.
pub(crate) async fn issue_access_token(
    connection: &mut PgConnection,
    sid: Uuid,
) -> Result<(SecretString, DateTime<Utc>), AuthError> {
    let mut bytes = [0_u8; TOKEN_BYTES];
    SysRng.try_fill_bytes(&mut bytes).map_err(|error| {
        AuthError::DatabaseError(format!("Secure access-token generation failed: {error}"))
    })?;
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let expires_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO session_access_tokens
             (token_hash, session_id, auth_version, expires_at, created_at)
         SELECT $1, s.id, u.auth_version,
                LEAST(issued.at + make_interval(secs => $3), s.expires_at), issued.at
         FROM sessions s
         JOIN users u ON u.id = s.user_id
         CROSS JOIN (SELECT clock_timestamp() AS at) issued
         WHERE s.id = $2
         RETURNING expires_at",
    )
    .bind(session::hash_token(&token))
    .bind(sid)
    .bind(ACCESS_TOKEN_LIFETIME.as_secs_f64())
    .fetch_optional(&mut *connection)
    .await
    .map_err(routes::database_error)?
    .ok_or(AuthError::InvalidToken)?;
    sqlx::query(
        "DELETE FROM session_access_tokens
         WHERE session_id = $1
           AND (
             expires_at <= clock_timestamp()
             OR token_hash IN (
               SELECT token_hash
               FROM session_access_tokens
               WHERE session_id = $1 AND expires_at > clock_timestamp()
               ORDER BY created_at DESC, token_hash DESC
               OFFSET $2
             )
           )",
    )
    .bind(sid)
    .bind(MAX_ACCESS_TOKENS_PER_SESSION)
    .execute(&mut *connection)
    .await
    .map_err(routes::database_error)?;
    Ok((token.into(), expires_at))
}

#[derive(sqlx::FromRow)]
struct PrincipalRow {
    sid: Uuid,
    user_id: UserId,
    auth_version: i64,
    expires_at: DateTime<Utc>,
}

impl From<PrincipalRow> for Principal {
    fn from(row: PrincipalRow) -> Self {
        Self {
            user: row.user_id,
            sid: row.sid,
            auth_version: row.auth_version,
            expires_at: row.expires_at,
        }
    }
}

/// Resolves a bearer into the principal it speaks for: an unexpired token of
/// an unexpired session whose account still has the version the token was
/// issued at.
///
/// # Errors
/// `AuthError::InvalidToken` for a token of the wrong shape (before any
/// query) and for an unknown, expired or revoked one, alike; the
/// `AuthError::DatabaseError` built by `routes::database_error` when the
/// query fails.
pub(crate) async fn validate_access_token(
    pool: &PgPool,
    token: &str,
) -> Result<Principal, AuthError> {
    if !valid_shape(token) {
        return Err(AuthError::InvalidToken);
    }
    let row: Option<PrincipalRow> = sqlx::query_as(
        "SELECT s.id AS sid, s.user_id, t.auth_version, t.expires_at
         FROM session_access_tokens t
         JOIN sessions s ON s.id = t.session_id
         JOIN users u ON u.id = s.user_id
         WHERE t.token_hash = $1
           AND t.expires_at > clock_timestamp()
           AND s.expires_at > clock_timestamp()
           AND u.auth_version = t.auth_version",
    )
    .bind(session::hash_token(token))
    .fetch_optional(pool)
    .await
    .map_err(routes::database_error)?;
    row.map(Principal::from).ok_or(AuthError::InvalidToken)
}

/// Whether `principal`, accepted earlier, still names an unexpired session of
/// its account at the same account version. Connections that outlive the
/// request that authenticated them (account streams, sockets, their upgrade
/// tickets, reconnect grace and attachment grants) re-check this rather than
/// their token: the per-session cap may evict the token they were accepted
/// with while its session lives on, and a refresh in another tab must not end
/// them. Callers enforce `principal.expires_at` themselves.
///
/// # Errors
/// `AuthError::InvalidToken` when the session ended, expired or belongs to
/// another account, or the account's version changed; the
/// `AuthError::DatabaseError` built by `routes::database_error` when the
/// query fails.
pub(crate) async fn revalidate_principal(
    pool: &PgPool,
    principal: &Principal,
) -> Result<(), AuthError> {
    let current: bool = sqlx::query_scalar(
        "SELECT EXISTS (
             SELECT 1
             FROM sessions s
             JOIN users u ON u.id = s.user_id
             WHERE s.id = $1
               AND s.user_id = $2
               AND u.auth_version = $3
               AND s.expires_at > clock_timestamp()
         )",
    )
    .bind(principal.sid)
    .bind(principal.user)
    .bind(principal.auth_version)
    .fetch_one(pool)
    .await
    .map_err(routes::database_error)?;
    if current {
        Ok(())
    } else {
        Err(AuthError::InvalidToken)
    }
}

/// Tokens for database-backed tests, issued through the production issuer.
#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;

    /// A committed access token for session `sid`.
    pub(crate) async fn issue(pool: &PgPool, sid: Uuid) -> String {
        let mut transaction = pool.begin().await.unwrap();
        let (token, _) = issue_access_token(&mut transaction, sid).await.unwrap();
        transaction.commit().await.unwrap();
        token.expose_secret().to_owned()
    }

    /// The principal a fresh token for session `sid` speaks for.
    pub(crate) async fn principal(pool: &PgPool, sid: Uuid) -> Principal {
        let token = issue(pool, sid).await;
        validate_access_token(pool, &token).await.unwrap()
    }
}

#[cfg(test)]
#[path = "access_tokens_tests.rs"]
mod tests;
