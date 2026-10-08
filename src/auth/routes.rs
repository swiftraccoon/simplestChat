#![forbid(unsafe_code)]

use crate::auth::{jwt, password, session, types::*};
use crate::signaling::{ClientIp, SignalingServer};
use axum::{
    Extension, Json,
    extract::State,
    http::{HeaderMap, HeaderValue, StatusCode, header},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tracing::{info, warn};
use uuid::Uuid;
use webauthn_rs::prelude::*;

const REFRESH_COOKIE_NAME: &str = "__Host-refresh_token";
const MAX_EMAIL_LEN: usize = 255;
const MAX_DISPLAY_NAME_LEN: usize = 64;
const MAX_PASSWORD_CHARS: usize = 128;
/// Bounds the hashing input; 128 characters of any script fit within it.
pub(super) const MAX_PASSWORD_BYTES: usize = 512;

/// 15–128 NFC-normalized characters, counted as characters so scripts with multi-byte letters
/// are neither admitted short nor refused long.
pub(super) fn password_length_ok(password: &str) -> bool {
    let normalized = password::normalize_selection(password);
    let characters = normalized.chars().count();
    (15..=MAX_PASSWORD_CHARS).contains(&characters) && password.len() <= MAX_PASSWORD_BYTES
}
/// Apply the same selection policy at signup, change and recovery. Login does
/// not reapply selection rules, so existing credentials remain usable.
pub(super) fn validate_new_password(value: &str) -> Result<(), AuthError> {
    if !password_length_ok(value) || value.chars().any(char::is_control) {
        return Err(AuthError::InvalidInput(
            "Password must be 15–128 characters without control characters",
        ));
    }
    if super::common_passwords::is_common(&password::normalize_selection(value)) {
        return Err(AuthError::InvalidInput("Choose a less common password"));
    }
    Ok(())
}

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
#[serde(deny_unknown_fields)]
pub struct PasskeyLoginStartRequest {}

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
    headers
}

pub(crate) fn no_store_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers
}

pub(super) fn refresh_token_from_headers(headers: &HeaderMap) -> Option<&str> {
    cookie_value(headers, REFRESH_COOKIE_NAME)
        .filter(|token| session::refresh_token_is_valid(token))
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
        && crate::labels::is_plain(display_name)
        && !crate::labels::is_reserved_name(display_name)
    {
        Ok(())
    } else {
        Err(AuthError::InvalidCredentials)
    }
}

pub(super) fn validate_ceremony_id(ceremony_id: &str) -> Result<(), AuthError> {
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

pub(super) fn credential_id(passkey: &Passkey) -> String {
    URL_SAFE_NO_PAD.encode(passkey.cred_id().as_ref())
}

fn authentication_credential_id(result: &AuthenticationResult) -> String {
    URL_SAFE_NO_PAD.encode(result.cred_id().as_ref())
}

pub(crate) fn database_error(error: sqlx::Error) -> AuthError {
    crate::db::record_error(&error);
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

pub(super) fn credential_insert_error(error: sqlx::Error) -> AuthError {
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
    // The lock is transaction-scoped, so the probe it protects must stay
    // cheap: count at most `max_users` rows rather than the whole table.
    let user_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM (SELECT 1 FROM users LIMIT $1) AS capacity")
            .bind(max_users)
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
        password::hash_account_password(&password_value)
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
        password::verify_account_password(&password_value, &password_hash)
    })
    .await
    .map_err(|error| AuthError::DatabaseError(format!("Password worker failed: {error}")))?
    .map_err(|error| AuthError::DatabaseError(format!("Verify error: {error}")))
}

pub(crate) fn acquire_auth_request(
    server: &SignalingServer,
) -> Result<tokio::sync::OwnedSemaphorePermit, AuthError> {
    server
        .try_acquire_auth_request()
        .ok_or(AuthError::ServiceBusy)
}

/// POST /api/auth/register
pub(crate) async fn register(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(req): Json<RegisterRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    // A live invite code opens closed registration; an offered code is always
    // spent, so an inviter's allowance is not a free pass past an open door.
    let invite_code = match req.invite_code.as_deref().map(str::trim) {
        Some(code) if !code.is_empty() => Some(
            crate::invite_codes::normalize(code)
                .ok_or(AuthError::InvalidInput("Invalid invite code"))?,
        ),
        _ => None,
    };
    if !server.registration_enabled() && invite_code.is_none() {
        return Err(AuthError::InviteRequired);
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }

    let email = canonicalize_email(&req.email)?;
    validate_display_name(&req.display_name)?;
    validate_new_password(&req.password)?;
    // The taken-email answer below is the only way to learn whether an address
    // has an account here, so each address gets a few answers an hour, not more.
    if !server.allow_registration(source_ip) {
        return Err(AuthError::RateLimited);
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
    let invited_by = match invite_code.as_deref() {
        Some(code) => Some(
            super::invites::consume_registration_invite(&mut transaction, code)
                .await
                .map_err(database_error)?
                .ok_or(AuthError::InvalidInput(
                    "This invite code is invalid, used up or expired",
                ))?,
        ),
        None => None,
    };

    let row = sqlx::query_as::<_, (Uuid, String, String)>(
        "INSERT INTO users (email, display_name, password_hash, invited_by) VALUES ($1, $2, $3, $4) RETURNING id, email, display_name",
    )
    .bind(&email)
    .bind(&req.display_name)
    .bind(&password_hash)
    .bind(invited_by)
    .fetch_one(&mut *transaction)
    .await
    .map_err(user_insert_error)?;

    let user_id = row.0.to_string();
    let session_id = session::create_session_with(&mut transaction, &row.0, &refresh_token).await?;
    let token = jwt::create_session_token(&user_id, &row.2, secret, 0, session_id)?;
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
pub(crate) async fn login(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(req): Json<LoginRequest>,
) -> Result<(HeaderMap, Json<AuthResponse>), AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let email = canonicalize_email(&req.email)?;
    // Only failures cost anything, and only this client's: knowing an email
    // no longer lets a stranger hold its owner out of password sign-in.
    if let Some(wait) = server.sign_in_wait(source_ip, &email) {
        return Err(AuthError::TooManyFailures(wait));
    }
    if req.password.len() > MAX_PASSWORD_BYTES {
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
        server.note_sign_in_failure(source_ip, &email);
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
    let session_id = session::create_session_with(&mut transaction, &row.0, &refresh_token).await?;
    let token = jwt::create_session_token(&user_id, &row.2, secret, row.4, session_id)?;
    transaction.commit().await.map_err(database_error)?;
    server.note_sign_in_success(source_ip, &email);

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

    let (user_id, session_id, refresh_token) =
        match session::rotate_refresh_token_with(&mut transaction, raw_token).await? {
            session::RefreshRotation::Rotated {
                user_id,
                session_id,
                refresh_token,
            } => (user_id, session_id, refresh_token),
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
    let token = jwt::create_session_token(&user_id_string, &row.1, secret, row.2, session_id)?;
    transaction.commit().await.map_err(database_error)?;
    session::spawn_expired_cleanup(pool, server.session_cleanup());

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
    // Password and passkey signup share the same taken-email answer budget.
    if !server.allow_registration(source_ip) {
        return Err(AuthError::RateLimited);
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

    let user_id = Uuid::new_v4();
    let (mut challenge, state) = webauthn
        .start_passkey_registration(user_id, &email, &req.display_name, None)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    require_discoverable_registration(&mut challenge)?;
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
    let session_id =
        session::create_session_with(&mut transaction, &registration.user_id, &refresh_token)
            .await?;

    let user_id = registration.user_id.to_string();
    let token =
        jwt::create_session_token(&user_id, &registration.display_name, secret, 0, session_id)?;
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

/// Adapt the pinned library's browser policy while retaining its verifier.
/// Resident-key selection is a browser requirement, not signed evidence about
/// authenticator storage. Successful usernameless authentication is the usable
/// compatibility check; an unsigned `credProps.rk` hint is never authorization.
pub(super) fn require_discoverable_registration(
    challenge: &mut CreationChallengeResponse,
) -> Result<(), AuthError> {
    let selection = challenge
        .public_key
        .authenticator_selection
        .as_mut()
        .ok_or_else(|| AuthError::WebAuthnError("Registration selection policy missing".into()))?;
    selection.resident_key = Some(webauthn_rs_proto::ResidentKeyRequirement::Required);
    selection.require_resident_key = true;
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ModalLoginOptions {
    public_key: webauthn_rs_proto::PublicKeyCredentialRequestOptions,
    mediation: ModalMediation,
}

#[derive(Serialize)]
#[serde(rename_all = "lowercase")]
enum ModalMediation {
    Required,
}

pub(super) fn modal_login_options(challenge: RequestChallengeResponse) -> Result<Value, AuthError> {
    // webauthn-rs 0.5.5 exposes discoverable verification under conditional-ui.
    // Mediation only controls browser presentation: it is not in the signed
    // assertion or server AuthenticationState. This explicit button uses a modal
    // prompt while keeping the generated challenge, required UV and RP intact.
    serde_json::to_value(ModalLoginOptions {
        public_key: challenge.public_key,
        mediation: ModalMediation::Required,
    })
    .map_err(|error| AuthError::WebAuthnError(error.to_string()))
}

/// POST /api/auth/passkey/login/start
///
/// Anonymous challenge creation never looks up an account or returns credential
/// IDs. Unknown JSON fields (including the retired email selector) are rejected
/// before account state could influence the response.
pub(crate) async fn passkey_login_start(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(_body): Json<PasskeyLoginStartRequest>,
) -> Result<Json<Value>, AuthError> {
    let _pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let webauthn = server.webauthn().ok_or(AuthError::NotConfigured)?;
    let store = server.challenge_store().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    if !jwt::secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }
    let _request_permit = acquire_auth_request(&server)?;

    let (challenge, state) = webauthn
        .start_discoverable_authentication()
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    let ceremony_id = store
        .store_authentication(state, source_ip)
        .ok_or(AuthError::RateLimited)?;
    Ok(Json(add_ceremony_id(
        modal_login_options(challenge)?,
        ceremony_id,
    )?))
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
        .ok_or(AuthError::InvalidPasskey)?;
    // The handle and credential ID are untrusted selectors until verification.
    // Resolve them together; never accept a credential owned by another account.
    let (account_id, presented_id) = webauthn
        .identify_discoverable_authentication(&body.credential)
        .map_err(|_| AuthError::InvalidPasskey)?;
    let passkey_id = URL_SAFE_NO_PAD.encode(presented_id);
    let mut transaction = pool.begin().await.map_err(database_error)?;

    // Keep the same users-before-credentials lock order as account mutation.
    // The verifier reads the locked current credential, so parallel assertions
    // cannot verify against a stale counter or a removed credential.
    let user = sqlx::query_as::<_, (String, String, i64)>(
        "SELECT email, display_name, auth_version FROM users WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(account_id)
    .fetch_optional(&mut *transaction)
    .await
    .map_err(database_error)?
    .ok_or(AuthError::InvalidPasskey)?;
    verify_discoverable_assertion(
        &mut transaction,
        webauthn,
        &body.credential,
        authentication.state,
        account_id,
        &passkey_id,
    )
    .await?;
    let refresh_token = session::generate_refresh_token()?;
    let session_id =
        session::create_session_with(&mut transaction, &account_id, &refresh_token).await?;

    let user_id = account_id.to_string();
    let token = jwt::create_session_token(&user_id, &user.1, secret, user.2, session_id)?;
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

/// Verify against the locked current credential. The caller must first lock its user row.
/// The same counter and ownership policy protects login and sensitive account operations.
pub(super) async fn verify_discoverable_assertion(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    webauthn: &Webauthn,
    credential: &PublicKeyCredential,
    state: DiscoverableAuthentication,
    account_id: Uuid,
    passkey_id: &str,
) -> Result<(), AuthError> {
    let credential_json: Value = sqlx::query_scalar(
        "SELECT credential_json FROM webauthn_credentials WHERE user_id = $1 AND credential_id = $2 FOR UPDATE",
    )
    .bind(account_id)
    .bind(passkey_id)
    .fetch_optional(&mut **transaction)
    .await
    .map_err(database_error)?
    .ok_or(AuthError::InvalidPasskey)?;
    let mut passkey: Passkey = serde_json::from_value(credential_json.clone())
        .map_err(|error| AuthError::DatabaseError(format!("Invalid credential: {error}")))?;
    let result = webauthn
        .finish_discoverable_authentication(credential, state, &[DiscoverableKey::from(&passkey)])
        .map_err(|_| AuthError::InvalidPasskey)?;
    if authentication_credential_id(&result) != passkey_id {
        return Err(AuthError::InvalidPasskey);
    }

    // Keep the strict application counter policy as well as library validation:
    // positive counters must advance, including concurrent ceremonies.
    let stored_counter = credential_json
        .pointer("/cred/counter")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let presented_counter = u64::from(result.counter());
    if (stored_counter > 0 || presented_counter > 0) && presented_counter <= stored_counter {
        return Err(AuthError::InvalidPasskey);
    }

    passkey
        .update_credential(&result)
        .ok_or(AuthError::InvalidPasskey)?;
    let updated_credential = serde_json::to_value(passkey)
        .map_err(|error| AuthError::WebAuthnError(error.to_string()))?;
    sqlx::query(
        "UPDATE webauthn_credentials SET credential_json = $3 WHERE user_id = $1 AND credential_id = $2",
    )
    .bind(account_id)
    .bind(passkey_id)
    .bind(updated_credential)
    .execute(&mut **transaction)
    .await
    .map_err(database_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_password_policy_counts_nfc_characters_and_allows_long_scripts() {
        for password in [
            "a".repeat(14),
            "e\u{301}".repeat(14),
            "x\n".repeat(8),
            "a".repeat(129),
        ] {
            assert!(validate_new_password(&password).is_err());
        }
        for password in ["a".repeat(15), "e\u{301}".repeat(15), "語".repeat(128)] {
            assert!(validate_new_password(&password).is_ok());
        }
    }

    #[test]
    fn passkey_login_start_accepts_no_account_selector() {
        assert!(serde_json::from_value::<PasskeyLoginStartRequest>(serde_json::json!({})).is_ok());
        for value in [
            serde_json::json!({"email": "user@example.test"}),
            serde_json::json!({"user_id": Uuid::new_v4()}),
            serde_json::json!({"credential_id": "credential"}),
            serde_json::json!(null),
        ] {
            assert!(serde_json::from_value::<PasskeyLoginStartRequest>(value).is_err());
        }
    }

    #[test]
    fn passkey_browser_policy_requires_discovery_without_restricting_provider() {
        let webauthn = WebauthnBuilder::new("localhost", &Url::parse("https://localhost").unwrap())
            .unwrap()
            .build()
            .unwrap();
        let (mut registration, _) = webauthn
            .start_passkey_registration(Uuid::new_v4(), "user@example.test", "User", None)
            .unwrap();
        require_discoverable_registration(&mut registration).unwrap();
        let registration = serde_json::to_value(registration).unwrap();
        let selection = &registration["publicKey"]["authenticatorSelection"];
        assert_eq!(selection["residentKey"], "required");
        assert_eq!(selection["requireResidentKey"], true);
        assert_eq!(selection["userVerification"], "required");
        assert!(selection.get("authenticatorAttachment").is_none());
        assert_eq!(registration["publicKey"]["attestation"], "none");

        let (challenge, _) = webauthn.start_discoverable_authentication().unwrap();
        let expected_public_key = serde_json::to_value(&challenge.public_key).unwrap();
        let response = modal_login_options(challenge).unwrap();
        assert_eq!(response["publicKey"], expected_public_key);
        assert_eq!(response["mediation"], "required");
        assert_eq!(
            response["publicKey"]["allowCredentials"],
            serde_json::json!([])
        );
        assert_eq!(response["publicKey"]["userVerification"], "required");
    }

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
        assert!(!password::verify_password("not-the-dummy-password", DUMMY_PASSWORD_HASH).unwrap());
    }

    #[test]
    fn rejects_non_host_prefixed_refresh_cookie() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            HeaderValue::from_static("refresh_token=attacker-controlled-token"),
        );
        assert_eq!(refresh_token_from_headers(&headers), None);
    }

    #[test]
    fn accepts_the_current_opaque_refresh_token_format() {
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
    fn rejects_unsupported_or_malformed_refresh_cookies() {
        for raw in [
            "v1.invalid.invalid".to_string(),
            uuid::Uuid::new_v4().to_string(),
            format!("v1l{}", "A".repeat(91)),
            "invalid-token".to_string(),
        ] {
            let mut headers = HeaderMap::new();
            headers.insert(
                header::COOKIE,
                HeaderValue::from_str(&format!("__Host-refresh_token={raw}")).unwrap(),
            );
            assert_eq!(refresh_token_from_headers(&headers), None);
        }
    }

    #[test]
    fn refresh_response_sets_and_clears_only_the_current_host_cookie() {
        let headers = refresh_cookie_headers("safe-token");
        let cookies = headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().expect("valid cookie"))
            .collect::<Vec<_>>();
        assert!(cookies.iter().any(|cookie| {
            cookie.starts_with("__Host-refresh_token=safe-token;") && cookie.contains("Path=/;")
        }));
        assert_eq!(cookies.len(), 1);
        let cleared = clear_refresh_cookie_headers();
        let cleared: Vec<_> = cleared.get_all(header::SET_COOKIE).iter().collect();
        assert_eq!(cleared.len(), 1);
        assert_eq!(
            cleared[0],
            "__Host-refresh_token=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"
        );
        assert_eq!(
            headers
                .get(header::CACHE_CONTROL)
                .and_then(|v| v.to_str().ok()),
            Some("no-store, max-age=0")
        );
    }
}
