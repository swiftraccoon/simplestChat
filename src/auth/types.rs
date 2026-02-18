#![forbid(unsafe_code)]

use axum::{http::StatusCode, response::{IntoResponse, Response}, Json};
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub enum AuthError {
    InvalidCredentials,
    EmailAlreadyExists,
    UserNotFound,
    InvalidToken,
    MissingToken,
    TokenExpired,
    DatabaseError(String),
    WebAuthnError(String),
    NotConfigured,
}

impl IntoResponse for AuthError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            AuthError::InvalidCredentials => (StatusCode::UNAUTHORIZED, "Invalid email or password"),
            AuthError::EmailAlreadyExists => (StatusCode::CONFLICT, "Email already registered"),
            AuthError::UserNotFound => (StatusCode::NOT_FOUND, "User not found"),
            AuthError::InvalidToken => (StatusCode::UNAUTHORIZED, "Invalid token"),
            AuthError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing authorization"),
            AuthError::TokenExpired => (StatusCode::UNAUTHORIZED, "Token expired"),
            AuthError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Database error"),
            AuthError::WebAuthnError(_) => (StatusCode::BAD_REQUEST, "WebAuthn error"),
            AuthError::NotConfigured => (StatusCode::SERVICE_UNAVAILABLE, "Authentication not configured"),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
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
    pub exp: usize,
}
