#![forbid(unsafe_code)]
//! Registration invitations: any account holds a few single-use codes that let
//! someone register while registration is closed.

use super::{account::authenticated_claims, routes, types::AuthError};
use crate::invite_codes;
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

/// Live (unused, unexpired) registration codes one account may hold.
pub const MAX_LIVE_REGISTRATION_INVITES: i64 = 5;
const REGISTRATION_INVITE_DAYS: i32 = 7;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RegistrationInvite {
    pub code: String,
    pub uses_left: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

async fn live_invites(pool: &PgPool, user: Uuid) -> Result<Vec<RegistrationInvite>, sqlx::Error> {
    sqlx::query_as(
        "SELECT code, uses_left, expires_at, created_at FROM invites
         WHERE kind = 'registration' AND created_by = $1 AND uses_left > 0 AND expires_at > now()
         ORDER BY created_at DESC, code",
    )
    .bind(user)
    .fetch_all(pool)
    .await
}

/// Spend a registration code inside the registering transaction; the inviter
/// when it was live, `None` when it was unknown, spent or expired.
pub(crate) async fn consume_registration_invite(
    transaction: &mut PgConnection,
    code: &str,
) -> Result<Option<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE invites SET uses_left = uses_left - 1
         WHERE code = $1 AND kind = 'registration' AND uses_left > 0 AND expires_at > now()
         RETURNING created_by",
    )
    .bind(code)
    .fetch_optional(&mut *transaction)
    .await
}

async fn caller(
    server: &SignalingServer,
    headers: &HeaderMap,
) -> Result<(Uuid, PgPool), AuthError> {
    let claims = authenticated_claims(server, headers).await?;
    let user = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?.clone();
    Ok((user, pool))
}

/// GET /api/auth/invites — the account's live registration codes.
pub async fn list(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Vec<RegistrationInvite>>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let invites = live_invites(&pool, user)
        .await
        .map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), Json(invites)))
}

/// POST /api/auth/invites — mint a single-use code, a week long.
pub async fn create(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<RegistrationInvite>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let code =
        invite_codes::generate().map_err(|error| AuthError::DatabaseError(error.to_string()))?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::text, 71341))")
        .bind(user)
        .execute(&mut *transaction)
        .await
        .map_err(routes::database_error)?;
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM invites
         WHERE kind = 'registration' AND created_by = $1 AND uses_left > 0 AND expires_at > now()",
    )
    .bind(user)
    .fetch_one(&mut *transaction)
    .await
    .map_err(routes::database_error)?;
    if live >= MAX_LIVE_REGISTRATION_INVITES {
        return Err(AuthError::InvalidInput(
            "You already hold five unused invite codes",
        ));
    }
    let invite: RegistrationInvite = sqlx::query_as(
        "INSERT INTO invites (code, kind, created_by, uses_left, expires_at)
         VALUES ($1, 'registration', $2, 1, now() + make_interval(days => $3))
         RETURNING code, uses_left, expires_at, created_at",
    )
    .bind(&code)
    .bind(user)
    .bind(REGISTRATION_INVITE_DAYS)
    .fetch_one(&mut *transaction)
    .await
    .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((routes::no_store_headers(), Json(invite)))
}

/// DELETE /api/auth/invites/:code — take back one of the account's own codes.
pub async fn revoke(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<StatusCode, AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let code = invite_codes::normalize(&code).ok_or(AuthError::InviteNotFound)?;
    let removed = sqlx::query(
        "DELETE FROM invites WHERE code = $1 AND kind = 'registration' AND created_by = $2",
    )
    .bind(&code)
    .bind(user)
    .execute(&pool)
    .await
    .map_err(routes::database_error)?
    .rows_affected();
    if removed == 0 {
        return Err(AuthError::InviteNotFound);
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_registration_invites_open_the_door_once_each_and_expire() {
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let inviter = Uuid::new_v4();
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Inviter')")
            .bind(inviter)
            .bind(format!("{inviter}@invites.invalid"))
            .execute(&pool)
            .await
            .unwrap();
        let live = invite_codes::generate().unwrap();
        let stale = invite_codes::generate().unwrap();
        for (code, days) in [(&live, 7), (&stale, -1)] {
            sqlx::query(
                "INSERT INTO invites (code, kind, created_by, uses_left, expires_at)
                 VALUES ($1, 'registration', $2, 1, now() + make_interval(days => $3))",
            )
            .bind(code)
            .bind(inviter)
            .bind(days)
            .execute(&pool)
            .await
            .unwrap();
        }
        assert_eq!(
            live_invites(&pool, inviter).await.unwrap().len(),
            1,
            "an expired code is not live"
        );

        let mut transaction = pool.begin().await.unwrap();
        assert_eq!(
            consume_registration_invite(&mut transaction, &live)
                .await
                .unwrap(),
            Some(inviter)
        );
        assert_eq!(
            consume_registration_invite(&mut transaction, &live)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            consume_registration_invite(&mut transaction, &stale)
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            consume_registration_invite(&mut transaction, "unknown")
                .await
                .unwrap(),
            None
        );
        transaction.commit().await.unwrap();
        assert!(live_invites(&pool, inviter).await.unwrap().is_empty());

        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(inviter)
            .execute(&pool)
            .await
            .unwrap();
    }
}
