#![forbid(unsafe_code)]

use crate::auth::types::{AuthError, Claims};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation, decode, encode};
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_LIFETIME_SECS: u64 = 15 * 60;
const JWT_ISSUER: &str = "simplestchat";
const JWT_AUDIENCE: &str = "simplestchat";
pub const MIN_SECRET_BYTES: usize = 32;

pub fn secret_is_strong(secret: &str) -> bool {
    secret.as_bytes().len() >= MIN_SECRET_BYTES
}

pub fn create_token(user_id: &str, display_name: &str, secret: &str) -> Result<String, AuthError> {
    if !secret_is_strong(secret) {
        return Err(AuthError::NotConfigured);
    }

    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::DatabaseError("System clock error".to_string()))?;

    let claims = Claims {
        sub: user_id.to_string(),
        name: display_name.to_string(),
        iss: JWT_ISSUER.to_string(),
        aud: JWT_AUDIENCE.to_string(),
        exp: (now.as_secs() + TOKEN_LIFETIME_SECS) as usize,
    };

    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AuthError::DatabaseError(format!("JWT encode error: {e}")))
}

pub fn validate_token(token: &str, secret: &str) -> Result<Claims, AuthError> {
    if !secret_is_strong(secret) {
        return Err(AuthError::InvalidToken);
    }

    let mut validation = Validation::new(Algorithm::HS256);
    // Access-token expiry is enforced exactly. Both issuer and verifier use the
    // local system clock, so the library's default 60-second grace is neither
    // necessary nor consistent with the WebSocket expiry enforcement.
    validation.leeway = 0;
    validation.set_issuer(&[JWT_ISSUER]);
    validation.set_audience(&[JWT_AUDIENCE]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);

    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &validation,
    )
    .map_err(|e| match e.kind() {
        jsonwebtoken::errors::ErrorKind::ExpiredSignature => AuthError::TokenExpired,
        _ => AuthError::InvalidToken,
    })?;

    Ok(data.claims)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_and_validate_token() {
        let secret = "test-secret-at-least-32-bytes-long!!";
        let token = create_token("user-123", "Alice", secret).unwrap();
        let claims = validate_token(&token, secret).unwrap();
        assert_eq!(claims.sub, "user-123");
        assert_eq!(claims.name, "Alice");
    }

    #[test]
    fn test_invalid_secret_rejects() {
        let token = create_token("user-123", "Alice", "secret-1-at-least-32-bytes-long!!").unwrap();
        let result = validate_token(&token, "secret-2-at-least-32-bytes-long!!");
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn test_weak_secret_rejects() {
        assert!(!secret_is_strong("secret"));
        assert!(matches!(
            create_token("user-123", "Alice", "secret"),
            Err(AuthError::NotConfigured)
        ));
    }

    #[test]
    fn test_garbage_token_rejects() {
        let result = validate_token("not.a.jwt", "secret");
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn test_expired_token_rejects_without_clock_skew_grace() {
        let secret = "test-secret-at-least-32-bytes-long!!";
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as usize;
        let claims = Claims {
            sub: "user-123".to_string(),
            name: "Alice".to_string(),
            iss: JWT_ISSUER.to_string(),
            aud: JWT_AUDIENCE.to_string(),
            exp: now.saturating_sub(1),
        };
        let token = encode(
            &Header::new(Algorithm::HS256),
            &claims,
            &EncodingKey::from_secret(secret.as_bytes()),
        )
        .unwrap();

        assert!(matches!(
            validate_token(&token, secret),
            Err(AuthError::TokenExpired)
        ));
    }
}
