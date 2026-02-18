#![forbid(unsafe_code)]

use crate::auth::{password, jwt, types::*};
use crate::signaling::SignalingServer;
use axum::{extract::State, Json};
use tracing::{info, warn};

/// POST /api/auth/register
pub async fn register(
    State(server): State<SignalingServer>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<AuthResponse>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    // Validate input
    if req.email.is_empty() || !req.email.contains('@') || req.email.len() > 255 {
        return Err(AuthError::InvalidCredentials);
    }
    if req.password.len() < 8 || req.password.len() > 128 {
        return Err(AuthError::InvalidCredentials);
    }
    if req.display_name.is_empty() || req.display_name.len() > 64 {
        return Err(AuthError::InvalidCredentials);
    }

    // Check if email already exists
    let exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)"
    )
    .bind(&req.email)
    .fetch_one(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    if exists {
        return Err(AuthError::EmailAlreadyExists);
    }

    // Hash password
    let hash = password::hash_password(&req.password)
        .map_err(|e| AuthError::DatabaseError(format!("Hash error: {e}")))?;

    // Insert user
    let row = sqlx::query_as::<_, (uuid::Uuid, String, String)>(
        "INSERT INTO users (email, display_name, password_hash) VALUES ($1, $2, $3) RETURNING id, email, display_name"
    )
    .bind(&req.email)
    .bind(&req.display_name)
    .bind(&hash)
    .fetch_one(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let user_id = row.0.to_string();
    let token = jwt::create_token(&user_id, &row.2, secret)?;

    info!("User registered: {} ({})", req.email, user_id);

    Ok(Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id,
            email: row.1,
            display_name: row.2,
        },
    }))
}

/// POST /api/auth/login
pub async fn login(
    State(server): State<SignalingServer>,
    Json(req): Json<LoginRequest>,
) -> Result<Json<AuthResponse>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    // Look up user by email
    let row = sqlx::query_as::<_, (uuid::Uuid, String, String, Option<String>)>(
        "SELECT id, email, display_name, password_hash FROM users WHERE email = $1"
    )
    .bind(&req.email)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?
    .ok_or(AuthError::InvalidCredentials)?;

    // Verify password
    let password_hash = row.3.as_deref().ok_or(AuthError::InvalidCredentials)?;
    let valid = password::verify_password(&req.password, password_hash)
        .map_err(|e| AuthError::DatabaseError(format!("Verify error: {e}")))?;

    if !valid {
        warn!("Failed login attempt for {}", req.email);
        return Err(AuthError::InvalidCredentials);
    }

    let user_id = row.0.to_string();
    let token = jwt::create_token(&user_id, &row.2, secret)?;

    info!("User logged in: {} ({})", req.email, user_id);

    Ok(Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id,
            email: row.1,
            display_name: row.2,
        },
    }))
}
