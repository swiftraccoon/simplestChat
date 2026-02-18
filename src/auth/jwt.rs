#![forbid(unsafe_code)]

use crate::auth::types::{AuthError, Claims};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use std::time::{SystemTime, UNIX_EPOCH};

const TOKEN_LIFETIME_SECS: u64 = 15 * 60;

pub fn create_token(user_id: &str, display_name: &str, secret: &str) -> Result<String, AuthError> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AuthError::DatabaseError("System clock error".to_string()))?;

    let claims = Claims {
        sub: user_id.to_string(),
        name: display_name.to_string(),
        exp: (now.as_secs() + TOKEN_LIFETIME_SECS) as usize,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|e| AuthError::DatabaseError(format!("JWT encode error: {e}")))
}

pub fn validate_token(token: &str, secret: &str) -> Result<Claims, AuthError> {
    let data = decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
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
        let token = create_token("user-123", "Alice", "secret-1").unwrap();
        let result = validate_token(&token, "secret-2");
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }

    #[test]
    fn test_garbage_token_rejects() {
        let result = validate_token("not.a.jwt", "secret");
        assert!(matches!(result, Err(AuthError::InvalidToken)));
    }
}
