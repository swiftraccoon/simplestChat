#![forbid(unsafe_code)]

use crate::ids::RoomId;
use crate::room::refusal::{Refusal, RoomError};
use crate::signaling::protocol::ChatStyle;
use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoomSettings {
    pub id: String,
    /// Never written to the wire: participants learn the owner by role, not by
    /// account id. Read when present, because the administration path rebuilds
    /// the settings from a JSON row that carries it.
    #[serde(skip_serializing, default)]
    pub owner_id: Uuid,
    pub display_name: String,
    pub password_protected: bool,
    pub require_registration: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_participants: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub topic: Option<String>,
    pub name_style: ChatStyle,
    pub topic_style: ChatStyle,
    pub history_retention_days: i32,
}

impl RoomSettings {
    /// The settings of a room that has none stored: open, unmoderated and unowned.
    pub fn defaults(room_id: &RoomId) -> Self {
        Self {
            id: room_id.to_string(),
            owner_id: Uuid::nil(),
            display_name: room_id.to_string(),
            name_style: ChatStyle::default(),
            topic_style: ChatStyle::default(),
            history_retention_days: 0,
            password_protected: false,
            require_registration: false,
            max_participants: None,
            max_broadcasters: None,
            allow_screen_sharing: true,
            allow_chat: true,
            allow_video: true,
            moderated: false,
            invite_only: false,
            secret: false,
            lobby_enabled: false,
            push_to_talk: false,
            guests_allowed: true,
            guests_can_broadcast: true,
            topic: None,
        }
    }
}

/// A partial settings change. An omitted field keeps its value; for the
/// ceilings and the password `Some(None)` clears it. The socket and the HTTP
/// administration API carry it, the room manager validates it, and the
/// persistence layer binds only the fields it names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moderated: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lobby_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_allowed: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guests_can_broadcast: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_broadcasters: Option<Option<i32>>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub max_participants: Option<Option<i32>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_screen_sharing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_chat: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_video: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_registration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push_to_talk: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<bool>,
    #[serde(
        default,
        deserialize_with = "nullable",
        skip_serializing_if = "Option::is_none"
    )]
    pub password: Option<Option<crate::secret::SecretString>>,
}

/// Reads an explicit JSON `null` as `Some(None)` (clear), so a missing key stays `None` (keep).
fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
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
    #[sqlx(json)]
    name_style: ChatStyle,
    #[sqlx(json)]
    topic_style: ChatStyle,
    history_retention_days: i32,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct CreateRoomRequest {
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

pub(crate) const MAX_DISPLAY_NAME_LEN: usize = 128;
pub(crate) const MAX_TOPIC_LEN: usize = 512;
pub(crate) const MIN_PASSWORD_LEN: usize = 8;
pub(crate) const MAX_PASSWORD_LEN: usize = 256;
pub(crate) const MAX_PARTICIPANTS: i32 = 10_000;
pub(crate) const MAX_BROADCASTERS: i32 = 1_000;
pub(crate) const MAX_PERSISTED_ROOMS_PER_OWNER: i64 = 100;

const GLOBAL_ROOM_CREATION_LOCK: i64 = 7_349_872_340_910;

/// The surrounding room manager deliberately exposes `sqlx::Error`, so use a
/// private, recognizable error value to preserve that API while allowing the
/// HTTP layer to distinguish an account quota from a database outage.
pub(crate) fn valid_room_id(room_id: &str) -> bool {
    RoomId::new(room_id).is_ok()
}

pub(crate) fn valid_new_room_password(password: &str) -> bool {
    (MIN_PASSWORD_LEN..=MAX_PASSWORD_LEN).contains(&password.len())
        && !password.chars().any(char::is_control)
}

pub(crate) fn validate_create_request(request: &CreateRoomRequest) -> Result<(), &'static str> {
    if !valid_room_id(&request.id) {
        return Err("Room ID must use 1-128 letters, numbers, hyphens, or underscores");
    }
    if request.display_name.trim().is_empty()
        || request.display_name.len() > MAX_DISPLAY_NAME_LEN
        || !crate::labels::is_plain(&request.display_name)
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

/// The stored settings and password hash of a room, from a pool or from inside a
/// transaction that already holds the row's locks.
pub(crate) async fn load_room<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    room_id: &RoomId,
) -> Result<Option<(RoomSettings, Option<String>)>, RoomError> {
    let row = sqlx::query_as::<_, RoomRow>(
        "SELECT id, owner_id, display_name, password_hash, require_registration,
                max_participants, max_broadcasters, allow_screen_sharing, allow_chat, allow_video,
                moderated, invite_only, secret, lobby_enabled, push_to_talk,
                guests_allowed, guests_can_broadcast, topic, name_style, topic_style, history_retention_days::integer AS history_retention_days
         FROM rooms WHERE id = $1",
    )
    .bind(room_id)
    .fetch_optional(executor)
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
                name_style: r.name_style,
                topic_style: r.topic_style,
                history_retention_days: r.history_retention_days,
            },
            password_hash,
        )
    }))
}

pub(crate) async fn create_room(
    pool: &PgPool,
    owner_id: &Uuid,
    req: &CreateRoomRequest,
    password_hash: Option<&str>,
    max_persisted_rooms: i64,
) -> Result<RoomSettings, RoomError> {
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
        return Err(Refusal::Unavailable("Persisted room capacity has been reached".into()).into());
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
        return Err(Refusal::Conflict(
            format!("Persisted room quota reached (maximum {MAX_PERSISTED_ROOMS_PER_OWNER})")
                .into(),
        )
        .into());
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
        name_style: ChatStyle::default(),
        topic_style: ChatStyle::default(),
        history_retention_days: 0,
    })
}

/// Apply a validated patch to the in-memory projection of room settings.
///
/// Ordinary `Option<bool>` fields leave the current value unchanged for `None`
/// and replace it for `Some(value)`. Nullable limits and the password have three
/// states matching the wire contract:
///
/// | Rust value | JSON patch | Effect |
/// | --- | --- | --- |
/// | `None` | field omitted | Preserve the current value |
/// | `Some(None)` | `null` | Remove the room-specific limit or password |
/// | `Some(Some(value))` | an explicit value | Replace the current value |
///
/// A zero limit is not the same as removing a limit. This helper neither checks
/// authorization/ranges nor persists changes; the caller must validate the
/// request and coordinate it with the database and current room generation.
/// Password input updates only `password_protected`; the plaintext is never
/// stored in [`RoomSettings`]. Database persistence receives a hash separately.
pub(crate) fn apply_settings_update(settings: &mut RoomSettings, patch: &SettingsPatch) {
    if let Some(v) = patch.moderated {
        settings.moderated = v;
    }
    if let Some(v) = patch.lobby_enabled {
        settings.lobby_enabled = v;
    }
    if let Some(v) = patch.guests_allowed {
        settings.guests_allowed = v;
    }
    if let Some(v) = patch.guests_can_broadcast {
        settings.guests_can_broadcast = v;
    }
    if let Some(v) = patch.max_broadcasters {
        settings.max_broadcasters = v;
    }
    if let Some(v) = patch.max_participants {
        settings.max_participants = v;
    }
    if let Some(v) = patch.allow_screen_sharing {
        settings.allow_screen_sharing = v;
    }
    if let Some(v) = patch.allow_chat {
        settings.allow_chat = v;
    }
    if let Some(v) = patch.allow_video {
        settings.allow_video = v;
    }
    if let Some(v) = patch.require_registration {
        settings.require_registration = v;
    }
    if let Some(v) = patch.invite_only {
        settings.invite_only = v;
    }
    if let Some(v) = patch.push_to_talk {
        settings.push_to_talk = v;
    }
    if let Some(v) = patch.secret {
        settings.secret = v;
    }
    if let Some(v) = &patch.password {
        settings.password_protected = v.is_some();
        // Don't store actual password in RoomSettings — it goes to DB only
    }
}

/// Persist selected room-setting columns in one parameter-bound SQL update.
///
/// Only parameters with an outer `Some` enter the update. For nullable limits
/// and `password_hash`, `None` preserves the column, `Some(None)` writes SQL
/// `NULL`, and `Some(Some(value))` replaces it. Pass a prepared password **hash**,
/// never plaintext. If every parameter is omitted, no query is executed.
///
/// A nonempty update must affect exactly one row; otherwise this returns
/// `sqlx::Error::RowNotFound`. This helper does not authorize, validate limits or
/// update live room state. The owning room operation must serialize persistence
/// with the appropriate runtime generation and enforce those preconditions.
///
/// # Errors
/// Returns the database error if the update fails. It does not open a transaction
/// with other mutations or provide a retry/idempotency token; callers must not
/// infer that an interrupted response means the database remained unchanged.
pub(crate) async fn update_room_settings(
    pool: &PgPool,
    room_id: &RoomId,
    patch: &SettingsPatch,
    password_hash: Option<Option<String>>,
) -> Result<(), RoomError> {
    // One assignment per provided field; the builder numbers the parameters, so
    // the column and its value can never drift apart.
    let mut builder = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE rooms SET ");
    let mut assignments = builder.separated(", ");
    let mut changed = false;
    macro_rules! assign {
        ($value:expr, $column:literal) => {
            if let Some(value) = $value {
                assignments
                    .push(concat!($column, " = "))
                    .push_bind_unseparated(value);
                changed = true;
            }
        };
    }
    assign!(patch.moderated, "moderated");
    assign!(patch.lobby_enabled, "lobby_enabled");
    assign!(patch.guests_allowed, "guests_allowed");
    assign!(patch.guests_can_broadcast, "guests_can_broadcast");
    assign!(patch.max_broadcasters, "max_broadcasters");
    assign!(patch.max_participants, "max_participants");
    assign!(patch.allow_screen_sharing, "allow_screen_sharing");
    assign!(patch.allow_chat, "allow_chat");
    assign!(patch.allow_video, "allow_video");
    assign!(patch.require_registration, "require_registration");
    assign!(patch.invite_only, "invite_only");
    assign!(patch.push_to_talk, "push_to_talk");
    assign!(patch.secret, "secret");
    assign!(password_hash, "password_hash");
    if !changed {
        return Ok(()); // Nothing to update
    }
    assignments.push("updated_at = now()");
    builder.push(" WHERE id = ").push_bind(room_id);

    let result = builder.build().execute(pool).await?;
    if result.rows_affected() != 1 {
        return Err(sqlx::Error::RowNotFound.into());
    }
    Ok(())
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
            name_style: ChatStyle::default(),
            topic_style: ChatStyle::default(),
            history_retention_days: 0,
        };
        let value = serde_json::to_value(&settings).unwrap();
        let obj = value.as_object().unwrap();
        for key in [
            "displayName",
            "nameStyle",
            "topicStyle",
            "passwordProtected",
            "requireRegistration",
            "maxParticipants",
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
        // An absent ceiling or topic is omitted, never null: the client reads `?:` fields.
        assert!(!obj.contains_key("maxBroadcasters"));
        assert_eq!(obj["topic"], "hello");
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

    /// The owner id leaves with no payload but survives a JSON round-trip that carries it,
    /// which is how the administration transaction loads the row it patches.
    #[test]
    fn owner_id_is_read_but_never_written() {
        let owner = Uuid::new_v4();
        let mut settings = RoomSettings::defaults(&"owned".parse().unwrap());
        settings.owner_id = owner;
        let mut value = serde_json::to_value(&settings).unwrap();
        assert!(value.get("ownerId").is_none());
        let anonymous: RoomSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(anonymous.owner_id, Uuid::nil());
        value["ownerId"] = serde_json::json!(owner);
        let loaded: RoomSettings = serde_json::from_value(value).unwrap();
        assert_eq!(loaded.owner_id, owner);
    }
}
