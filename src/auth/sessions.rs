//! Account-owned session management. Session ids are revocation handles, never
//! credentials; refresh secrets and their digests are never returned.
#![forbid(unsafe_code)]

use super::{
    account::authenticated_claims,
    routes, session,
    types::{AuthError, Claims},
};
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct AccountSession {
    pub id: Uuid,
    pub current: bool,
    pub created_at: DateTime<Utc>,
    pub refreshed_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

async fn list_sessions(pool: &PgPool, claims: &Claims) -> Result<Vec<AccountSession>, AuthError> {
    let user = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    sqlx::query_as(
        "SELECT id, id = $2 AS current, created_at,
                COALESCE(refresh_token_rotated_at, created_at) AS refreshed_at, expires_at
         FROM sessions WHERE user_id = $1 AND expires_at > clock_timestamp()
         ORDER BY (id = $2) DESC, created_at DESC, id LIMIT $3",
    )
    .bind(user)
    .bind(claims.sid)
    .bind(session::MAX_ACTIVE_SESSIONS_PER_USER)
    .fetch_all(pool)
    .await
    .map_err(routes::database_error)
}

/// Recheck authorization under the same users-then-sessions locks used by login
/// and refresh. A revoked session cannot race a later session-management write.
pub(crate) async fn lock_current_session(
    transaction: &mut Transaction<'_, Postgres>,
    claims: &Claims,
) -> Result<(Uuid, String), AuthError> {
    let user = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let version: Option<i64> =
        sqlx::query_scalar("SELECT auth_version FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user)
            .fetch_optional(&mut **transaction)
            .await
            .map_err(routes::database_error)?;
    if version != Some(claims.auth_version) {
        return Err(AuthError::InvalidToken);
    }
    let current: Option<String> = sqlx::query_scalar(
        "SELECT refresh_token_hash FROM sessions WHERE id = $1 AND user_id = $2
         AND expires_at > clock_timestamp() FOR UPDATE",
    )
    .bind(claims.sid)
    .bind(user)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(routes::database_error)?;
    Ok((user, current.ok_or(AuthError::InvalidToken)?))
}

async fn revoke_sessions(
    pool: &PgPool,
    claims: &Claims,
    target: Option<Uuid>,
    cookie: Option<&str>,
) -> Result<bool, AuthError> {
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let (user, current_hash) = lock_current_session(&mut transaction, claims).await?;
    // Another tab may have installed a newer cookie since this bearer was
    // issued. Revoke the requested session without clearing that other cookie.
    let clear_cookie = target == Some(claims.sid)
        && cookie.is_some_and(|cookie| session::hash_token(cookie) == current_hash);
    if let Some(target) = target {
        // Missing and foreign handles have the same idempotent response.
        sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id = $2")
            .bind(user)
            .bind(target)
            .execute(&mut *transaction)
            .await
            .map_err(routes::database_error)?;
    } else {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1 AND id <> $2")
            .bind(user)
            .bind(claims.sid)
            .execute(&mut *transaction)
            .await
            .map_err(routes::database_error)?;
    }
    transaction.commit().await.map_err(routes::database_error)?;
    Ok(clear_cookie)
}

/// GET /api/auth/sessions: live sessions belonging to the caller, current first.
pub async fn list(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Vec<AccountSession>>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let sessions =
        list_sessions(server.db_pool().ok_or(AuthError::NotConfigured)?, &claims).await?;
    Ok((routes::no_store_headers(), Json(sessions)))
}

/// DELETE /api/auth/sessions/{id}: revoke one of the caller's sessions. Existing
/// socket and reconnect-grace validation check the deleted row every 5 s.
pub async fn revoke(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let clear_cookie = revoke_sessions(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        Some(id),
        routes::refresh_token_from_headers(&headers),
    )
    .await?;
    let headers = if clear_cookie {
        routes::clear_refresh_cookie_headers()
    } else {
        routes::no_store_headers()
    };
    Ok((headers, StatusCode::NO_CONTENT))
}

/// DELETE /api/auth/sessions/others: keep the calling session and revoke the rest.
pub async fn revoke_others(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    revoke_sessions(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        None,
        None,
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

#[cfg(test)]
#[path = "session_management_tests.rs"]
mod tests;
