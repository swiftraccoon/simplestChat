#![forbid(unsafe_code)]

use crate::auth::{jwt, password, session, types::*};
use crate::signaling::{ClientIp, SignalingServer};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;
use webauthn_rs::prelude::*;

const REFRESH_COOKIE_NAME: &str = "__Host-refresh_token";
const MAX_EMAIL_LEN: usize = 255;
const MAX_DISPLAY_NAME_LEN: usize = 64;
const MAX_PASSWORD_LEN: usize = 128;
const GLOBAL_USER_REGISTRATION_LOCK: i64 = 7_349_872_340_911;
// A valid Argon2id hash using the same default work factors as real account
// hashes. Unknown and passwordless accounts verify against this value so the
// login path does not disclose account state through an obvious timing gap.
const DUMMY_PASSWORD_HASH: &str = "$argon2id$v=19$m=19456,t=2,p=1$Zml4ZWQtYXV0aC1zYWx0IQ$56/mxjo1tyWyHRqsMxTLQs/Zunrj6km+QS8vLNAMeFo";

#[derive(Debug, Deserialize)]
pub struct PasskeyRegisterStartRequest {
    email: String,
    display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct PasskeyLoginStartRequest {
    email: String,
}

#[derive(Debug, Deserialize)]
pub struct PasskeyRegisterFinishRequest {
    ceremony_id: String,
    credential: RegisterPublicKeyCredential,
}

#[derive(Debug, Deserialize)]
pub struct PasskeyLoginFinishRequest {
    ceremony_id: String,
    credential: PublicKeyCredential,
}

fn refresh_cookie_headers(raw_token: &str) -> HeaderMap {
    let mut headers = no_store_headers();
    let cookie = format!(
        "{REFRESH_COOKIE_NAME}={raw_token}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=604800"
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).expect("server-generated refresh cookie is valid"),
    );
    // Best-effort cleanup for old host-only cookies. A Domain-scoped legacy
    // cookie cannot be cleared here, so authentication deliberately ignores
    // the legacy name entirely (see refresh_token_from_headers).
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "refresh_token=; HttpOnly; Secure; SameSite=Strict; Path=/api/auth/refresh; Max-Age=0",
        ),
    );
    headers
}

pub(super) fn clear_refresh_cookie_headers() -> HeaderMap {
    let mut headers = no_store_headers();
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "__Host-refresh_token=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0",
        ),
    );
    headers.append(
        header::SET_COOKIE,
        HeaderValue::from_static(
            "refresh_token=; HttpOnly; Secure; SameSite=Strict; Path=/api/auth/refresh; Max-Age=0",
        ),
    );
    headers
}

pub(super) fn no_store_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers
}

fn refresh_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, REFRESH_COOKIE_NAME).filter(|token| {
        !token.is_empty()
            && token.len() <= 128
            && token
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    })
}

fn cookie_value<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .filter_map(|cookie| cookie.trim().split_once('='))
        .find_map(|(cookie_name, value)| (cookie_name == name).then_some(value))
}

fn validate_email(email: &str) -> Result<(), AuthError> {
    let valid_length = !email.is_empty() && email.len() <= MAX_EMAIL_LEN;
    let valid_characters = email.is_ascii()
        && !email
            .chars()
            .any(|character| character.is_control() || character.is_whitespace());
    let valid_shape = email.split_once('@').is_some_and(|(local, domain)| {
        !local.is_empty() && !domain.is_empty() && !domain.contains('@')
    });

    if valid_length && valid_characters && valid_shape {
        Ok(())
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

pub(super) fn canonicalize_email(email: &str) -> Result<String, AuthError> {
    let canonical = email.trim().to_ascii_lowercase();
    validate_email(&canonical)?;
    Ok(canonical)
}

pub(super) fn validate_display_name(display_name: &str) -> Result<(), AuthError> {
    if !display_name.trim().is_empty()
        && display_name.len() <= MAX_DISPLAY_NAME_LEN
        && !display_name.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

fn validate_ceremony_id(ceremony_id: &str) -> Result<(), AuthError> {
    Uuid::parse_str(ceremony_id)
        .map(|_| ())
        .map_err(|_| AuthError::WebAuthnError("Invalid ceremony".into()))
}

fn add_ceremony_id(mut response: Value, ceremony_id: String) -> Result<Value, AuthError> {
    response
        .as_object_mut()
        .ok_or_else(|| AuthError::WebAuthnError("Invalid challenge response".into()))?
        .insert("ceremony_id".into(), Value::String(ceremony_id));
    Ok(response)
}

fn credential_id(passkey: &Passkey) -> String {
    URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref())
}

fn authentication_credential_id(result: &AuthenticationResult) -> String {
    URL_SAFE_NO_PAD.encode(result.cred_id().as_ref())
}

pub(super) fn database_error(error: sqlx::Error) -> AuthError {
    AuthError::DatabaseError(error.to_string())
}

fn user_insert_error(error: sqlx::Error) -> AuthError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return AuthError::EmailAlreadyExists;
    }
    database_error(error)
}

fn credential_insert_error(error: sqlx::Error) -> AuthError {
    if let sqlx::Error::Database(database_error) = &error
        && database_error.code().as_deref() == Some("23505")
    {
        return AuthError::WebAuthnError("Credential already registered".into());
    }
    database_error(error)
}

async fn enforce_user_capacity(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    max_users: i64,
) -> Result<(), AuthError> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(GLOBAL_USER_REGISTRATION_LOCK)
        .execute(&mut **transaction)
        .await
        .map_err(database_error)?;
    let user_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users")
        .fetch_one(&mut **transaction)
        .await
        .map_err(database_error)?;
    if user_count >= max_users {
        return Err(AuthError::RegistrationDisabled);
    }
    Ok(())
}

pub(super) async fn hash_password_async(
    password_value: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<String, AuthError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        password::hash_password(&password_value)
    })
    .await
    .map_err(|error| AuthError::DatabaseError(format!("Password worker failed: {error}")))?
    .map_err(|error| AuthError::DatabaseError(format!("Hash error: {error}")))
}

pub(super) async fn verify_password_async(
    password_value: String,
    password_hash: String,
    permit: tokio::sync::OwnedSemaphorePermit,
) -> Result<bool, AuthError> {
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        password::verify_password(&password_value, &password_hash)
    })
    .await
    .map_err(|error| AuthError::DatabaseError(format!("Password worker failed: {error}")))?
    .map_err(|error| AuthError::DatabaseError(format!("Verify error: {error}")))
}

pub(super) fn acquire_auth_request(
    server: &SignalingServer,
) -> Result<tokio::sync::OwnedSemaphorePermit, AuthError> {
    server
        .try_acquire_auth_request()
        .ok_or(AuthError::ServiceBusy)
}

/// POST /api/auth/register
pub async fn register(
    State(server): State<SignalingServer>,
    Json(req): Json<RegisterRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    if !server.registration_enabled() {
        return Err(AuthError::RegistrationDisabled);
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }

    let email = canonicalize_email(&req.email)?;
    validate_display_name(&req.display_name)?;
    if req.password.len() < 8 || req.password.len() > MAX_PASSWORD_LEN {
        return Err(AuthError::InvalidCredentials);
    }
    let _request_permit = acquire_auth_request(&server)?;

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)")
        .bind(&email)
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
    if exists {
        return Err(AuthError::EmailAlreadyExists);
    }

    let password_permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::RateLimited)?;
    let password_hash = hash_password_async(req.password, password_permit).await?;
    let refresh_token = session::generate_refresh_token()?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    enforce_user_capacity(&mut transaction, server.max_users()).await?;

    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "INSERT INTO users (email, display_name, password_hash) VALUES ($1, $2, $3) RETURNING id, email, display_name",
    )
    .bind(&email)
    .bind(&req.display_name)
    .bind(&password_hash)
    .fetch_one(&mut *transaction)
    .await
    .map_err(user_insert_error)?;

    let user_id = row.0.to_string();
    let token = jwt::create_token(&user_id, &row.2, secret)?;
    session::create_session_with(&mut transaction, &row.0, &refresh_token).await?;
    transaction.commit().await.map_err(database_error)?;

    info!(user_id, "User registered");
    Ok((
        refresh_cookie_headers(&refresh_token.raw),
        Json(AuthResponse {
            token,
            user: UserInfo {
                id: user_id,
                email: row.1,
                display_name: row.2,
            },
        }),
    ))
}

/// POST /api/auth/login
pub async fn login(
    State(server): State<SignalingServer>,
    Json(req): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let email = canonicalize_email(&req.email)?;
    if !server.allow_auth_principal(&email) {
        return Err(AuthError::RateLimited);
    }
    if req.password.len() > MAX_PASSWORD_LEN {
        return Err(AuthError::InvalidCredentials);
    }
    let _request_permit = acquire_auth_request(&server)?;

    let row = sqlx::query_as::<_, (Uuid, String, String, Option<String>, i64)>(
        "SELECT id, email, display_name, password_hash, auth_version FROM users WHERE email = $1",
    )
    .bind(&email)
    .fetch_optional(pool)
    .await
    .map_err(database_error)?;

    let has_password = row.as_ref().and_then(|record| record.3.as_ref()).is_some();
    let password_hash = row
        .as_ref()
        .and_then(|record| record.3.as_deref())
        .unwrap_or(DUMMY_PASSWORD_HASH)
        .to_owned();
    let password_permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::RateLimited)?;
    let password_matches =
        match verify_password_async(req.password, password_hash, password_permit).await {
            Ok(matches) => matches,
            Err(error) => {
                warn!(?error, "Stored password hash could not be verified");
                false
            }
        };
    if !has_password || !password_matches {
        warn!("Failed login attempt");
        return Err(AuthError::InvalidCredentials);
    }
    let row = row.ok_or(AuthError::InvalidCredentials)?;

    let user_id = row.0.to_string();
    let refresh_token = session::generate_refresh_token()?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    let current = sqlx::query_as::<_, (Option<String>, i64)>(
        "SELECT password_hash, auth_version FROM users WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(row.0)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?;
    if current != Some((row.3.clone(), row.4)) {
        return Err(AuthError::InvalidCredentials);
    }
    let token = jwt::create_token_with_version(&user_id, &row.2, secret, row.4)?;
    session::create_session_with(&mut transaction, &row.0, &refresh_token).await?;
    transaction.commit().await.map_err(database_error)?;

    info!(user_id, "User logged in");
    Ok((
        refresh_cookie_headers(&refresh_token.raw),
        Json(AuthResponse {
            token,
            user: UserInfo {
                id: user_id,
                email: row.1,
                display_name: row.2,
            },
        }),
    ))
}

/// POST /api/auth/refresh
pub async fn refresh(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let raw_token = refresh_token_from_headers(&headers).ok_or(AuthError::MissingToken)?;
    let _request_permit = acquire_auth_request(&server)?;
    let mut transaction = pool.begin().await.map_err(database_error)?;

    let (user_id, refresh_token) =
        match session::rotate_refresh_token_with(&mut transaction, raw_token).await? {
            session::RefreshRotation::Rotated {
                user_id,
                refresh_token,
            } => (user_id, refresh_token),
            session::RefreshRotation::ConcurrentRequest => {
                // The winning response will install the successor cookie. Do
                // not authenticate this consumed bearer, but also do not let a
                // near-simultaneous browser request revoke that successor.
                transaction.commit().await.map_err(database_error)?;
                return Err(AuthError::InvalidToken);
            }
            session::RefreshRotation::ReuseDetected => {
                // Reuse revocation is a durable security action. Commit it
                // before returning the same non-oracular error as any invalid
                // token.
                transaction.commit().await.map_err(database_error)?;
                warn!("Refresh-token reuse detected; revoked session family");
                return Err(AuthError::InvalidToken);
            }
        };
    let row = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT email, display_name, auth_version FROM users WHERE id = $1",
    )
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(AuthError::UserNotFound)?;

    let user_id_string = user_id.to_string();
    let token = jwt::create_token_with_version(&user_id_string, &row.1, secret, row.2)?;
    transaction.commit().await.map_err(database_error)?;
    session::spawn_expired_cleanup(pool);

    Ok((
        refresh_cookie_headers(&refresh_token.raw),
        Json(AuthResponse {
            token,
            user: UserInfo {
                id: user_id_string,
                email: row.0,
                display_name: row.1,
            },
        }),
    ))
}

/// POST /api/auth/logout
pub async fn logout(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> (HeaderMap, StatusCode) {
    let response_headers = clear_refresh_cookie_headers();
    let Some(raw_token) = refresh_token_from_headers(&headers) else {
        return (response_headers, StatusCode::NO_CONTENT);
    };
    let Some(pool) = server.db_pool() else {
        return (response_headers, StatusCode::NO_CONTENT);
    };
    let Some(_request_permit) = server.try_acquire_auth_request() else {
        let mut headers = no_store_headers();
        headers.insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        return (headers, StatusCode::SERVICE_UNAVAILABLE);
    };

    match session::delete_session_by_token(pool, raw_token).await {
        Ok(_) => (response_headers, StatusCode::NO_CONTENT),
        Err(error) => {
            warn!(?error, "Failed to revoke refresh session during logout");
            // Retain the HttpOnly cookie so the caller can retry revocation;
            // clearing it here would discard the only handle to a still-live
            // stolen server-side session.
            (no_store_headers(), StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// POST /api/auth/passkey/register/start
pub(crate) async fn passkey_register_start(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(req): Json<PasskeyRegisterStartRequest>,
) -> Result<Json<Value>, AuthError> {
    if !server.registration_enabled() {
        return Err(AuthError::RegistrationDisabled);
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let email = canonicalize_email(&req.email)?;
    validate_display_name(&req.display_name)?;
    let _request_permit = acquire_auth_request(&server)?;

    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)")
        .bind(&email)
        .fetch_one(pool)
        .await
        .map_err(database_error)?;
    if exists {
        return Err(AuthError::EmailAlreadyExists);
    }

    let user_id = Uuid::new_v4();
    let (challenge, state) = webauthn
        .start_passkey_registration(user_id, &email, &req.display_name, None)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let ceremony_id = store
        .store_registration(state, user_id, email, req.display_name.clone(), source_ip)
        .ok_or_else(|| {
            AuthError::WebAuthnError("Too many pending registrations, try again later".into())
        })?;
    let response = serde_json::to_value(challenge)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    Ok(Json(add_ceremony_id(response, ceremony_id)?))
}

/// POST /api/auth/passkey/register/finish
pub async fn passkey_register_finish(
    State(server): State<SignalingServer>,
    Json(body): Json<PasskeyRegisterFinishRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    if !server.registration_enabled() {
        return Err(AuthError::RegistrationDisabled);
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    validate_ceremony_id(&body.ceremony_id)?;
    let _request_permit = acquire_auth_request(&server)?;

    let registration = store.take_registration(&body.ceremony_id).ok_or_else(|| {
        AuthError::WebAuthnError("No pending registration or challenge expired".into())
    })?;
    let passkey = webauthn
        .finish_passkey_registration(&body.credential, &registration.state)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let passkey_id = credential_id(&passkey);
    let credential_json = serde_json::to_value(&passkey)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let refresh_token = session::generate_refresh_token()?;
    let mut transaction = pool.begin().await.map_err(database_error)?;
    enforce_user_capacity(&mut transaction, server.max_users()).await?;

    sqlx::query("INSERT INTO users (id, email, display_name) VALUES ($1, $2, $3)")
        .bind(registration.user_id)
        .bind(&registration.email)
        .bind(&registration.display_name)
        .execute(&mut *transaction)
        .await
        .map_err(user_insert_error)?;
    sqlx::query(
        "INSERT INTO webauthn_credentials (user_id, credential_id, credential_json) VALUES ($1, $2, $3)",
    )
    .bind(registration.user_id)
    .bind(&passkey_id)
    .bind(&credential_json)
    .execute(&mut *transaction)
    .await
    .map_err(credential_insert_error)?;
    session::create_session_with(&mut transaction, &registration.user_id, &refresh_token).await?;

    let user_id = registration.user_id.to_string();
    let token = jwt::create_token(&user_id, &registration.display_name, secret)?;
    transaction.commit().await.map_err(database_error)?;
    info!(user_id, "User registered via passkey");

    Ok((
        refresh_cookie_headers(&refresh_token.raw),
        Json(AuthResponse {
            token,
            user: UserInfo {
                id: user_id,
                email: registration.email,
                display_name: registration.display_name,
            },
        }),
    ))
}

/// POST /api/auth/passkey/login/start
pub(crate) async fn passkey_login_start(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(body): Json<PasskeyLoginStartRequest>,
) -> Result<Json<Value>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let email = canonicalize_email(&body.email)?;
    if !server.allow_auth_principal(&email) {
        return Err(AuthError::RateLimited);
    }
    let _request_permit = acquire_auth_request(&server)?;

    let user_id: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE email = $1")
        .bind(&email)
        .fetch_optional(pool)
        .await
        .map_err(database_error)?
        .ok_or(AuthError::InvalidCredentials)?;
    let credential_rows: Vec<Value> =
        sqlx::query_scalar("SELECT credential_json FROM webauthn_credentials WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await
            .map_err(database_error)?;
    if credential_rows.is_empty() {
        return Err(AuthError::InvalidCredentials);
    }
    let passkeys = credential_rows
        .into_iter()
        .map(|value| {
            serde_json::from_value(value)
                .map_err(|error| AuthError::DatabaseError(format!("Invalid credential: {error}")))
        })
        .collect::<Result<Vec<Passkey>, AuthError>>()?;

    let (challenge, state) = webauthn
        .start_passkey_authentication(&passkeys)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let ceremony_id = store
        .store_authentication(state, user_id, source_ip)
        .ok_or_else(|| {
            AuthError::WebAuthnError("Too many pending authentications, try again later".into())
        })?;
    let response = serde_json::to_value(challenge)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    Ok(Json(add_ceremony_id(response, ceremony_id)?))
}

/// POST /api/auth/passkey/login/finish
pub async fn passkey_login_finish(
    State(server): State<SignalingServer>,
    Json(body): Json<PasskeyLoginFinishRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    validate_ceremony_id(&body.ceremony_id)?;
    let _request_permit = acquire_auth_request(&server)?;

    let authentication = store
        .take_authentication(&body.ceremony_id)
        .ok_or_else(|| {
            AuthError::WebAuthnError("No pending authentication or challenge expired".into())
        })?;
    let result = webauthn
        .finish_passkey_authentication(&body.credential, &authentication.state)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let passkey_id = authentication_credential_id(&result);
    let refresh_token = session::generate_refresh_token()?;
    let mut transaction = pool.begin().await.map_err(database_error)?;

    let user = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT email, display_name, auth_version FROM users WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(authentication.user_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(AuthError::UserNotFound)?;
    let credential_json: Value = sqlx::query_scalar(
        "SELECT credential_json FROM webauthn_credentials WHERE user_id = $1 AND credential_id = $2 FOR UPDATE",
    )
    .bind(authentication.user_id)
    .bind(&passkey_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or_else(|| AuthError::WebAuthnError("Credential not registered to user".into()))?;

    // The WebAuthn state contains the counter observed at ceremony start. Check
    // the locked, current row too so concurrent ceremonies cannot accept a
    // stale or rolled-back counter.
    let stored_counter = credential_json
        .pointer("/cred/counter")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let presented_counter = u64::from(result.counter());
    if (stored_counter > 0 || presented_counter > 0) && presented_counter <= stored_counter {
        return Err(AuthError::WebAuthnError(
            "Credential counter indicates possible cloning".into(),
        ));
    }

    let mut passkey: Passkey = serde_json::from_value(credential_json)
        .map_err(|error| AuthError::DatabaseError(format!("Invalid credential: {error}")))?;
    passkey
        .update_credential(&result)
        .ok_or_else(|| AuthError::WebAuthnError("Credential mismatch".into()))?;
    let updated_credential = serde_json::to_value(passkey)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    sqlx::query(
        "UPDATE webauthn_credentials SET credential_json = $3 WHERE user_id = $1 AND credential_id = $2",
    )
    .bind(authentication.user_id)
    .bind(&passkey_id)
    .bind(updated_credential)
    .execute(&mut *transaction)
    .await
    .map_err(database_error)?;
    session::create_session_with(&mut transaction, &authentication.user_id, &refresh_token).await?;

    let user_id = authentication.user_id.to_string();
    let token = jwt::create_token_with_version(&user_id, &user.1, secret, user.2)?;
    transaction.commit().await.map_err(database_error)?;
    info!(user_id, "User logged in via passkey");

    Ok((
        refresh_cookie_headers(&refresh_token.raw),
        Json(AuthResponse {
            token,
            user: UserInfo {
                id: user_id,
                email: user.0,
                display_name: user.1,
            },
        }),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_bounded_email_addresses() {
        assert!(validate_email("alice@example.com").is_ok());
        assert!(validate_email("aliceexample.com").is_err());
        assert!(validate_email("alice@@example.com").is_err());
        assert!(validate_email(" alice@example.com").is_err());
        assert!(validate_email("alice@exam\nple.com").is_err());
        let oversized = format!("{}@example.com", "a".repeat(MAX_EMAIL_LEN));
        assert!(validate_email(&oversized).is_err());
    }

    #[test]
    fn canonicalizes_email_addresses_before_identity_lookup() {
        assert_eq!(
            canonicalize_email("  Alice@Example.COM  ").unwrap(),
            "alice@example.com"
        );
        assert!(canonicalize_email("álîce@example.com").is_err());
    }

    #[test]
    fn dummy_password_hash_is_valid_and_never_matches_an_arbitrary_password() {
        assert_eq!(
            password::verify_password("not-the-dummy-password", DUMMY_PASSWORD_HASH).unwrap(),
            false
        );
    }

    #[test]
    fn prefers_host_prefixed_refresh_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static(
                "refresh_token=legacy-token; __Host-refresh_token=current-token",
            ),
        );
        assert_eq!(refresh_token_from_headers(&headers), Some("current-token"));
    }

    #[test]
    fn rejects_legacy_refresh_cookie_even_when_it_is_the_only_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("refresh_token=attacker-controlled-token"),
        );
        assert_eq!(refresh_token_from_headers(&headers), None);
    }

    #[test]
    fn accepts_the_versioned_opaque_refresh_token_format() {
        let token = session::generate_refresh_token().unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_str(&format!("__Host-refresh_token={}", token.raw)).unwrap(),
        );
        assert_eq!(
            refresh_token_from_headers(&headers),
            Some(token.raw.as_str())
        );
    }

    #[test]
    fn rejects_delimiter_characters_not_accepted_by_previous_servers() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("__Host-refresh_token=v1.invalid.invalid"),
        );
        assert_eq!(refresh_token_from_headers(&headers), None);
    }

    #[test]
    fn refresh_response_sets_host_cookie_and_expires_legacy_cookie() {
        let headers = refresh_cookie_headers("safe-token");
        let cookies = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("valid cookie"))
            .collect::<Vec<_>>();
        assert!(cookies.iter().any(|cookie| {
            cookie.starts_with("__Host-refresh_token=safe-token;") && cookie.contains("Path=/;")
        }));
        assert!(cookies.iter().any(|cookie| {
            cookie.starts_with("refresh_token=;") && cookie.contains("Max-Age=0")
        }));
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store, max-age=0")
        );
    }
}
