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
    EmailAlreadyExists,
    UserNotFound,
    InvalidToken,
    MissingToken,
    TokenExpired,
    RateLimited,
    ServiceBusy,
    RegistrationDisabled,
    DatabaseError(String),
    WebAuthnError(String),
    NotConfigured,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let service_busy = matches!(&self, AuthError::ServiceBusy);
        let (status, message) = match self {
            AuthError::InvalidInput(message) => (StatusCode::BAD_REQUEST, message),
            AuthError::InvalidCredentials => {
                (StatusCode::UNAUTHORIZED, "Invalid email or password")
            }
            AuthError::EmailAlreadyExists => (StatusCode::CONFLICT, "Email already registered"),
            AuthError::UserNotFound => (StatusCode::NOT_FOUND, "User not found"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing authorization"),
            AuthError::TokenExpired => (StatusCode::UNAUTHORIZED, "Token expired"),
            AuthError::RateLimited => (StatusCode::TOO_MANY_REQUESTS, "Too many attempts"),
            AuthError::ServiceBusy => (StatusCode::SERVICE_UNAVAILABLE, "Service busy"),
            AuthError::RegistrationDisabled => (StatusCode::FORBIDDEN, "Registration is disabled"),
            AuthError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
            AuthError::WebAuthnError(_) => (StatusCode::BAD_REQUEST, "WebAuthn error"),
            AuthError::NotConfigured => (
                StatusCode::SERVICE_UNAVAILABLE,
                "Authentication not configured",
            ),
        };
        let mut response = (status, Json(serde_json::json!({ "error": message }))).into_response();
        if service_busy {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from_static("1"));
        }
        response
    }
}

#[derive(Debug, Deserialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub display_name: String,
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
    #[serde(default)]
    pub auth_version: i64,
}
