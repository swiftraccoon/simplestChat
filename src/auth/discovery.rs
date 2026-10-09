//! Bounded account contacts and room shortcuts. Exact account identifiers are
//! shared explicitly; there is no email or account-name search endpoint.
#![forbid(unsafe_code)]

use super::{
    account, routes,
    types::{AuthError, Claims},
};
use crate::{
    room::api::{RoomListItem, RoomListRow, room_list_item},
    signaling::SignalingServer,
};
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

const MAX_CONTACTS: i64 = 100;
const MAX_REQUESTS_PER_DAY: i64 = 30;
const MAX_FAVORITES: i64 = 100;
const MAX_RECENT: i64 = 50;
static VISIT_WRITES: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(16);

/// A shortcut is best effort and never holds up joining or queues unbounded work.
pub(crate) fn note_room_visit(pool: &PgPool, own: Uuid, room: &str) {
    let Ok(permit) = VISIT_WRITES.try_acquire() else {
        return;
    };
    let pool = pool.clone();
    let room = room.to_owned();
    tokio::spawn(async move {
        let _permit = permit;
        if let Err(error) = record_room_visit(&pool, own, &room).await {
            crate::db::record_error(&error);
            tracing::debug!("Recent room could not be saved");
        }
    });
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Contact {
    account_id: Uuid,
    account_name: String,
    status: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContactsPage {
    account_id: Uuid,
    contacts: Vec<Contact>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContactRequest {
    account_id: Uuid,
}

fn account_id(claims: &Claims) -> Result<Uuid, AuthError> {
    claims.sub.parse().map_err(|_| AuthError::InvalidToken)
}

fn ordered_pair(own: Uuid, peer: Uuid) -> (Uuid, Uuid) {
    if own < peer { (own, peer) } else { (peer, own) }
}

fn allows_contact(preferences: &serde_json::Value, peer: Uuid) -> bool {
    let id = peer.to_string();
    preferences
        .get("allowPrivateMessages")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(true)
        && !preferences
            .get("ignored")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|people| {
                people.iter().any(|person| {
                    person.get("id").and_then(serde_json::Value::as_str) == Some(id.as_str())
                })
            })
}

/// Serialize caps and relationship changes by locking users in UUID order,
/// then recheck the current session. This preserves auth's users → sessions order.
async fn lock_contact_accounts(
    connection: &mut PgConnection,
    claims: &Claims,
    peer: Uuid,
) -> Result<Option<bool>, AuthError> {
    let own = account_id(claims)?;
    let people: Vec<(Uuid, i64, serde_json::Value)> = sqlx::query_as(
        "SELECT id,auth_version,preferences FROM users WHERE id=ANY($1) ORDER BY id FOR UPDATE",
    )
    .bind(vec![own, peer])
    .fetch_all(&mut *connection)
    .await
    .map_err(routes::database_error)?;
    let actor = people
        .iter()
        .find(|person| person.0 == own)
        .ok_or(AuthError::InvalidToken)?;
    if actor.1 != claims.auth_version {
        return Err(AuthError::InvalidToken);
    }
    require_session(connection, claims, own).await?;
    Ok(people
        .iter()
        .find(|person| person.0 == peer)
        .map(|target| allows_contact(&actor.2, peer) && allows_contact(&target.2, own)))
}

async fn require_session(
    connection: &mut PgConnection,
    claims: &Claims,
    own: Uuid,
) -> Result<(), AuthError> {
    let live: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp() FOR SHARE")
        .bind(claims.sid).bind(own).fetch_optional(connection).await.map_err(routes::database_error)?;
    live.map(|_| ()).ok_or(AuthError::InvalidToken)
}

pub async fn list_contacts(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<ContactsPage>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own = account_id(&claims)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let rows: Vec<(Uuid, String, String, Uuid)> = sqlx::query_as(
        "SELECT u.id,u.display_name,c.status,c.requester_id FROM contacts c JOIN users u
         ON u.id=CASE WHEN c.low_id=$1 THEN c.high_id ELSE c.low_id END
         WHERE (c.low_id=$1 OR c.high_id=$1)
           AND (c.status='accepted' OR (c.status='pending' AND c.requested_at>now()-interval '14 days'))
         ORDER BY c.status,u.display_name,u.id LIMIT 100")
        .bind(own).fetch_all(pool).await.map_err(routes::database_error)?;
    Ok((
        routes::no_store_headers(),
        Json(ContactsPage {
            account_id: own,
            contacts: rows
                .into_iter()
                .map(|(account_id, account_name, status, requester)| Contact {
                    account_id,
                    account_name,
                    status: if status == "accepted" {
                        "accepted"
                    } else if requester == own {
                        "outgoing"
                    } else {
                        "incoming"
                    },
                })
                .collect(),
        }),
    ))
}

async fn request_contact(pool: &PgPool, claims: &Claims, peer: Uuid) -> Result<(), AuthError> {
    let own = account_id(claims)?;
    if own == peer {
        return Err(AuthError::InvalidInput("Choose another account"));
    }
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let allowed = lock_contact_accounts(&mut tx, claims, peer).await?;
    // Missing, opted-out and ignoring accounts have the same response.
    if allowed != Some(true) {
        return Ok(());
    }
    let (low, high) = ordered_pair(own, peer);
    sqlx::query("DELETE FROM contacts WHERE (low_id=$1 OR high_id=$1) AND status<>'accepted' AND updated_at<now()-interval '30 days'")
        .bind(own).execute(&mut *tx).await.map_err(routes::database_error)?;
    let recent: i64 = sqlx::query_scalar("SELECT count(*) FROM contacts WHERE requester_id=$1 AND requested_at>now()-interval '1 day'")
        .bind(own).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
    if recent >= MAX_REQUESTS_PER_DAY {
        return Err(AuthError::RateLimited);
    }
    let existing: Option<(String, DateTime<Utc>)> =
        sqlx::query_as("SELECT status,updated_at FROM contacts WHERE low_id=$1 AND high_id=$2")
            .bind(low)
            .bind(high)
            .fetch_optional(&mut *tx)
            .await
            .map_err(routes::database_error)?;
    if existing.is_some_and(|(status, updated)| {
        status == "accepted" || updated > Utc::now() - chrono::Duration::days(30)
    }) {
        return Ok(());
    }
    let counts: Vec<(Uuid, i64)> = sqlx::query_as(
        "SELECT u.id,(SELECT count(*) FROM contacts c WHERE (c.low_id=u.id OR c.high_id=u.id)
         AND (c.status='accepted' OR (c.status='pending' AND c.requested_at>now()-interval '14 days')))
         FROM users u WHERE u.id=ANY($1)")
        .bind(vec![own, peer]).fetch_all(&mut *tx).await.map_err(routes::database_error)?;
    if counts
        .iter()
        .any(|(id, count)| *id == own && *count >= MAX_CONTACTS)
    {
        return Err(AuthError::InvalidInput(
            "Keep at most 100 contacts and pending requests",
        ));
    }
    if counts.iter().any(|(_, count)| *count >= MAX_CONTACTS) {
        return Ok(());
    }
    sqlx::query("INSERT INTO contacts (low_id,high_id,requester_id,status) VALUES ($1,$2,$3,'pending')
         ON CONFLICT (low_id,high_id) DO UPDATE SET requester_id=$3,status='pending',requested_at=now(),updated_at=now()")
        .bind(low).bind(high).bind(own).execute(&mut *tx).await.map_err(routes::database_error)?;
    tx.commit().await.map_err(routes::database_error)
}

pub async fn create_contact(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<ContactRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    request_contact(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        request.account_id,
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

async fn change_contact(
    pool: &PgPool,
    claims: &Claims,
    peer: Uuid,
    accept: bool,
) -> Result<(), AuthError> {
    let own = account_id(claims)?;
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let allowed = lock_contact_accounts(&mut tx, claims, peer).await?;
    let (low, high) = ordered_pair(own, peer);
    if accept {
        if allowed != Some(true) {
            return Err(AuthError::InvalidInput("Contact request unavailable"));
        }
        let changed = sqlx::query("UPDATE contacts SET status='accepted',updated_at=now()
            WHERE low_id=$1 AND high_id=$2 AND requester_id=$3 AND status='pending' AND requested_at>now()-interval '14 days'")
            .bind(low).bind(high).bind(peer).execute(&mut *tx).await.map_err(routes::database_error)?;
        if changed.rows_affected() == 0 {
            return Err(AuthError::InvalidInput("Contact request unavailable"));
        }
    } else {
        sqlx::query("UPDATE contacts SET status='declined',updated_at=now() WHERE low_id=$1 AND high_id=$2 AND status<>'declined'")
            .bind(low).bind(high).execute(&mut *tx).await.map_err(routes::database_error)?;
    }
    tx.commit().await.map_err(routes::database_error)
}

pub async fn accept_contact(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    change_contact(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        peer,
        true,
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

pub async fn remove_contact(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(peer): Path<Uuid>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    change_contact(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        peer,
        false,
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

/// Caller holds both account rows before checking the relationship. A contact
/// never overrides private-message opt-out or either account's ignore list.
pub(crate) async fn accepted_contact(
    connection: &mut PgConnection,
    own: Uuid,
    peer: Uuid,
) -> Result<bool, sqlx::Error> {
    let (low, high) = ordered_pair(own, peer);
    sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM contacts WHERE low_id=$1 AND high_id=$2 AND status='accepted')")
        .bind(low).bind(high).fetch_one(connection).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SavedRoom {
    room: RoomListItem,
    favorite: bool,
    last_visited: Option<DateTime<Utc>>,
}

#[derive(Serialize)]
pub struct SavedRoomsPage {
    rooms: Vec<SavedRoom>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SaveRoomRequest {
    favorite: bool,
}

async fn visible_saved_entries(
    pool: &PgPool,
    own: Uuid,
) -> Result<Vec<(String, bool, Option<DateTime<Utc>>)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT s.room_id,s.favorite,s.last_visited FROM saved_rooms s JOIN rooms r ON r.id=s.room_id
         WHERE s.user_id=$1 AND (r.secret=false OR r.owner_id=$1 OR EXISTS(SELECT 1 FROM room_roles m WHERE m.room_id=r.id AND m.user_id=$1))
         ORDER BY s.favorite DESC,s.last_visited DESC NULLS LAST,s.room_id LIMIT 150")
        .bind(own).fetch_all(pool).await
}

pub async fn saved_rooms(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<SavedRoomsPage>), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    let own = account_id(&claims)?;
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let entries = visible_saved_entries(pool, own)
        .await
        .map_err(routes::database_error)?;
    let ids: Vec<&str> = entries.iter().map(|entry| entry.0.as_str()).collect();
    // Repeat visibility in the details query so a concurrent membership removal
    // cannot expose an unlisted room between these two bounded reads.
    let rooms: Vec<RoomListRow> = sqlx::query_as(
        "SELECT r.id,r.display_name,r.topic,r.password_hash IS NOT NULL,r.moderated,r.description,r.image_url,r.secret,r.name_style,r.topic_style
         FROM rooms r WHERE r.id=ANY($1) AND (r.secret=false OR r.owner_id=$2 OR EXISTS(SELECT 1 FROM room_roles m WHERE m.room_id=r.id AND m.user_id=$2))")
        .bind(ids).bind(own).fetch_all(pool).await.map_err(routes::database_error)?;
    let mut details: std::collections::HashMap<String, RoomListRow> = rooms
        .into_iter()
        .map(|room| (room.0.clone(), room))
        .collect();
    let rooms = entries
        .into_iter()
        .filter_map(|(id, favorite, last_visited)| {
            details.remove(&id).map(|room| SavedRoom {
                room: room_list_item(&server, room),
                favorite,
                last_visited,
            })
        })
        .collect();
    Ok((routes::no_store_headers(), Json(SavedRoomsPage { rooms })))
}

async fn lock_room_account(
    connection: &mut PgConnection,
    claims: &Claims,
) -> Result<Uuid, AuthError> {
    let own = account_id(claims)?;
    let version: Option<i64> =
        sqlx::query_scalar("SELECT auth_version FROM users WHERE id=$1 FOR UPDATE")
            .bind(own)
            .fetch_optional(&mut *connection)
            .await
            .map_err(routes::database_error)?;
    if version != Some(claims.auth_version) {
        return Err(AuthError::InvalidToken);
    }
    require_session(connection, claims, own).await?;
    Ok(own)
}

async fn save_favorite(
    pool: &PgPool,
    claims: &Claims,
    room: &str,
    favorite: bool,
) -> Result<(), AuthError> {
    let mut tx = pool.begin().await.map_err(routes::database_error)?;
    let own = lock_room_account(&mut tx, claims).await?;
    if favorite {
        let visible: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM rooms r WHERE r.id=$1
            AND (r.secret=false OR r.owner_id=$2 OR EXISTS(SELECT 1 FROM room_roles m WHERE m.room_id=r.id AND m.user_id=$2)))")
            .bind(room).bind(own).fetch_one(&mut *tx).await.map_err(routes::database_error)?;
        if !visible {
            return Err(AuthError::InvalidInput("Room unavailable"));
        }
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM saved_rooms WHERE user_id=$1 AND favorite AND room_id<>$2",
        )
        .bind(own)
        .bind(room)
        .fetch_one(&mut *tx)
        .await
        .map_err(routes::database_error)?;
        if count >= MAX_FAVORITES {
            return Err(AuthError::InvalidInput("Keep at most 100 favorite rooms"));
        }
        sqlx::query(
            "INSERT INTO saved_rooms (user_id,room_id,favorite) VALUES ($1,$2,true)
            ON CONFLICT (user_id,room_id) DO UPDATE SET favorite=true",
        )
        .bind(own)
        .bind(room)
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
    } else {
        sqlx::query(
            "DELETE FROM saved_rooms WHERE user_id=$1 AND room_id=$2 AND last_visited IS NULL",
        )
        .bind(own)
        .bind(room)
        .execute(&mut *tx)
        .await
        .map_err(routes::database_error)?;
        sqlx::query("UPDATE saved_rooms SET favorite=false WHERE user_id=$1 AND room_id=$2")
            .bind(own)
            .bind(room)
            .execute(&mut *tx)
            .await
            .map_err(routes::database_error)?;
        trim_recent(&mut tx, own)
            .await
            .map_err(routes::database_error)?;
    }
    tx.commit().await.map_err(routes::database_error)
}

pub async fn save_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(room): Path<String>,
    Json(request): Json<SaveRoomRequest>,
) -> Result<(HeaderMap, StatusCode), AuthError> {
    let _permit = routes::acquire_auth_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers).await?;
    if room.is_empty() || room.len() > 128 {
        return Err(AuthError::InvalidInput("Room unavailable"));
    }
    save_favorite(
        server.db_pool().ok_or(AuthError::NotConfigured)?,
        &claims,
        &room,
        request.favorite,
    )
    .await?;
    Ok((routes::no_store_headers(), StatusCode::NO_CONTENT))
}

async fn trim_recent(connection: &mut PgConnection, own: Uuid) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM saved_rooms WHERE user_id=$1 AND NOT favorite AND room_id IN
        (SELECT room_id FROM saved_rooms WHERE user_id=$1 AND NOT favorite ORDER BY last_visited DESC,room_id OFFSET $2)")
        .bind(own).bind(MAX_RECENT).execute(connection).await?;
    Ok(())
}

/// Only call after admission succeeds; ad-hoc rooms are deliberately not saved.
pub(crate) async fn record_room_visit(
    pool: &PgPool,
    own: Uuid,
    room: &str,
) -> Result<(), sqlx::Error> {
    let mut tx = pool.begin().await?;
    let exists: Option<Uuid> = sqlx::query_scalar("SELECT id FROM users WHERE id=$1 FOR UPDATE")
        .bind(own)
        .fetch_optional(&mut *tx)
        .await?;
    if exists.is_none() {
        return Ok(());
    }
    sqlx::query("INSERT INTO saved_rooms (user_id,room_id,last_visited) SELECT $1,id,now() FROM rooms WHERE id=$2
        ON CONFLICT (user_id,room_id) DO UPDATE SET last_visited=now()")
        .bind(own).bind(room).execute(&mut *tx).await?;
    trim_recent(&mut tx, own).await?;
    tx.commit().await
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
