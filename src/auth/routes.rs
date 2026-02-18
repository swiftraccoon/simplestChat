#![forbid(unsafe_code)]

use crate::auth::{password, jwt, session, types::*};
use crate::signaling::SignalingServer;
use axum::{extract::State, http::{header, HeaderMap}, Json};
use serde_json::Value;
use webauthn_rs::prelude::*;
use tracing::{info, warn};

fn refresh_cookie_headers(raw_token: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let cookie = format!(
        "refresh_token={}; HttpOnly; Secure; SameSite=Strict; Path=/api/auth/refresh; Max-Age=604800",
        raw_token
    );
    headers.insert(header::SET_COOKIE, cookie.parse().unwrap());
    headers
}

/// POST /api/auth/register
pub async fn register(
    State(server): State<SignalingServer>,
    Json(req): Json<RegisterRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
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

    let (raw_refresh, hash_refresh) = session::generate_refresh_token();
    session::create_session(pool, &row.0, &hash_refresh).await?;
    let headers = refresh_cookie_headers(&raw_refresh);

    info!("User registered: {} ({})", req.email, user_id);

    Ok((headers, Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id,
            email: row.1,
            display_name: row.2,
        },
    })))
}

/// POST /api/auth/login
pub async fn login(
    State(server): State<SignalingServer>,
    Json(req): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
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

    let (raw_refresh, hash_refresh) = session::generate_refresh_token();
    session::create_session(pool, &row.0, &hash_refresh).await?;
    let headers = refresh_cookie_headers(&raw_refresh);

    info!("User logged in: {} ({})", req.email, user_id);

    Ok((headers, Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id,
            email: row.1,
            display_name: row.2,
        },
    })))
}

/// POST /api/auth/refresh
pub async fn refresh(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    let cookie_header = headers.get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let raw_token = cookie_header
        .split(';')
        .find_map(|c| {
            let c = c.trim();
            c.strip_prefix("refresh_token=")
        })
        .ok_or(AuthError::MissingToken)?;

    let user_id = session::consume_refresh_token(pool, raw_token).await?;

    let row = sqlx::query_as::<_, (String, String)>(
        "SELECT email, display_name FROM users WHERE id = $1"
    )
    .bind(&user_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?
    .ok_or(AuthError::UserNotFound)?;

    let user_id_str = user_id.to_string();
    let token = jwt::create_token(&user_id_str, &row.1, secret)?;

    let (raw_refresh, hash_refresh) = session::generate_refresh_token();
    session::create_session(pool, &user_id, &hash_refresh).await?;

    let resp_headers = refresh_cookie_headers(&raw_refresh);

    Ok((resp_headers, Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id_str,
            email: row.0,
            display_name: row.1,
        },
    })))
}

/// POST /api/auth/passkey/register/start
pub async fn passkey_register_start(
    State(server): State<SignalingServer>,
    Json(req): Json<RegisterRequest>,
) -> Result<Json<Value>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;

    if req.email.is_empty() || !req.email.contains('@') {
        return Err(AuthError::InvalidCredentials);
    }
    if req.display_name.is_empty() || req.display_name.len() > 64 {
        return Err(AuthError::InvalidCredentials);
    }

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)")
        .bind(&req.email)
        .fetch_one(pool)
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    if exists {
        return Err(AuthError::EmailAlreadyExists);
    }

    let user_id = Uuid::new_v4();

    let (ccr, reg_state) = webauthn
        .start_passkey_registration(user_id, &req.email, &req.display_name, None)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    store.store_registration(&req.email, reg_state);

    let mut response = serde_json::to_value(&ccr)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    response["_user_id"] = serde_json::Value::String(user_id.to_string());
    response["_email"] = serde_json::Value::String(req.email);
    response["_display_name"] = serde_json::Value::String(req.display_name);

    Ok(Json(response))
}

/// POST /api/auth/passkey/register/finish
pub async fn passkey_register_finish(
    State(server): State<SignalingServer>,
    Json(body): Json<Value>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    let email = body["email"].as_str().ok_or(AuthError::WebAuthnError("Missing email".into()))?;
    let credential: RegisterPublicKeyCredential = serde_json::from_value(body["credential"].clone())
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    let reg_state = store.take_registration(email)
        .ok_or(AuthError::WebAuthnError("No pending registration or challenge expired".into()))?;

    let passkey = webauthn
        .finish_passkey_registration(&credential, &reg_state)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    let user_id = body["user_id"].as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4);

    let display_name = body["display_name"].as_str().unwrap_or(email);

    sqlx::query(
        "INSERT INTO users (id, email, display_name) VALUES ($1, $2, $3)"
    )
    .bind(&user_id)
    .bind(email)
    .bind(display_name)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let credential_json = serde_json::to_value(&passkey)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    sqlx::query(
        "INSERT INTO webauthn_credentials (user_id, credential_json) VALUES ($1, $2)"
    )
    .bind(&user_id)
    .bind(&credential_json)
    .execute(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let user_id_str = user_id.to_string();
    let token = jwt::create_token(&user_id_str, display_name, secret)?;

    let (raw_refresh, hash_refresh) = session::generate_refresh_token();
    session::create_session(pool, &user_id, &hash_refresh).await?;

    info!("User registered via passkey: {} ({})", email, user_id_str);

    Ok((refresh_cookie_headers(&raw_refresh), Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id_str,
            email: email.to_string(),
            display_name: display_name.to_string(),
        },
    })))
}

/// POST /api/auth/passkey/login/start
pub async fn passkey_login_start(
    State(server): State<SignalingServer>,
    Json(body): Json<Value>,
) -> Result<Json<Value>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;

    let email = body["email"].as_str().ok_or(AuthError::InvalidCredentials)?;

    let user_row = sqlx::query_as::<_, (uuid::Uuid,)>(
        "SELECT id FROM users WHERE email = $1"
    )
    .bind(email)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?
    .ok_or(AuthError::UserNotFound)?;

    let cred_rows = sqlx::query_as::<_, (serde_json::Value,)>(
        "SELECT credential_json FROM webauthn_credentials WHERE user_id = $1"
    )
    .bind(&user_row.0)
    .fetch_all(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    if cred_rows.is_empty() {
        return Err(AuthError::WebAuthnError("No passkeys registered".into()));
    }

    let passkeys: Vec<Passkey> = cred_rows
        .iter()
        .filter_map(|r| serde_json::from_value(r.0.clone()).ok())
        .collect();

    let (rcr, auth_state) = webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    store.store_authentication(email, auth_state);

    let response = serde_json::to_value(&rcr)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    Ok(Json(response))
}

/// POST /api/auth/passkey/login/finish
pub async fn passkey_login_finish(
    State(server): State<SignalingServer>,
    Json(body): Json<Value>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    let email = body["email"].as_str().ok_or(AuthError::InvalidCredentials)?;
    let credential: PublicKeyCredential = serde_json::from_value(body["credential"].clone())
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    let auth_state = store.take_authentication(email)
        .ok_or(AuthError::WebAuthnError("No pending authentication or challenge expired".into()))?;

    let _auth_result = webauthn
        .finish_passkey_authentication(&credential, &auth_state)
        .map_err(|e| AuthError::WebAuthnError(e.to_string()))?;

    let row = sqlx::query_as::<_, (uuid::Uuid, String, String)>(
        "SELECT id, email, display_name FROM users WHERE email = $1"
    )
    .bind(email)
    .fetch_one(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let user_id_str = row.0.to_string();
    let token = jwt::create_token(&user_id_str, &row.2, secret)?;

    let (raw_refresh, hash_refresh) = session::generate_refresh_token();
    session::create_session(pool, &row.0, &hash_refresh).await?;

    info!("User logged in via passkey: {} ({})", email, user_id_str);

    Ok((refresh_cookie_headers(&raw_refresh), Json(AuthResponse {
        token,
        user: UserInfo {
            id: user_id_str,
            email: row.1,
            display_name: row.2,
        },
    })))
}
