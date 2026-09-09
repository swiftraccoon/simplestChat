#![forbid(unsafe_code)]

use super::{
    jwt, routes, session,
    types::{AuthError, Claims},
};
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::{TryRngCore, rngs::OsRng};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub const MAX_IMAGE_BYTES: usize = 128 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 2048;
const RECOVERY_PREFIX: &str = "sc-recovery-";

#[derive(Serialize, FromRow)]
pub struct AccountProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
    pub recovery_enabled: bool,
}

#[derive(Serialize, FromRow)]
pub struct PublicProfile {
    pub id: Uuid,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateProfileRequest {
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryKeyRequest {
    pub current_password: String,
}

#[derive(Serialize)]
pub struct RecoveryKeyResponse {
    pub recovery_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RedeemRecoveryRequest {
    pub email: String,
    pub recovery_key: String,
    pub new_password: String,
}

pub async fn authenticated_claims(
    server: &SignalingServer,
    headers: &HeaderMap,
) -> Result<Claims, AuthError> {
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)?;
    let claims = jwt::validate_token(token, secret)?;
    jwt::validate_current_claims(pool, &claims).await?;
    Ok(claims)
}

fn profile_error() -> AuthError {
    AuthError::InvalidInput("Invalid profile: name must be 1–64 bytes; bio at most 1024 bytes")
}

pub fn validate_text(value: &str, maximum: usize, multiline: bool) -> bool {
    value.len() <= maximum
        && !value.chars().any(|character| {
            character.is_control() && !(multiline && matches!(character, '\n' | '\t'))
        })
}

fn validate_password(password: &str) -> Result<(), AuthError> {
    if (8..=128).contains(&password.len()) && !password.chars().any(char::is_control) {
        Ok(())
    } else {
        Err(AuthError::InvalidInput(
            "Password must be 8–128 bytes without control characters",
        ))
    }
}

fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
    match mime {
        "image/png"
            if bytes.len() >= 33
                && bytes.starts_with(b"\x89PNG\r\n\x1a\n")
                && bytes.get(8..16) == Some(&b"\0\0\0\rIHDR"[..]) =>
        {
            Some((
                u32::from_be_bytes(bytes[16..20].try_into().ok()?),
                u32::from_be_bytes(bytes[20..24].try_into().ok()?),
            ))
        }
        "image/jpeg" if bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) => {
            let mut position = 2;
            while position + 4 <= bytes.len() {
                if bytes[position] != 0xff {
                    return None;
                }
                while bytes.get(position) == Some(&0xff) {
                    position += 1;
                }
                let marker = *bytes.get(position)?;
                position += 1;
                if matches!(marker, 0xd8 | 0xd9 | 0xda | 0x00) {
                    return None;
                }
                if matches!(marker, 0xd0..=0xd7 | 0x01) {
                    continue;
                }
                let length = u16::from_be_bytes(bytes.get(position..position + 2)?.try_into().ok()?)
                    as usize;
                if length < 2 || position + length > bytes.len() {
                    return None;
                }
                if matches!(marker, 0xc0..=0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf) {
                    if length < 8 {
                        return None;
                    }
                    let height =
                        u16::from_be_bytes(bytes[position + 3..position + 5].try_into().ok()?);
                    let width =
                        u16::from_be_bytes(bytes[position + 5..position + 7].try_into().ok()?);
                    return Some((width.into(), height.into()));
                }
                position += length;
            }
            None
        }
        "image/webp"
            if bytes.len() >= 30
                && bytes.starts_with(b"RIFF")
                && bytes.get(8..12) == Some(&b"WEBP"[..]) =>
        {
            let riff_len = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
            if riff_len + 8 != bytes.len() {
                return None;
            }
            let little24 = |data: &[u8]| {
                u32::from(data[0]) | u32::from(data[1]) << 8 | u32::from(data[2]) << 16
            };
            match bytes.get(12..16)? {
                b"VP8X" if bytes[20] & 0x02 == 0 => {
                    Some((little24(&bytes[24..27]) + 1, little24(&bytes[27..30]) + 1))
                }
                b"VP8 " if bytes.get(23..26) == Some(&[0x9d, 0x01, 0x2a][..]) => Some((
                    u16::from_le_bytes(bytes[26..28].try_into().ok()?) as u32 & 0x3fff,
                    u16::from_le_bytes(bytes[28..30].try_into().ok()?) as u32 & 0x3fff,
                )),
                b"VP8L" if bytes[20] == 0x2f => {
                    let packed = u32::from_le_bytes(bytes[21..25].try_into().ok()?);
                    Some(((packed & 0x3fff) + 1, ((packed >> 14) & 0x3fff) + 1))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// Images are stored only as bounded raster data URLs and rendered as images.
/// Header/dimension validation rejects SVG, arbitrary URLs, and oversized rasters;
/// the browser's image decoder handles the remainder of the raster bitstream.
pub fn validate_image_data_url(value: Option<&str>) -> Result<(), AuthError> {
    let Some(value) = value else {
        return Ok(());
    };
    if value.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 + 32 {
        return Err(AuthError::InvalidInput("Image must be at most 128 KiB"));
    }
    let (header, data) = value
        .split_once(',')
        .ok_or(AuthError::InvalidInput("Invalid image"))?;
    let mime = header
        .strip_prefix("data:")
        .and_then(|header| header.strip_suffix(";base64"))
        .ok_or(AuthError::InvalidInput("Use a PNG, JPEG, or WebP image"))?;
    let bytes = STANDARD
        .decode(data)
        .map_err(|_| AuthError::InvalidInput("Invalid image encoding"))?;
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(AuthError::InvalidInput("Image must be at most 128 KiB"));
    }
    let (width, height) =
        image_dimensions(mime, &bytes).ok_or(AuthError::InvalidInput("Invalid raster image"))?;
    if width == 0 || height == 0 || width > MAX_IMAGE_DIMENSION || height > MAX_IMAGE_DIMENSION {
        return Err(AuthError::InvalidInput(
            "Image dimensions must be at most 2048 × 2048",
        ));
    }
    Ok(())
}

async fn load_profile(pool: &PgPool, id: Uuid) -> Result<AccountProfile, AuthError> {
    sqlx::query_as("SELECT id, email, display_name, avatar_url, bio, recovery_key_hash IS NOT NULL AS recovery_enabled FROM users WHERE id = $1")
        .bind(id).fetch_optional(pool).await.map_err(routes::database_error)?.ok_or(AuthError::UserNotFound)
}

pub async fn get_profile(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<AccountProfile>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let profile = load_profile(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?,
    )
    .await?;
    Ok((routes::no_store_headers(), Json(profile)))
}

pub async fn update_profile(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<UpdateProfileRequest>,
) -> Result<(HeaderMap, Json<AccountProfile>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    routes::validate_display_name(&request.display_name).map_err(|_| profile_error())?;
    if !validate_text(&request.bio, 1024, true) {
        return Err(profile_error());
    }
    validate_image_data_url(request.avatar_url.as_deref())?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let updated = sqlx::query("UPDATE users SET display_name = $2, avatar_url = $3, bio = $4, updated_at = now() WHERE id = $1 AND auth_version = $5")
        .bind(id).bind(request.display_name.trim()).bind(request.avatar_url).bind(request.bio).bind(claims.auth_version)
        .execute(pool).await.map_err(routes::database_error)?;
    if updated.rows_affected() == 0 {
        return Err(AuthError::InvalidToken);
    }
    Ok((
        routes::no_store_headers(),
        Json(load_profile(pool, id).await?),
    ))
}

pub async fn public_profile(
    State(server): State<SignalingServer>,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<PublicProfile>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let profile =
        sqlx::query_as("SELECT id, display_name, avatar_url, bio FROM users WHERE id = $1")
            .bind(id)
            .fetch_optional(pool)
            .await
            .map_err(routes::database_error)?
            .ok_or(AuthError::UserNotFound)?;
    Ok((routes::no_store_headers(), Json(profile)))
}

async fn verified_password_hash(
    server: &SignalingServer,
    claims: &Claims,
    password: String,
) -> Result<String, AuthError> {
    if password.len() > 128 {
        return Err(AuthError::InvalidCredentials);
    }
    if !server.allow_auth_principal(&claims.sub) {
        return Err(AuthError::RateLimited);
    }
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let stored: Option<String> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 AND auth_version = $2")
            .bind(id)
            .bind(claims.auth_version)
            .fetch_optional(pool)
            .await
            .map_err(routes::database_error)?
            .flatten();
    let stored = stored.ok_or(AuthError::InvalidCredentials)?;
    let permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::ServiceBusy)?;
    if !routes::verify_password_async(password, stored.clone(), permit).await? {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(stored)
}

pub async fn change_password(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<ChangePasswordRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    validate_password(&request.new_password)?;
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let old_hash = verified_password_hash(&server, &claims, request.current_password).await?;
    let permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::ServiceBusy)?;
    let new_hash = routes::hash_password_async(request.new_password, permit).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let changed = sqlx::query("UPDATE users SET password_hash = $2, auth_version = auth_version + 1, updated_at = now() WHERE id = $1 AND password_hash = $3 AND auth_version = $4")
        .bind(id).bind(new_hash).bind(old_hash).bind(claims.auth_version)
        .execute(&mut *transaction).await.map_err(routes::database_error)?;
    if changed.rows_affected() == 0 {
        return Err(AuthError::InvalidCredentials);
    }
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    server.revoke_account_sessions(claims.sub, claims.auth_version + 1);
    Ok((
        routes::clear_refresh_cookie_headers(),
        StatusCode::NO_CONTENT,
    ))
}

fn generate_recovery_key() -> Result<String, AuthError> {
    let mut secret = [0u8; 32];
    OsRng.try_fill_bytes(&mut secret).map_err(|error| {
        AuthError::DatabaseError(format!("Recovery generation failed: {error}"))
    })?;
    Ok(format!(
        "{RECOVERY_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(secret)
    ))
}

fn recovery_hash(key: &str) -> Result<String, AuthError> {
    let encoded = key
        .strip_prefix(RECOVERY_PREFIX)
        .ok_or(AuthError::InvalidCredentials)?;
    if encoded.len() != 43
        || URL_SAFE_NO_PAD
            .decode(encoded)
            .ok()
            .is_none_or(|bytes| bytes.len() != 32)
    {
        return Err(AuthError::InvalidCredentials);
    }
    Ok(session::hash_token(key))
}

pub async fn create_recovery_key(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<RecoveryKeyRequest>,
) -> Result<(HeaderMap, Json<RecoveryKeyResponse>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let old_hash = verified_password_hash(&server, &claims, request.current_password).await?;
    let key = generate_recovery_key()?;
    let key_hash = recovery_hash(&key)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let updated = sqlx::query("UPDATE users SET recovery_key_hash = $2, updated_at = now() WHERE id = $1 AND password_hash = $3 AND auth_version = $4")
        .bind(id).bind(key_hash).bind(old_hash).bind(claims.auth_version).execute(pool).await.map_err(routes::database_error)?;
    if updated.rows_affected() == 0 {
        return Err(AuthError::InvalidCredentials);
    }
    Ok((
        routes::no_store_headers(),
        Json(RecoveryKeyResponse { recovery_key: key }),
    ))
}

async fn consume_recovery(
    pool: &PgPool,
    email: &str,
    key_hash: &str,
    new_hash: &str,
) -> Result<(Uuid, i64), AuthError> {
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    // Conditional UPDATE takes the user lock, atomically consumes this key, and
    // serializes with password changes, session issuance, and refresh rotation.
    let (id, version): (Uuid, i64) = sqlx::query_as("UPDATE users SET password_hash = $3, recovery_key_hash = NULL, auth_version = auth_version + 1, updated_at = now() WHERE email = $1 AND recovery_key_hash = $2 RETURNING id, auth_version")
        .bind(email).bind(key_hash).bind(new_hash).fetch_optional(&mut *transaction).await.map_err(routes::database_error)?.ok_or(AuthError::InvalidCredentials)?;
    sqlx::query("DELETE FROM sessions WHERE user_id = $1")
        .bind(id)
        .execute(&mut *transaction)
        .await
        .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((id, version))
}

pub async fn redeem_recovery(
    State(server): State<SignalingServer>,
    Json(request): Json<RedeemRecoveryRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let email = routes::canonicalize_email(&request.email)?;
    if !server.allow_auth_principal(&email) {
        return Err(AuthError::RateLimited);
    }
    validate_password(&request.new_password)?;
    let hash = recovery_hash(request.recovery_key.trim())?;
    let _permit = routes::acquire_auth_request(&server)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let eligible: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND recovery_key_hash = $2)",
    )
    .bind(&email)
    .bind(&hash)
    .fetch_one(pool)
    .await
    .map_err(routes::database_error)?;
    if !eligible {
        return Err(AuthError::InvalidCredentials);
    }
    let permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::ServiceBusy)?;
    let password_hash = routes::hash_password_async(request.new_password, permit).await?;
    let (id, version) = consume_recovery(pool, &email, &hash, &password_hash).await?;
    server.revoke_account_sessions(id.to_string(), version);
    Ok((
        routes::clear_refresh_cookie_headers(),
        StatusCode::NO_CONTENT,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recovery_keys_have_full_entropy_shape_and_only_hashes_are_stored() {
        let first = generate_recovery_key().unwrap();
        let second = generate_recovery_key().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), RECOVERY_PREFIX.len() + 43);
        assert_eq!(recovery_hash(&first).unwrap().len(), 64);
        assert!(recovery_hash("guess").is_err());
        assert!(recovery_hash(&format!("{RECOVERY_PREFIX}{}", "A".repeat(1000))).is_err());
    }

    #[test]
    fn profile_content_rejects_active_formats_and_unbounded_images() {
        assert!(validate_image_data_url(None).is_ok());
        for value in [
            "https://example.test/avatar.png",
            "data:image/svg+xml;base64,PHN2Zz4=",
            "data:text/html;base64,PGh0bWw+",
            "data:image/png;base64,aGVsbG8=",
        ] {
            assert!(validate_image_data_url(Some(value)).is_err());
        }
        let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aCGUAAAAASUVORK5CYII=").unwrap();
        assert!(
            validate_image_data_url(Some(&format!(
                "data:image/png;base64,{}",
                STANDARD.encode(&png)
            )))
            .is_ok()
        );
        let mut huge_dimensions = png.clone();
        huge_dimensions[16..20].copy_from_slice(&4000u32.to_be_bytes());
        assert!(
            validate_image_data_url(Some(&format!(
                "data:image/png;base64,{}",
                STANDARD.encode(huge_dimensions)
            )))
            .is_err()
        );
        assert!(
            validate_image_data_url(Some(&format!(
                "data:image/png;base64,{}",
                "A".repeat(180_000)
            )))
            .is_err()
        );
        assert!(validate_text("A short bio\nsecond line", 1024, true));
        assert!(!validate_text("bad\0value", 1024, true));
        assert!(!validate_text(&"a".repeat(1025), 1024, true));
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn recovery_is_atomic_one_time_and_revokes_all_sessions() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(4)
            .connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
            .await
            .unwrap();
        let email = format!("recovery-{}@example.test", Uuid::new_v4());
        let key = generate_recovery_key().unwrap();
        let key_hash = recovery_hash(&key).unwrap();
        let id: Uuid = sqlx::query_scalar("INSERT INTO users (email, display_name, password_hash, recovery_key_hash) VALUES ($1, 'Recovery test', 'old-hash', $2) RETURNING id")
            .bind(&email).bind(&key_hash).fetch_one(&pool).await.unwrap();
        let secret = "account-recovery-test-secret-at-least-32-bytes";
        let old_token = jwt::create_token(&id.to_string(), "Recovery test", secret).unwrap();
        let old_claims = jwt::validate_token(&old_token, secret).unwrap();
        assert!(
            jwt::validate_current_claims(&pool, &old_claims)
                .await
                .is_ok()
        );
        for _ in 0..2 {
            session::create_session(&pool, &id, &session::generate_refresh_token().unwrap())
                .await
                .unwrap();
        }
        assert!(
            consume_recovery(&pool, &email, &session::hash_token("wrong"), "bad-hash")
                .await
                .is_err()
        );
        let still_live: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(still_live, 2);
        let (first, second) = tokio::join!(
            consume_recovery(&pool, &email, &key_hash, "new-one"),
            consume_recovery(&pool, &email, &key_hash, "new-two")
        );
        assert_ne!(first.is_ok(), second.is_ok());
        let row: (Option<String>, i64, Option<String>) = sqlx::query_as(
            "SELECT recovery_key_hash, auth_version, password_hash FROM users WHERE id = $1",
        )
        .bind(id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert!(row.0.is_none());
        assert_eq!(row.1, 1);
        assert!(matches!(row.2.as_deref(), Some("new-one" | "new-two")));
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM sessions WHERE user_id = $1")
            .bind(id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(remaining, 0);
        assert!(matches!(
            jwt::validate_current_claims(&pool, &old_claims).await,
            Err(AuthError::InvalidToken)
        ));
        let current_token =
            jwt::create_token_with_version(&id.to_string(), "Recovery test", secret, 1).unwrap();
        let current_claims = jwt::validate_token(&current_token, secret).unwrap();
        assert!(
            jwt::validate_current_claims(&pool, &current_claims)
                .await
                .is_ok()
        );
        assert!(
            consume_recovery(&pool, &email, &key_hash, "replay")
                .await
                .is_err()
        );
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
    }
}
