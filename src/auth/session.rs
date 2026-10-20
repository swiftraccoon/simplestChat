#![forbid(unsafe_code)]

use crate::auth::types::AuthError;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{TryRng, rngs::SysRng};
use sha2::{Digest, Sha256};
use sqlx::{Executor, PgPool, Postgres, Transaction};
use uuid::Uuid;

pub(super) const MAX_ACTIVE_SESSIONS_PER_USER: i64 = 32;
const REFRESH_SECRET_BYTES: usize = 32;
const REFRESH_TOKEN_PREFIX: &str = "v1n";
const ENCODED_REFRESH_SECRET_LEN: usize = 43;
// Permit an immediately concurrent request to lose the rotation race without
// treating it as theft. It receives no token and cannot extend this window.
pub(crate) const CONCURRENT_REFRESH_GRACE_SECONDS: i64 = 2;

pub(crate) struct RefreshToken {
    pub(crate) raw: String,
    token_hash: String,
    family_hash: String,
}

pub(crate) enum RefreshRotation {
    Rotated {
        user_id: Uuid,
        session_id: Uuid,
        refresh_token: RefreshToken,
    },
    /// The exact predecessor was presented within the short concurrency grace
    /// period. It is rejected, but does not revoke the winner's successor.
    ConcurrentRequest,
    /// A predecessor outside the grace period (or an older family token) was
    /// replayed, and the current successor family has been revoked.
    ReuseDetected,
}

pub(crate) fn generate_refresh_token() -> Result<RefreshToken, AuthError> {
    let family_secret = random_secret()?;
    build_refresh_token(&family_secret)
}

pub(crate) fn hash_token(raw: &str) -> String {
    hash_bytes(raw.as_bytes())
}

fn hash_bytes(value: &[u8]) -> String {
    hex::encode(Sha256::digest(value))
}

fn random_secret() -> Result<[u8; REFRESH_SECRET_BYTES], AuthError> {
    let mut secret = [0_u8; REFRESH_SECRET_BYTES];
    SysRng.try_fill_bytes(&mut secret).map_err(|error| {
        AuthError::DatabaseError(format!("Secure refresh-token generation failed: {error}"))
    })?;
    Ok(secret)
}

/// The token remains opaque to clients. The generation secret comes first so
/// log truncation is less likely to expose the stable family capability.
fn build_refresh_token(
    family_secret: &[u8; REFRESH_SECRET_BYTES],
) -> Result<RefreshToken, AuthError> {
    let generation_secret = random_secret()?;
    let raw = format!(
        "{REFRESH_TOKEN_PREFIX}{}{}",
        URL_SAFE_NO_PAD.encode(generation_secret),
        URL_SAFE_NO_PAD.encode(family_secret)
    );
    Ok(RefreshToken {
        token_hash: hash_token(&raw),
        family_hash: hash_bytes(family_secret),
        raw,
    })
}

/// Validate the sole opaque refresh-token format before any database lookup.
pub(super) fn refresh_token_is_valid(raw_token: &str) -> bool {
    family_secret(raw_token).is_some()
}

/// Return the 256-bit stable family capability from the current token format.
fn family_secret(raw_token: &str) -> Option<[u8; REFRESH_SECRET_BYTES]> {
    let payload = raw_token.strip_prefix(REFRESH_TOKEN_PREFIX)?;
    if payload.len() != ENCODED_REFRESH_SECRET_LEN * 2 || !payload.is_ascii() {
        return None;
    }
    let (generation, family) = payload.split_at(ENCODED_REFRESH_SECRET_LEN);
    let generation = URL_SAFE_NO_PAD.decode(generation).ok()?;
    if generation.len() != REFRESH_SECRET_BYTES {
        return None;
    }
    URL_SAFE_NO_PAD.decode(family).ok()?.try_into().ok()
}

#[cfg(test)]
pub(crate) async fn create_session(
    pool: &PgPool,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<Uuid, AuthError> {
    let mut transaction = pool
        .begin()
        .await
        .map_err(crate::auth::routes::database_error)?;
    let session = create_session_with(&mut transaction, user_id, refresh_token).await?;
    transaction
        .commit()
        .await
        .map_err(crate::auth::routes::database_error)?;
    Ok(session)
}

pub(crate) async fn create_session_with(
    transaction: &mut Transaction<'_, Postgres>,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<Uuid, AuthError> {
    // Serialize login/session-cap maintenance per account. Refresh rotation
    // takes the same lock before mutating a session, so every path follows the
    // users->sessions lock order and the cap remains exact under concurrency.
    sqlx::query("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::auth::routes::database_error)?;
    let new_session_id = insert_session_with(&mut **transaction, user_id, refresh_token).await?;

    // A user who knows valid credentials must not be able to grow the sessions
    // table without bound by repeatedly logging in. Keep the newest sessions,
    // and remove expired rows for this user in the same transaction.
    sqlx::query(
        "DELETE FROM sessions
         WHERE user_id = $1
           AND id <> $2
           AND (
             expires_at <= now()
             OR id IN (
               SELECT id
               FROM sessions
               WHERE user_id = $1 AND id <> $2 AND expires_at > now()
               ORDER BY created_at DESC, id DESC
               OFFSET $3
             )
           )",
    )
    .bind(user_id)
    .bind(new_session_id)
    .bind(MAX_ACTIVE_SESSIONS_PER_USER - 1)
    .execute(&mut **transaction)
    .await
    .map_err(crate::auth::routes::database_error)?;
    Ok(new_session_id)
}

async fn insert_session_with<'e, E>(
    executor: E,
    user_id: &Uuid,
    refresh_token: &RefreshToken,
) -> Result<Uuid, AuthError>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query_scalar(
        "INSERT INTO sessions (
             user_id, refresh_token_hash, refresh_token_family_hash, expires_at
         ) VALUES ($1, $2, $3, clock_timestamp() + interval '7 days')
         RETURNING id",
    )
    .bind(user_id)
    .bind(&refresh_token.token_hash)
    .bind(&refresh_token.family_hash)
    .fetch_one(executor)
    .await
    .map_err(crate::auth::routes::database_error)
}

pub(crate) async fn rotate_refresh_token_with(
    transaction: &mut Transaction<'_, Postgres>,
    raw_token: &str,
) -> Result<RefreshRotation, AuthError> {
    let token_hash = hash_token(raw_token);
    let family_secret = family_secret(raw_token).ok_or(AuthError::InvalidToken)?;
    let family_hash = hash_bytes(&family_secret);

    // Resolve the account without taking a session-row lock. Both current-token
    // and family lookups use 256-bit hashes and return the same external error,
    // so random input cannot act as a practical account-existence oracle.
    let candidate_user_id: Uuid = sqlx::query_scalar(
        "SELECT user_id
         FROM sessions
         WHERE expires_at > clock_timestamp()
           AND (
             refresh_token_hash = $1
             OR refresh_token_family_hash = $2
           )
         ORDER BY (refresh_token_hash = $1) DESC
         LIMIT 1",
    )
    .bind(&token_hash)
    .bind(&family_hash)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(crate::auth::routes::database_error)?
    .ok_or(AuthError::InvalidToken)?;

    let locked_user: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(candidate_user_id)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(crate::auth::routes::database_error)?;
    if locked_user.is_none() {
        return Err(AuthError::InvalidToken);
    }

    // Re-check after acquiring the per-user lock. All login and refresh paths
    // use users->sessions ordering, so concurrent rotations serialize without
    // lock inversion and a rotation never changes the session count.
    let current_session = sqlx::query_as::<_, (Uuid, String)>(
        "SELECT id, refresh_token_family_hash
         FROM sessions
         WHERE refresh_token_hash = $1
           AND user_id = $2
           AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(&token_hash)
    .bind(candidate_user_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(crate::auth::routes::database_error)?;

    if let Some((session_id, stored_family_hash)) = current_session {
        if stored_family_hash != family_hash {
            return Err(AuthError::InvalidToken);
        }

        let successor = build_refresh_token(&family_secret)?;
        sqlx::query(
            "UPDATE sessions
             SET refresh_token_hash = $1,
                 refresh_token_family_hash = $2,
                 previous_refresh_token_hash = $3,
                 refresh_token_rotated_at = clock_timestamp(),
                 expires_at = clock_timestamp() + interval '7 days'
             WHERE id = $4",
        )
        .bind(&successor.token_hash)
        .bind(&successor.family_hash)
        .bind(&token_hash)
        .bind(session_id)
        .execute(&mut **transaction)
        .await
        .map_err(crate::auth::routes::database_error)?;

        return Ok(RefreshRotation::Rotated {
            user_id: candidate_user_id,
            session_id,
            refresh_token: successor,
        });
    }

    let family_session = sqlx::query_as::<_, (Uuid, bool)>(
        "SELECT id,
                COALESCE(
                    previous_refresh_token_hash = $3
                    AND refresh_token_rotated_at
                        > clock_timestamp() - ($4::bigint * interval '1 second'),
                    false
                ) AS exact_predecessor_in_grace
         FROM sessions
         WHERE refresh_token_family_hash = $1
           AND user_id = $2
           AND expires_at > clock_timestamp()
         FOR UPDATE",
    )
    .bind(&family_hash)
    .bind(candidate_user_id)
    .bind(&token_hash)
    .bind(CONCURRENT_REFRESH_GRACE_SECONDS)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(crate::auth::routes::database_error)?
    .ok_or(AuthError::InvalidToken)?;

    if family_session.1 {
        return Ok(RefreshRotation::ConcurrentRequest);
    }

    sqlx::query("DELETE FROM sessions WHERE id = $1")
        .bind(family_session.0)
        .execute(&mut **transaction)
        .await
        .map_err(crate::auth::routes::database_error)?;
    Ok(RefreshRotation::ReuseDetected)
}

/// Revoke the family represented by either its current token or any predecessor.
/// Logout remains idempotent, so an unknown or malformed token is not an error.
pub(crate) async fn delete_session_by_token(
    pool: &PgPool,
    raw_token: &str,
) -> Result<bool, AuthError> {
    let token_hash = hash_token(raw_token);
    let Some(family_secret) = family_secret(raw_token) else {
        return Ok(false);
    };
    let family_hash = hash_bytes(&family_secret);
    let result = sqlx::query(
        "DELETE FROM sessions
         WHERE refresh_token_hash = $1
            OR refresh_token_family_hash = $2",
    )
    .bind(token_hash)
    .bind(family_hash)
    .execute(pool)
    .await
    .map_err(crate::auth::routes::database_error)?;
    Ok(result.rows_affected() > 0)
}

/// Serializes the opportunistic expired-session sweep. A refresh burst must
/// not pile up concurrent table-wide DELETEs, and shutdown must be able to
/// wait for the one in flight before closing the pool.
#[derive(Clone)]
pub(crate) struct SessionCleanup {
    gate: std::sync::Arc<tokio::sync::Semaphore>,
}

impl Default for SessionCleanup {
    fn default() -> Self {
        Self {
            gate: std::sync::Arc::new(tokio::sync::Semaphore::new(1)),
        }
    }
}

impl SessionCleanup {
    /// Claims the single sweep slot, or `None` while a sweep is running.
    pub(crate) fn begin(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.gate.clone().try_acquire_owned().ok()
    }

    /// Sweeps in flight (zero or one), for drain accounting.
    pub(crate) fn pending(&self) -> usize {
        1_usize.saturating_sub(self.gate.available_permits())
    }
}

const EXPIRED_CLEANUP_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Runs at most one bounded sweep at a time; a refresh that finds a sweep in
/// flight simply skips its turn.
pub(crate) fn spawn_expired_cleanup(pool: &PgPool, cleanup: &SessionCleanup) {
    let Some(permit) = cleanup.begin() else {
        return;
    };
    let pool = pool.clone();
    tokio::spawn(async move {
        let _permit = permit;
        match tokio::time::timeout(EXPIRED_CLEANUP_TIMEOUT, cleanup_expired(&pool)).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => tracing::debug!("Expired session sweep failed; the next refresh retries"),
            Err(_) => tracing::warn!("Expired session sweep exceeded its time budget"),
        }
    });
}

/// Rows one sweep removes; a backlog drains over successive refreshes instead of
/// one table-wide DELETE that could exceed its time budget and roll back.
const EXPIRED_CLEANUP_BATCH: i64 = 1000;

pub(crate) async fn cleanup_expired(pool: &PgPool) -> Result<u64, AuthError> {
    let result = sqlx::query(
        "DELETE FROM sessions
         WHERE id IN (SELECT id FROM sessions WHERE expires_at < now() LIMIT $1)",
    )
    .bind(EXPIRED_CLEANUP_BATCH)
    .execute(pool)
    .await
    .map_err(crate::auth::routes::database_error)?;
    Ok(result.rows_affected())
}

#[cfg(test)]
#[path = "session_tests.rs"]
mod tests;
