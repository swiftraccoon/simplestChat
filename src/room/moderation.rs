#![forbid(unsafe_code)]

use crate::room::roles::Role;
use sqlx::PgPool;
use std::net::IpAddr;
use uuid::Uuid;

/// In-memory punitive state for a participant
#[derive(Debug, Clone, Default)]
pub struct PunitiveState {
    pub cam_banned: bool,
    pub text_muted: bool,
}

/// Persist a ban to room_states. Registered targets are keyed by user_id;
/// guests are keyed by IP. Rows carry both when available.
pub async fn persist_ban(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    reason: Option<&str>,
    expires_at: Option<chrono::DateTime<chrono::Utc>>,
    applied_by: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO room_states (room_id, user_id, state, ip_address, reason, expires_at, applied_by)
         VALUES ($1, $2, 'banned', $3::inet, $4, $5, $6)
         ON CONFLICT (room_id, user_id, state) DO UPDATE
           SET ip_address = EXCLUDED.ip_address,
               reason = EXCLUDED.reason,
               expires_at = EXCLUDED.expires_at,
               applied_by = EXCLUDED.applied_by",
    )
    .bind(room_id)
    .bind(user_id)
    .bind(ip.map(|i| i.to_string()))
    .bind(reason)
    .bind(expires_at)
    .bind(applied_by)
    .execute(pool)
    .await?;
    Ok(())
}

/// Check whether a joining participant is banned. Registered users match by
/// user_id (from any IP). IP rows only block unauthenticated connections —
/// shared IPs (CGNAT, tunnels) must not lock out registered users.
pub async fn is_banned(
    pool: &PgPool,
    room_id: &str,
    user_id: Option<Uuid>,
    ip: Option<IpAddr>,
    authenticated: bool,
) -> Result<bool, sqlx::Error> {
    let (exists,): (bool,) = sqlx::query_as(
        "SELECT EXISTS(
            SELECT 1 FROM room_states
            WHERE room_id = $1 AND state = 'banned'
              AND (expires_at IS NULL OR expires_at > now())
              AND ( (user_id IS NOT NULL AND user_id = $2)
                 OR ($3::inet IS NOT NULL AND $4 = false AND ip_address = $3::inet) )
        )",
    )
    .bind(room_id)
    .bind(user_id)
    .bind(ip.map(|i| i.to_string()))
    .bind(authenticated)
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Remove a registered user's persisted ban (guest/IP rows have no stable
/// identity to unban through the UI). Returns true if a ban row was deleted.
pub async fn remove_user_ban(
    pool: &PgPool,
    room_id: &str,
    user_id: Uuid,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM room_states WHERE room_id = $1 AND state = 'banned' AND user_id = $2")
        .bind(room_id)
        .bind(user_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Check if a participant can produce (not cam-banned, has correct role for moderated rooms)
pub fn can_produce(state: &PunitiveState, role: Role, moderated: bool, source: &str) -> bool {
    if state.cam_banned && (source == "camera" || source == "screen") {
        return false;
    }
    if moderated && !role.can_broadcast(true) {
        return false;
    }
    true
}

/// Check if a participant can chat (not text-muted, has correct role)
pub fn can_chat(state: &PunitiveState, role: Role, moderated: bool) -> bool {
    if state.text_muted {
        return false;
    }
    role.can_chat(moderated)
}
