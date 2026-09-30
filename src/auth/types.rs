#![forbid(unsafe_code)]

use axum::{
    Json,
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum AuthError {
    InvalidInput(&'static str),
    InvalidCredentials,
    InvalidPasskey,
    EmailAlreadyExists,
    UserNotFound,
    InvalidToken,
    MissingToken,
    TokenExpired,
    RateLimited,
    /// Failed sign-ins have earned this client a wait, in whole seconds.
    TooManyFailures(u64),
    /// Registration is closed to strangers; a live invite code opens it.
    InviteRequired,
    InviteNotFound,
    ServiceBusy,
    RegistrationDisabled,
    DatabaseError(String),
    WebAuthnError(String),
    NotConfigured,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let retry_after = match &self {
            AuthError::ServiceBusy => Some(1),
            AuthError::TooManyFailures(seconds) => Some(*seconds),
            _ => None,
        };
        let wait_message = match &self {
            AuthError::TooManyFailures(seconds) => Some(format!(
                "Too many failed attempts; try again in {seconds} s"
            )),
            _ => None,
        };
        let (status, message) = match self {
            AuthError::InvalidInput(message) => (StatusCode::BAD_REQUEST, message),
            AuthError::TooManyFailures(_) => {
                (StatusCode::TOO_MANY_REQUESTS, "Too many failed attempts")
            }
            AuthError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "Invalid email or password")
            }
            AuthError::InvalidPasskey => (StatusCode::UNAUTHORIZED, "Passkey sign-in failed"),
            AuthError::EmailAlreadyExists => (StatusCode::CONFLICT, "Email already registered"),
            AuthError::UserNotFound => (StatusCode::NOT_FOUND, "User not found"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing authorization"),
            AuthError::TokenExpired => (StatusCode::UNAUTHORIZED, "Token expired"),
            AuthError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "Too many attempts"),
            AuthError::ServiceBusy => (StatusCode::SERVICE_UNAVAILABLE, "Service busy"),
            AuthError::RegistrationDisabled => (StatusCode::FORBIDDEN, "Registration is disabled"),
            AuthError::InviteRequired => (
                StatusCode::FORBIDDEN,
                "Registration is by invitation; enter an invite code",
            ),
            AuthError::InviteNotFound => (StatusCode::NOT_FOUND, "Invite not found"),
            AuthError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
            AuthError::WebAuthnError(_) => (StatusCode::BAD_REQUEST, "WebAuthn error"),
            AuthError::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Authentication not configured",
            ),
        };
        let message = wait_message.unwrap_or_else(|| message.to_owned());
        let mut response = (status, Json(serde_json::json!({ "error": message }))).into_response();
        if let Some(seconds) = retry_after
            && let Ok(value) = HeaderValue::from_str(&seconds.to_string())
        {
            response.headers_mut().insert(header::RETRY_AFTER, value);
        }
        response
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub display_name: String,
    /// A registration invitation; required while registration is closed.
    #[serde(default)]
    pub invite_code: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Serialize)]
pub struct AuthResponse {
    pub token: String,
    pub user: UserInfo,
}

#[derive(Debug, Serialize, Clone)]
pub struct UserInfo {
    pub id: String,
    pub email: String,
    pub display_name: String,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Claims {
    pub sub: String,
    pub name: String,
    pub iss: String,
    pub aud: String,
    pub exp: usize,
    pub auth_version: i64,
    /// The refresh session this token was issued with. Validation requires that
    /// live session to belong to the account, so logout retires the token.
    pub sid: uuid::Uuid,
}
