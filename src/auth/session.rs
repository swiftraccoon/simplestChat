#![forbid(unsafe_code)]

use crate::auth::types::AuthError;
use sha2::{Sha256, Digest};
use sqlx::PgPool;
use uuid::Uuid;

pub fn generate_refresh_token() -> (String, String) {
    let raw = Uuid::new_v4().to_string();
    let hash = hex::encode(Sha256::digest(raw.as_bytes()));
    (raw, hash)
}

pub fn hash_token(raw: &str) -> String {
    hex::encode(Sha256::digest(raw.as_bytes()))
}

pub async fn create_session(
    pool: &PgPool,
    user_id: &Uuid,
    token_hash: &str,
) -> Result<(), AuthError> {
    sqlx::query(
        "INSERT INTO sessions (user_id, refresh_token_hash, expires_at) VALUES ($1, $2, now() + interval '7 days')"
    )
    .bind(user_id)
    .bind(token_hash)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(())
}

pub async fn consume_refresh_token(
    pool: &PgPool,
    raw_token: &str,
) -> Result<Uuid, AuthError> {
    let token_hash = hash_token(raw_token);

    let row = sqlx::query_as::<_, (Uuid, Uuid)>(
        "DELETE FROM sessions WHERE refresh_token_hash = $1 AND expires_at > now() RETURNING id, user_id"
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?
    .ok_or(AuthError::InvalidToken)?;

    Ok(row.1)
}

pub async fn delete_user_sessions(pool: &PgPool, user_id: &Uuid) -> Result<(), AuthError> {
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(user_id)
        .execute(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(())
}

pub async fn cleanup_expired(pool: &PgPool) -> Result<u64, AuthError> {
    let result = sqlx::query("DELETE FROM sessions WHERE expires_at < now()")
        .execute(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
    Ok(result.rows_affected())
}
