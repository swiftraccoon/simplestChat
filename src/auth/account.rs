//! Account profiles, password changes and one-time recovery credentials.
//!
//! Authenticated handlers validate both the signed access token and the current
//! database authentication version. Password changes and recovery commit the
//! new password, increment that version and remove refresh sessions together;
//! only then do they notify connected sessions of revocation. Profile edits do
//! not change account credentials. Responses containing account or recovery
//! information are not cacheable.
//!
//! Recovery is possession-based, not an email-verification or reset-email flow.
//! Only a digest of the recovery key is stored. Treat the key returned to the
//! caller as a credential: never include it in logs, metrics or room messages.

#![forbid(unsafe_code)]

use super::{
    jwt, routes, session,
    sessions::lock_current_session,
    types::{AuthError, Claims},
};
use crate::signaling::protocol::{ChatStyle, deserialize_appearance_style};
use crate::signaling::{ClientIp, SignalingServer};
use axum::{
    Extension, Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use base64::{
    Engine as _,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use rand::{TryRng, rngs::SysRng};
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

pub(crate) const MAX_IMAGE_BYTES: usize = 128 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 2048;
const RECOVERY_PREFIX: &str = "sc-recovery-";

#[derive(Serialize, FromRow)]
pub(crate) struct AccountProfile {
    pub id: Uuid,
    pub email: String,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
    #[sqlx(json)]
    pub profile_style: ChatStyle,
    pub recovery_enabled: bool,
}

#[derive(Serialize, FromRow)]
pub(crate) struct PublicProfile {
    pub id: Uuid,
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
    #[sqlx(json)]
    pub profile_style: ChatStyle,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UpdateProfileRequest {
    pub display_name: String,
    pub avatar_url: Option<String>,
    pub bio: String,
    #[serde(deserialize_with = "deserialize_appearance_style")]
    pub profile_style: ChatStyle,
}

/// Chat preferences that follow an account across devices, in the browser's
/// own shape (`Preferences` in web/src/social-chat.ts, camelCase). Desktop
/// notification consent and the chat look stay where they are decided: the
/// device and the profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub(crate) struct ChatPreferences {
    pub allow_private_messages: bool,
    pub sounds: bool,
    pub large_text: bool,
    pub timestamps: String,
    pub ignored: Vec<IgnoredPerson>,
}

impl Default for ChatPreferences {
    fn default() -> Self {
        Self {
            allow_private_messages: true,
            sounds: false,
            large_text: false,
            timestamps: "time".to_string(),
            ignored: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct IgnoredPerson {
    pub id: String,
    pub name: String,
}

fn present<'de, D, T>(deserializer: D) -> Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(deserializer).map(Some)
}

/// An explicit field intent, never a replacement of a client's stale snapshot.
/// Ignore operations are set/remove rather than toggles, so repeating one is safe.
#[derive(Default, Deserialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct PreferencePatch {
    #[serde(deserialize_with = "present")]
    allow_private_messages: Option<bool>,
    #[serde(deserialize_with = "present")]
    sounds: Option<bool>,
    #[serde(deserialize_with = "present")]
    large_text: Option<bool>,
    #[serde(deserialize_with = "present")]
    timestamps: Option<String>,
    #[serde(deserialize_with = "present")]
    ignore: Option<IgnoredPerson>,
    #[serde(deserialize_with = "present")]
    unignore: Option<Uuid>,
}

impl PreferencePatch {
    fn apply(
        self,
        mut saved: ChatPreferences,
        account: Uuid,
    ) -> Result<ChatPreferences, AuthError> {
        if self.ignore.is_some() && self.unignore.is_some() {
            return Err(AuthError::InvalidInput("Choose one ignore operation"));
        }
        if self.allow_private_messages.is_none()
            && self.sounds.is_none()
            && self.large_text.is_none()
            && self.timestamps.is_none()
            && self.ignore.is_none()
            && self.unignore.is_none()
        {
            return Err(AuthError::InvalidInput("Choose a preference to change"));
        }
        if let Some(value) = self.allow_private_messages {
            saved.allow_private_messages = value;
        }
        if let Some(value) = self.sounds {
            saved.sounds = value;
        }
        if let Some(value) = self.large_text {
            saved.large_text = value;
        }
        if let Some(value) = self.timestamps {
            saved.timestamps = value;
        }
        if let Some(person) = self.ignore {
            // Canonical UUIDs prevent spelling variants from evading set semantics.
            let id = Uuid::parse_str(&person.id)
                .map_err(|_| AuthError::InvalidInput("Invalid account to ignore"))?;
            saved.ignored.retain(|old| old.id != id.to_string());
            saved.ignored.push(IgnoredPerson {
                id: id.to_string(),
                name: person.name,
            });
        }
        if let Some(id) = self.unignore {
            if id == account {
                return Err(AuthError::InvalidInput(
                    "Choose another account to unignore",
                ));
            }
            saved.ignored.retain(|person| person.id != id.to_string());
        }
        validate_preferences(&saved, account)?;
        Ok(saved)
    }
}

const TIMESTAMP_FORMATS: [&str; 5] = ["hover", "time", "time12", "seconds", "datetime"];
/// The same ceiling a room applies to a session's ignore list.
const MAX_IGNORED: usize = 100;

/// The browser's validation, repeated: a stored object must be one this build
/// could have written, whatever client sent it.
fn validate_preferences(preferences: &ChatPreferences, own_id: Uuid) -> Result<(), AuthError> {
    if !TIMESTAMP_FORMATS.contains(&preferences.timestamps.as_str()) {
        return Err(AuthError::InvalidInput("Unknown timestamp format"));
    }
    if preferences.ignored.len() > MAX_IGNORED {
        return Err(AuthError::InvalidInput("Ignore list is full (100 people)"));
    }
    let mut seen = std::collections::HashSet::with_capacity(preferences.ignored.len());
    for person in &preferences.ignored {
        let id = Uuid::parse_str(&person.id)
            .map_err(|_| AuthError::InvalidInput("Ignored people are named by account id"))?;
        if id == own_id || !seen.insert(id) {
            return Err(AuthError::InvalidInput(
                "Ignore list repeats a person or names you",
            ));
        }
        if person.name.is_empty() || !validate_text(&person.name, 64, false) {
            return Err(AuthError::InvalidInput(
                "Ignored names must be 1–64 bytes without control characters",
            ));
        }
    }
    Ok(())
}

/// An unreadable saved policy must remain an error rather than turning a
/// private-message opt-out or ignored account list into permissive defaults.
fn stored_preferences(
    value: Option<serde_json::Value>,
    account: Uuid,
) -> Result<ChatPreferences, AuthError> {
    let value = value.ok_or(AuthError::InvalidToken)?;
    decode_chat_preferences(value, account).map_err(routes::database_error)
}

pub(crate) fn decode_chat_preferences(
    value: serde_json::Value,
    account: Uuid,
) -> Result<ChatPreferences, sqlx::Error> {
    let invalid = || {
        sqlx::Error::Decode(Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Saved chat preferences could not be read",
        )))
    };
    let mut preferences: ChatPreferences = serde_json::from_value(value).map_err(|_| invalid())?;
    validate_preferences(&preferences, account).map_err(|_| invalid())?;
    for person in &mut preferences.ignored {
        person.id = Uuid::parse_str(&person.id)
            .map_err(|_| invalid())?
            .to_string();
    }
    Ok(preferences)
}

/// Read current account privacy for room publication and authenticated controls.
pub(crate) async fn load_chat_preferences(
    pool: &PgPool,
    account: Uuid,
) -> Result<ChatPreferences, sqlx::Error> {
    let value = sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1")
        .bind(account)
        .fetch_one(pool)
        .await?;
    decode_chat_preferences(value, account)
}

/// GET /api/auth/preferences
pub(crate) async fn get_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<ChatPreferences>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?.clone();
    let rooms = server.room_manager().clone();
    // A successful explicit refresh also repairs a preceding save's incomplete
    // room synchronization. No account setting is changed by this read.
    let preferences = tokio::spawn(async move {
        let _permit = permit;
        let (id, preferences) = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            current_preferences(&pool, &claims),
        )
        .await
        .map_err(|_| AuthError::ServiceBusy)??;
        rooms
            .refresh_account_preferences(id)
            .await
            .map_err(|_| AuthError::ServiceBusy)?;
        Ok::<_, AuthError>(preferences)
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)??;
    Ok((routes::no_store_headers(), Json(preferences)))
}

/// Merge one explicit intent with current account state under the user row lock.
/// A concurrent appearance change cannot remove a newer ignore or PM opt-out.
pub(crate) async fn patch_preferences(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<PreferencePatch>,
) -> Result<(HeaderMap, Json<ChatPreferences>), AuthError> {
    let permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?.clone();
    let rooms = server.room_manager().clone();
    // Once admitted, both the transaction and publication outlive a lost HTTP
    // response. Retain the permit until this bounded owned operation settles.
    let preferences = tokio::spawn(async move {
        let _permit = permit;
        let write = tokio::time::timeout(
            std::time::Duration::from_secs(15),
            save_preferences(&pool, &claims, request),
        )
        .await
        .unwrap_or(Err(AuthError::ServiceBusy));
        if let Ok((id, _)) = &write {
            rooms
                .account_updates()
                .changed(*id, super::updates::Scope::Preferences, None);
        }
        let account = match &write {
            Ok((id, _)) => Some(*id),
            // A transport/COMMIT failure may have committed. Attempt a fresh
            // publication even though the caller must still reconcile the save.
            Err(AuthError::DatabaseError(_) | AuthError::ServiceBusy) => {
                Uuid::parse_str(&claims.sub).ok()
            }
            Err(_) => None,
        };
        let synchronization = if let Some(account) = account {
            rooms.refresh_account_preferences(account).await
        } else {
            Ok(())
        };
        let (_, preferences) = write?;
        synchronization.map_err(|_| AuthError::PreferencesSynchronizationFailed)?;
        Ok::<_, AuthError>(preferences)
    })
    .await
    .map_err(|_| AuthError::ServiceBusy)??;
    Ok((routes::no_store_headers(), Json(preferences)))
}

pub(crate) fn ensure_unexpired_claim(claims: &Claims) -> Result<(), AuthError> {
    let expiration = i64::try_from(claims.exp).map_err(|_| AuthError::InvalidToken)?;
    if expiration <= chrono::Utc::now().timestamp() {
        return Err(AuthError::TokenExpired);
    }
    Ok(())
}

/// The session row is already locked. Its admission predicate may have been
/// evaluated before a lock wait, so use a fresh clock after the wait and before
/// committing the preference operation.
async fn ensure_preference_session_current(
    transaction: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    id: Uuid,
    claims: &Claims,
) -> Result<(), AuthError> {
    let live: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM sessions
         WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp())",
    )
    .bind(claims.sid)
    .bind(id)
    .fetch_one(&mut **transaction)
    .await
    .map_err(routes::database_error)?;
    if !live {
        return Err(AuthError::InvalidToken);
    }
    ensure_unexpired_claim(claims)
}

async fn current_preferences(
    pool: &PgPool,
    claims: &Claims,
) -> Result<(Uuid, ChatPreferences), AuthError> {
    ensure_unexpired_claim(claims)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let (id, _) = lock_current_session(&mut transaction, claims).await?;
    ensure_preference_session_current(&mut transaction, id, claims).await?;
    let saved = sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1")
        .bind(id)
        .fetch_optional(&mut *transaction)
        .await
        .map_err(routes::database_error)?;
    let preferences = stored_preferences(saved, id)?;
    ensure_preference_session_current(&mut transaction, id, claims).await?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((id, preferences))
}

async fn save_preferences(
    pool: &PgPool,
    claims: &Claims,
    request: PreferencePatch,
) -> Result<(Uuid, ChatPreferences), AuthError> {
    ensure_unexpired_claim(claims)?;
    let mut transaction = pool.begin().await.map_err(routes::database_error)?;
    let (id, _) = lock_current_session(&mut transaction, claims).await?;
    ensure_preference_session_current(&mut transaction, id, claims).await?;
    let saved = sqlx::query_scalar("SELECT preferences FROM users WHERE id=$1")
        .bind(id)
        .fetch_one(&mut *transaction)
        .await
        .map_err(routes::database_error)?;
    let preferences = request.apply(
        decode_chat_preferences(saved, id).map_err(routes::database_error)?,
        id,
    )?;
    let value = serde_json::to_value(&preferences)
        .map_err(|_| AuthError::InvalidInput("Invalid preferences"))?;
    sqlx::query(
        "UPDATE users SET preferences = $2, updated_at = now()
         WHERE id = $1 AND auth_version = $3",
    )
    .bind(id)
    .bind(&value)
    .bind(claims.auth_version)
    .execute(&mut *transaction)
    .await
    .map_err(routes::database_error)?;
    ensure_preference_session_current(&mut transaction, id, claims).await?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((id, preferences))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RecoveryKeyRequest {
    pub current_password: String,
}

#[derive(Serialize)]
pub(crate) struct RecoveryKeyResponse {
    pub recovery_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RedeemRecoveryRequest {
    pub email: String,
    pub recovery_key: String,
    pub new_password: String,
}

/// Resolve an exact `Bearer ` authorization header into currently valid claims.
///
/// Signature and expiry validation alone are insufficient: this also checks the
/// account's persisted authentication version, so a password change or recovery
/// invalidates previously issued access tokens. It neither refreshes a session
/// nor accepts a refresh cookie in place of the header.
///
/// # Errors
/// Returns an [`AuthError`] for missing configuration, absent/malformed/expired
/// credentials, revoked claims or a failed database check. Callers must propagate
/// the failure rather than downgrading an authenticated request to guest access.
pub(crate) async fn authenticated_claims(
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

/// Check a UTF-8 byte limit and reject control characters without trimming text.
///
/// `multiline` permits only newline and tab among control characters. This does
/// not enforce a nonempty value; callers apply field-specific minimum lengths.
pub(crate) fn validate_text(value: &str, maximum: usize, multiline: bool) -> bool {
    value.len() <= maximum
        && !value.chars().any(|character| {
            character.is_control() && !(multiline && matches!(character, '\n' | '\t'))
        })
}

pub(crate) fn image_dimensions(mime: &str, bytes: &[u8]) -> Option<(u32, u32)> {
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
pub(crate) fn validate_image_data_url(value: Option<&str>) -> Result<(), AuthError> {
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
    sqlx::query_as("SELECT id, email, display_name, avatar_url, bio, profile_style, recovery_key_hash IS NOT NULL AS recovery_enabled FROM users WHERE id = $1")
        .bind(id).fetch_optional(pool).await.map_err(routes::database_error)?.ok_or(AuthError::UserNotFound)
}

pub(crate) async fn get_profile(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<AccountProfile>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    let profile = load_profile(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?,
    )
    .await?;
    Ok((routes::no_store_headers(), Json(profile)))
}

pub(crate) async fn update_profile(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<UpdateProfileRequest>,
) -> Result<(HeaderMap, Json<AccountProfile>), AuthError> {
    let _permit = routes::acquire_account_request(&server)?;
    let claims = authenticated_claims(&server, &headers).await?;
    routes::validate_display_name(&request.display_name).map_err(|_| profile_error())?;
    if !validate_text(&request.bio, 1024, true) {
        return Err(profile_error());
    }
    validate_image_data_url(request.avatar_url.as_deref())?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let id = Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken)?;
    let profile: AccountProfile = sqlx::query_as(
        "UPDATE users SET display_name = $2, avatar_url = $3, bio = $4, profile_style = $6, updated_at = now()
         WHERE id = $1 AND auth_version = $5
         RETURNING id, email, display_name, avatar_url, bio, profile_style, recovery_key_hash IS NOT NULL AS recovery_enabled",
    )
    .bind(id).bind(request.display_name.trim()).bind(request.avatar_url).bind(request.bio).bind(claims.auth_version).bind(sqlx::types::Json(request.profile_style))
    .fetch_optional(pool).await.map_err(routes::database_error)?
    .ok_or(AuthError::InvalidToken)?;
    Ok((routes::no_store_headers(), Json(profile)))
}

pub(crate) async fn public_profile(
    State(server): State<SignalingServer>,
    Path(id): Path<Uuid>,
) -> Result<(HeaderMap, Json<PublicProfile>), AuthError> {
    let _permit = server.acquire_profile_request().await?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let profile = sqlx::query_as(
        "SELECT id, display_name, avatar_url, bio, profile_style FROM users WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map_err(routes::database_error)?
    .ok_or(AuthError::UserNotFound)?;
    Ok((routes::no_store_headers(), Json(profile)))
}

pub(super) async fn verified_password_hash(
    server: &SignalingServer,
    claims: &Claims,
    password: String,
) -> Result<String, AuthError> {
    if password.len() > routes::MAX_PASSWORD_BYTES {
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

/// Change a password after verifying the account's current password and claims.
///
/// The conditional update prevents a concurrent credential change from being
/// overwritten. Password replacement, authentication-version increment and
/// refresh-session deletion commit atomically. Afterwards, connected account
/// sessions are notified and the response clears the caller's refresh cookie.
/// Success is HTTP 204 with no JSON body; the caller must sign in again.
/// Existing passkeys and an unused recovery key are not replaced by this route.
///
/// # Errors and retries
/// Validation, credential, capacity and database failures return [`AuthError`].
/// A lost response does not prove the transaction failed: retrying with the old
/// password or token after a successful change fails authentication. Clients
/// should attempt normal sign-in with the new password, not blindly replay this
/// mutation. Password hashing runs in the bounded password-work lane.
pub(crate) async fn change_password(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<ChangePasswordRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    routes::validate_new_password(&request.new_password)?;
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
    crate::room::invites::revoke_issued_invites(&mut *transaction, id)
        .await
        .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    server.revoke_account_sessions(claims.sub, claims.auth_version + 1);
    Ok((
        routes::clear_refresh_cookie_headers(),
        StatusCode::NO_CONTENT,
    ))
}

pub(super) fn generate_recovery_key() -> Result<String, AuthError> {
    let mut secret = [0u8; 32];
    SysRng.try_fill_bytes(&mut secret).map_err(|error| {
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

/// Replace the account's recovery key after verifying its current password.
///
/// The response exposes the new credential once, with `Cache-Control: no-store`;
/// only its digest is persisted. Creating a key invalidates the preceding key
/// but does not revoke existing sessions or change the password. A passkey-only
/// account without a password cannot use this password-authorized operation.
///
/// # Errors and retries
/// Returns [`AuthError`] for failed authentication, password verification,
/// admission, randomness or persistence. A successful update may outlive a lost
/// response. There is no read-back endpoint: a subsequent successful request
/// creates a different key and invalidates the inaccessible one. Clients must
/// not claim that an earlier key still works when replacement is unconfirmed.
pub(crate) async fn create_recovery_key(
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
    crate::room::invites::revoke_issued_invites(&mut *transaction, id)
        .await
        .map_err(routes::database_error)?;
    transaction.commit().await.map_err(routes::database_error)?;
    Ok((id, version))
}

/// Consume a saved recovery key and set a new password without a login session.
///
/// The email address selects the account; it is not verified by this operation.
/// The key's digest is matched in a conditional transactional update, so a key
/// can succeed only once even if requests overlap. The same commit clears the
/// recovery digest, increments the authentication version and deletes refresh
/// sessions. Connected account sessions are then notified of revocation.
/// Success is HTTP 204, clears the caller's refresh cookie and issues no tokens;
/// the user must sign in and create a new recovery key if desired.
///
/// # Errors and retries
/// Invalid, replaced or already-consumed keys fail with [`AuthError`], as do
/// validation, capacity and database failures. A lost success response cannot
/// be recovered by redeeming the key again. The client should attempt sign-in
/// using the new password rather than assuming that an unconfirmed reset failed.
pub(crate) async fn redeem_recovery(
    State(server): State<SignalingServer>,
    Extension(ClientIp(source_ip)): Extension<ClientIp>,
    Json(request): Json<RedeemRecoveryRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let email = routes::canonicalize_email(&request.email)?;
    // A guessed key is a failed sign-in: the same growing delay as a password.
    if let Some(wait) = server.sign_in_wait(source_ip, &email) {
        return Err(AuthError::TooManyFailures(wait));
    }
    routes::validate_new_password(&request.new_password)?;
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
        server.note_sign_in_failure(source_ip, &email);
        return Err(AuthError::InvalidCredentials);
    }
    let permit = server
        .try_acquire_password_work()
        .ok_or(AuthError::ServiceBusy)?;
    let password_hash = routes::hash_password_async(request.new_password, permit).await?;
    let (id, version) = consume_recovery(pool, &email, &hash, &password_hash).await?;
    server.note_sign_in_success(source_ip, &email);
    server.revoke_account_sessions(id.to_string(), version);
    Ok((
        routes::clear_refresh_cookie_headers(),
        StatusCode::NO_CONTENT,
    ))
}

#[cfg(test)]
#[path = "account_tests.rs"]
mod tests;
