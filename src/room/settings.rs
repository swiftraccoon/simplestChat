#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSettings {
    pub id: String,
    pub owner_id: Uuid,
    pub display_name: String,
    pub password_protected: bool,
    pub require_registration: bool,
    pub max_participants: Option<i32>,
    pub max_broadcasters: Option<i32>,
    pub allow_screen_sharing: bool,
    pub allow_chat: bool,
    pub allow_video: bool,
    pub moderated: bool,
    pub invite_only: bool,
    pub secret: bool,
    pub lobby_enabled: bool,
    pub push_to_talk: bool,
    pub guests_allowed: bool,
    pub guests_can_broadcast: bool,
    pub topic: Option<String>,
}

#[derive(FromRow)]
struct RoomRow {
    id: String,
    owner_id: Uuid,
    display_name: String,
    password_hash: Option<String>,
    require_registration: bool,
    max_participants: Option<i32>,
    max_broadcasters: Option<i32>,
    allow_screen_sharing: bool,
    allow_chat: bool,
    allow_video: bool,
    moderated: bool,
    invite_only: bool,
    secret: bool,
    lobby_enabled: bool,
    push_to_talk: bool,
    guests_allowed: bool,
    guests_can_broadcast: bool,
    topic: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateRoomRequest {
    pub id: String,
    pub display_name: String,
    pub password: Option<String>,
    pub require_registration: Option<bool>,
    pub max_participants: Option<i32>,
    pub max_broadcasters: Option<i32>,
    pub moderated: Option<bool>,
    pub secret: Option<bool>,
    pub lobby_enabled: Option<bool>,
    pub guests_allowed: Option<bool>,
    pub guests_can_broadcast: Option<bool>,
    pub topic: Option<String>,
}

pub const MAX_ROOM_ID_LEN: usize = 128;
pub const MAX_DISPLAY_NAME_LEN: usize = 128;
pub const MAX_TOPIC_LEN: usize = 512;
pub const MIN_PASSWORD_LEN: usize = 8;
pub const MAX_PASSWORD_LEN: usize = 256;
pub const MAX_PARTICIPANTS: i32 = 10_000;
pub const MAX_BROADCASTERS: i32 = 1_000;
pub const MAX_PERSISTED_ROOMS_PER_OWNER: i64 = 100;

const PERSISTED_ROOM_QUOTA_ERROR: &str = "persisted-room owner quota reached";
const PERSISTED_ROOM_GLOBAL_QUOTA_ERROR: &str = "persisted-room global quota reached";
const GLOBAL_ROOM_CREATION_LOCK: i64 = 7_349_872_340_910;

/// The surrounding room manager deliberately exposes `sqlx::Error`, so use a
/// private, recognizable error value to preserve that API while allowing the
/// HTTP layer to distinguish an account quota from a database outage.
pub fn is_persisted_room_quota_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::InvalidArgument(message) if message == PERSISTED_ROOM_QUOTA_ERROR
    )
}

pub fn is_global_persisted_room_quota_error(error: &sqlx::Error) -> bool {
    matches!(
        error,
        sqlx::Error::InvalidArgument(message) if message == PERSISTED_ROOM_GLOBAL_QUOTA_ERROR
    )
}

pub fn valid_room_id(room_id: &str) -> bool {
    !room_id.is_empty()
        && room_id.len() <= MAX_ROOM_ID_LEN
        && room_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

pub fn valid_new_room_password(password: &str) -> bool {
    (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len())
        && !password.chars().any(char::is_control)
}

pub fn validate_create_request(request: &CreateRoomRequest) -> Result<(), &'static str> {
    if !valid_room_id(&request.id) {
        return Err("Room ID must use 1-128 letters, numbers, hyphens, or underscores");
    }
    if request.display_name.trim().is_empty()
        || request.display_name.len() > MAX_DISPLAY_NAME_LEN
        || request.display_name.chars().any(char::is_control)
    {
        return Err("Display name must be 1-128 characters without control characters");
    }
    if request
        .password
        .as_ref()
        .is_some_and(|password| !valid_new_room_password(password))
    {
        return Err("Password must be 8-256 bytes without control characters");
    }
    if request
        .topic
        .as_ref()
        .is_some_and(|topic| topic.len() > MAX_TOPIC_LEN || topic.chars().any(char::is_control))
    {
        return Err("Topic must be at most 512 characters without control characters");
    }
    if request
        .max_participants
        .is_some_and(|value| !(1..=MAX_PARTICIPANTS).contains(&value))
    {
        return Err("Maximum participants must be between 1 and 10000");
    }
    if request
        .max_broadcasters
        .is_some_and(|value| !(1..=MAX_BROADCASTERS).contains(&value))
    {
        return Err("Maximum broadcasters must be between 1 and 1000");
    }
    if matches!(
        (request.max_broadcasters, request.max_participants),
        (Some(broadcasters), Some(participants)) if broadcasters > participants
    ) {
        return Err("Maximum broadcasters cannot exceed maximum participants");
    }
    Ok(())
}

pub async fn load_room(
    pool: &PgPool,
    room_id: &str,
) -> Result<Option<(RoomSettings, Option<String>)>, sqlx::Error> {
    let row = sqlx::query_as::<_, RoomRow>(
        "SELECT id, owner_id, display_name, password_hash, require_registration,
                max_participants, max_broadcasters, allow_screen_sharing, allow_chat, allow_video,
                moderated, invite_only, secret, lobby_enabled, push_to_talk,
                guests_allowed, guests_can_broadcast, topic
         FROM rooms WHERE id = $1",
    )
    .bind(room_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| {
        let password_hash = r.password_hash;
        (
            RoomSettings {
                id: r.id,
                owner_id: r.owner_id,
                display_name: r.display_name,
                password_protected: password_hash.is_some(),
                require_registration: r.require_registration,
                max_participants: r.max_participants,
                max_broadcasters: r.max_broadcasters,
                allow_screen_sharing: r.allow_screen_sharing,
                allow_chat: r.allow_chat,
                allow_video: r.allow_video,
                moderated: r.moderated,
                invite_only: r.invite_only,
                secret: r.secret,
                lobby_enabled: r.lobby_enabled,
                push_to_talk: r.push_to_talk,
                guests_allowed: r.guests_allowed,
                guests_can_broadcast: r.guests_can_broadcast,
                topic: r.topic,
            },
            password_hash,
        )
    }))
}

pub async fn create_room(
    pool: &PgPool,
    owner_id: &Uuid,
    req: &CreateRoomRequest,
    password_hash: Option<&str>,
    max_persisted_rooms: i64,
) -> Result<RoomSettings, sqlx::Error> {
    let mut transaction = pool.begin().await?;

    // Serialize the global count across every application instance, then take
    // the owner lock in the same order on every create path.
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(GLOBAL_ROOM_CREATION_LOCK)
        .execute(&mut *transaction)
        .await?;
    let persisted_room_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rooms")
        .fetch_one(&mut *transaction)
        .await?;
    if persisted_room_count >= max_persisted_rooms {
        transaction.rollback().await?;
        return Err(sqlx::Error::InvalidArgument(
            PERSISTED_ROOM_GLOBAL_QUOTA_ERROR.to_string(),
        ));
    }

    // Serialize the per-owner count-and-insert sequence. A hash collision only
    // causes harmless extra serialization.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1::uuid::text, 0))")
        .bind(owner_id)
        .execute(&mut *transaction)
        .await?;

    let owned_room_count: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM rooms WHERE owner_id = $1")
            .bind(owner_id)
            .fetch_one(&mut *transaction)
            .await?;
    if owned_room_count >= MAX_PERSISTED_ROOMS_PER_OWNER {
        transaction.rollback().await?;
        return Err(sqlx::Error::InvalidArgument(
            PERSISTED_ROOM_QUOTA_ERROR.to_string(),
        ));
    }

    sqlx::query(
        "INSERT INTO rooms (id, owner_id, display_name, password_hash, require_registration,
                           max_participants, max_broadcasters, moderated, secret, lobby_enabled,
                           guests_allowed, guests_can_broadcast, topic)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
    )
    .bind(&req.id)
    .bind(owner_id)
    .bind(&req.display_name)
    .bind(password_hash)
    .bind(req.require_registration.unwrap_or(false))
    .bind(req.max_participants)
    .bind(req.max_broadcasters)
    .bind(req.moderated.unwrap_or(false))
    .bind(req.secret.unwrap_or(false))
    .bind(req.lobby_enabled.unwrap_or(false))
    .bind(req.guests_allowed.unwrap_or(true))
    .bind(req.guests_can_broadcast.unwrap_or(true))
    .bind(&req.topic)
    .execute(&mut *transaction)
    .await?;

    transaction.commit().await?;

    Ok(RoomSettings {
        id: req.id.clone(),
        owner_id: *owner_id,
        display_name: req.display_name.clone(),
        password_protected: password_hash.is_some(),
        require_registration: req.require_registration.unwrap_or(false),
        max_participants: req.max_participants,
        max_broadcasters: req.max_broadcasters,
        allow_screen_sharing: true,
        allow_chat: true,
        allow_video: true,
        moderated: req.moderated.unwrap_or(false),
        invite_only: false,
        secret: req.secret.unwrap_or(false),
        lobby_enabled: req.lobby_enabled.unwrap_or(false),
        push_to_talk: false,
        guests_allowed: req.guests_allowed.unwrap_or(true),
        guests_can_broadcast: req.guests_can_broadcast.unwrap_or(true),
        topic: req.topic.clone(),
    })
}

/// Apply partial updates to an in-memory RoomSettings struct.
/// Only fields that are `Some` are updated; `None` fields are left unchanged.
#[allow(clippy::too_many_arguments)]
pub fn apply_settings_update(
    settings: &mut RoomSettings,
    moderated: Option<bool>,
    lobby_enabled: Option<bool>,
    guests_allowed: Option<bool>,
    guests_can_broadcast: Option<bool>,
    max_broadcasters: Option<Option<i32>>,
    max_participants: Option<Option<i32>>,
    allow_screen_sharing: Option<bool>,
    allow_chat: Option<bool>,
    allow_video: Option<bool>,
    require_registration: Option<bool>,
    invite_only: Option<bool>,
    push_to_talk: Option<bool>,
    secret: Option<bool>,
    password: Option<Option<String>>,
) {
    if let Some(v) = moderated {
        settings.moderated = v;
    }
    if let Some(v) = lobby_enabled {
        settings.lobby_enabled = v;
    }
    if let Some(v) = guests_allowed {
        settings.guests_allowed = v;
    }
    if let Some(v) = guests_can_broadcast {
        settings.guests_can_broadcast = v;
    }
    if let Some(v) = max_broadcasters {
        settings.max_broadcasters = v;
    }
    if let Some(v) = max_participants {
        settings.max_participants = v;
    }
    if let Some(v) = allow_screen_sharing {
        settings.allow_screen_sharing = v;
    }
    if let Some(v) = allow_chat {
        settings.allow_chat = v;
    }
    if let Some(v) = allow_video {
        settings.allow_video = v;
    }
    if let Some(v) = require_registration {
        settings.require_registration = v;
    }
    if let Some(v) = invite_only {
        settings.invite_only = v;
    }
    if let Some(v) = push_to_talk {
        settings.push_to_talk = v;
    }
    if let Some(v) = secret {
        settings.secret = v;
    }
    if let Some(v) = password {
        settings.password_protected = v.is_some();
        // Don't store actual password in RoomSettings — it goes to DB only
    }
}

/// Persist partial room settings updates to the database.
/// Only updates columns whose corresponding parameter is `Some`.
#[allow(clippy::too_many_arguments)]
pub async fn update_room_settings(
    pool: &PgPool,
    room_id: &str,
    moderated: Option<bool>,
    lobby_enabled: Option<bool>,
    guests_allowed: Option<bool>,
    guests_can_broadcast: Option<bool>,
    max_broadcasters: Option<Option<i32>>,
    max_participants: Option<Option<i32>>,
    allow_screen_sharing: Option<bool>,
    allow_chat: Option<bool>,
    allow_video: Option<bool>,
    require_registration: Option<bool>,
    invite_only: Option<bool>,
    push_to_talk: Option<bool>,
    secret: Option<bool>,
    password_hash: Option<Option<String>>,
) -> Result<(), sqlx::Error> {
    // Build a dynamic SET clause for only the provided fields
    let mut set_parts: Vec<String> = Vec::new();
    let mut param_idx: usize = 2; // $1 is room_id

    macro_rules! maybe_add {
        ($opt:expr, $col:expr) => {
            if $opt.is_some() {
                set_parts.push(format!("{} = ${}", $col, param_idx));
                param_idx += 1;
            }
        };
    }

    maybe_add!(moderated, "moderated");
    maybe_add!(lobby_enabled, "lobby_enabled");
    maybe_add!(guests_allowed, "guests_allowed");
    maybe_add!(guests_can_broadcast, "guests_can_broadcast");
    maybe_add!(max_broadcasters, "max_broadcasters");
    maybe_add!(max_participants, "max_participants");
    maybe_add!(allow_screen_sharing, "allow_screen_sharing");
    maybe_add!(allow_chat, "allow_chat");
    maybe_add!(allow_video, "allow_video");
    maybe_add!(require_registration, "require_registration");
    maybe_add!(invite_only, "invite_only");
    maybe_add!(push_to_talk, "push_to_talk");
    maybe_add!(secret, "secret");
    maybe_add!(password_hash, "password_hash");

    if set_parts.is_empty() {
        return Ok(()); // Nothing to update
    }

    let sql = format!("UPDATE rooms SET {} WHERE id = $1", set_parts.join(", "));
    // Only fixed column names and placeholder numbers enter this SQL string;
    // every caller-supplied value remains a separate bound parameter.
    let mut query = sqlx::query(sqlx::AssertSqlSafe(sql)).bind(room_id);

    // Bind values in the same order as the SET parts
    if let Some(v) = moderated {
        query = query.bind(v);
    }
    if let Some(v) = lobby_enabled {
        query = query.bind(v);
    }
    if let Some(v) = guests_allowed {
        query = query.bind(v);
    }
    if let Some(v) = guests_can_broadcast {
        query = query.bind(v);
    }
    if let Some(v) = max_broadcasters {
        query = query.bind(v);
    }
    if let Some(v) = max_participants {
        query = query.bind(v);
    }
    if let Some(v) = allow_screen_sharing {
        query = query.bind(v);
    }
    if let Some(v) = allow_chat {
        query = query.bind(v);
    }
    if let Some(v) = allow_video {
        query = query.bind(v);
    }
    if let Some(v) = require_registration {
        query = query.bind(v);
    }
    if let Some(v) = invite_only {
        query = query.bind(v);
    }
    if let Some(v) = push_to_talk {
        query = query.bind(v);
    }
    if let Some(v) = secret {
        query = query.bind(v);
    }
    if let Some(v) = password_hash {
        query = query.bind(v);
    }

    let result = query.execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound);
    }
    Ok(())
}

pub async fn delete_room(
    pool: &PgPool,
    room_id: &str,
    owner_id: &Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM rooms WHERE id = $1 AND owner_id = $2")
        .bind(room_id)
        .bind(owner_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The web client reads camelCase keys from the roomJoined/roomSettingsChanged
    /// payloads — this guards against snake_case regressions in the wire format.
    #[test]
    fn room_settings_serialize_camel_case() {
        let settings = RoomSettings {
            id: "room-1".to_string(),
            owner_id: Uuid::nil(),
            display_name: "Room One".to_string(),
            password_protected: false,
            require_registration: false,
            max_participants: Some(10),
            max_broadcasters: None,
            allow_screen_sharing: true,
            allow_chat: true,
            allow_video: true,
            moderated: true,
            invite_only: false,
            secret: false,
            lobby_enabled: true,
            push_to_talk: false,
            guests_allowed: true,
            guests_can_broadcast: true,
            topic: Some("hello".to_string()),
        };
        let value = serde_json::to_value(&settings).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "displayName",
            "passwordProtected",
            "requireRegistration",
            "maxParticipants",
            "maxBroadcasters",
            "allowScreenSharing",
            "allowChat",
            "allowVideo",
            "inviteOnly",
            "lobbyEnabled",
            "pushToTalk",
            "guestsAllowed",
            "guestsCanBroadcast",
        ] {
            assert!(obj.contains_key(key), "missing camelCase key: {key}");
        }
        assert!(
            !obj.contains_key("lobby_enabled"),
            "snake_case key leaked into wire format"
        );
    }

    #[test]
    fn create_request_validation_rejects_unsafe_and_invalid_ranges() {
        let mut request = CreateRoomRequest {
            id: "safe-room_1".to_string(),
            display_name: "Safe Room".to_string(),
            password: None,
            require_registration: None,
            max_participants: Some(10),
            max_broadcasters: Some(5),
            moderated: None,
            secret: None,
            lobby_enabled: None,
            guests_allowed: None,
            guests_can_broadcast: None,
            topic: None,
        };
        assert!(validate_create_request(&request).is_ok());

        request.id = "../room".to_string();
        assert!(validate_create_request(&request).is_err());
        request.id = "safe-room".to_string();
        request.max_participants = Some(0);
        assert!(validate_create_request(&request).is_err());
        request.max_participants = Some(2);
        request.max_broadcasters = Some(3);
        assert!(validate_create_request(&request).is_err());

        request.max_broadcasters = Some(2);
        request.topic = Some("x".repeat(MAX_TOPIC_LEN + 1));
        assert!(validate_create_request(&request).is_err());

        request.topic = None;
        request.password = Some("1234567".to_string());
        assert_eq!(
            validate_create_request(&request),
            Err("Password must be 8-256 bytes without control characters")
        );
        request.password = Some("12345678".to_string());
        assert!(validate_create_request(&request).is_ok());
        request.password = Some("éééé".to_string());
        assert_eq!(request.password.as_ref().unwrap().len(), MIN_PASSWORD_LEN);
        assert!(validate_create_request(&request).is_ok());
        request.password = Some("password\n".to_string());
        assert!(validate_create_request(&request).is_err());
    }

    #[test]
    fn persisted_room_quota_error_is_distinguishable() {
        let quota_error = sqlx::Error::InvalidArgument(PERSISTED_ROOM_QUOTA_ERROR.to_string());
        assert!(is_persisted_room_quota_error(&quota_error));
        assert!(!is_global_persisted_room_quota_error(&quota_error));

        let global_error =
            sqlx::Error::InvalidArgument(PERSISTED_ROOM_GLOBAL_QUOTA_ERROR.to_string());
        assert!(is_global_persisted_room_quota_error(&global_error));
        assert!(!is_persisted_room_quota_error(&global_error));
        assert!(!is_persisted_room_quota_error(&sqlx::Error::PoolClosed));
        assert_eq!(MAX_PERSISTED_ROOMS_PER_OWNER, 100);
    }
}
