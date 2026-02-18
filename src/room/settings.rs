#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};
use sqlx::{FromRow, PgPool};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
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

pub async fn load_room(pool: &PgPool, room_id: &str) -> Result<Option<RoomSettings>, sqlx::Error> {
    let row = sqlx::query_as::<_, RoomRow>(
        "SELECT id, owner_id, display_name, password_hash, require_registration,
                max_participants, max_broadcasters, allow_screen_sharing, allow_chat, allow_video,
                moderated, invite_only, secret, lobby_enabled, push_to_talk,
                guests_allowed, guests_can_broadcast, topic
         FROM rooms WHERE id = $1"
    )
    .bind(room_id)
    .fetch_optional(pool)
    .await?;

    Ok(row.map(|r| RoomSettings {
        id: r.id,
        owner_id: r.owner_id,
        display_name: r.display_name,
        password_protected: r.password_hash.is_some(),
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
    }))
}

pub async fn create_room(
    pool: &PgPool,
    owner_id: &Uuid,
    req: &CreateRoomRequest,
    password_hash: Option<&str>,
) -> Result<RoomSettings, sqlx::Error> {
    sqlx::query(
        "INSERT INTO rooms (id, owner_id, display_name, password_hash, require_registration,
                           max_participants, max_broadcasters, moderated, secret, lobby_enabled,
                           guests_allowed, guests_can_broadcast, topic)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)"
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
    .execute(pool)
    .await?;

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
pub fn apply_settings_update(
    settings: &mut RoomSettings,
    moderated: Option<bool>,
    lobby_enabled: Option<bool>,
    guests_allowed: Option<bool>,
    guests_can_broadcast: Option<bool>,
    max_broadcasters: Option<Option<i32>>,
    allow_screen_sharing: Option<bool>,
    allow_chat: Option<bool>,
    push_to_talk: Option<bool>,
    secret: Option<bool>,
    password: Option<Option<String>>,
) {
    if let Some(v) = moderated { settings.moderated = v; }
    if let Some(v) = lobby_enabled { settings.lobby_enabled = v; }
    if let Some(v) = guests_allowed { settings.guests_allowed = v; }
    if let Some(v) = guests_can_broadcast { settings.guests_can_broadcast = v; }
    if let Some(v) = max_broadcasters { settings.max_broadcasters = v; }
    if let Some(v) = allow_screen_sharing { settings.allow_screen_sharing = v; }
    if let Some(v) = allow_chat { settings.allow_chat = v; }
    if let Some(v) = push_to_talk { settings.push_to_talk = v; }
    if let Some(v) = secret { settings.secret = v; }
    if let Some(v) = password {
        settings.password_protected = v.is_some();
        // Don't store actual password in RoomSettings — it goes to DB only
    }
}

/// Persist partial room settings updates to the database.
/// Only updates columns whose corresponding parameter is `Some`.
pub async fn update_room_settings(
    pool: &PgPool,
    room_id: &str,
    moderated: Option<bool>,
    lobby_enabled: Option<bool>,
    guests_allowed: Option<bool>,
    guests_can_broadcast: Option<bool>,
    max_broadcasters: Option<Option<i32>>,
    allow_screen_sharing: Option<bool>,
    allow_chat: Option<bool>,
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
    maybe_add!(allow_screen_sharing, "allow_screen_sharing");
    maybe_add!(allow_chat, "allow_chat");
    maybe_add!(push_to_talk, "push_to_talk");
    maybe_add!(secret, "secret");
    maybe_add!(password_hash, "password_hash");

    if set_parts.is_empty() {
        return Ok(()); // Nothing to update
    }

    let sql = format!("UPDATE rooms SET {} WHERE id = $1", set_parts.join(", "));
    let mut query = sqlx::query(&sql).bind(room_id);

    // Bind values in the same order as the SET parts
    if let Some(v) = moderated { query = query.bind(v); }
    if let Some(v) = lobby_enabled { query = query.bind(v); }
    if let Some(v) = guests_allowed { query = query.bind(v); }
    if let Some(v) = guests_can_broadcast { query = query.bind(v); }
    if let Some(v) = max_broadcasters { query = query.bind(v); }
    if let Some(v) = allow_screen_sharing { query = query.bind(v); }
    if let Some(v) = allow_chat { query = query.bind(v); }
    if let Some(v) = push_to_talk { query = query.bind(v); }
    if let Some(v) = secret { query = query.bind(v); }
    if let Some(v) = password_hash { query = query.bind(v); }

    query.execute(pool).await?;
    Ok(())
}

pub async fn delete_room(pool: &PgPool, room_id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM rooms WHERE id = $1")
        .bind(room_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
