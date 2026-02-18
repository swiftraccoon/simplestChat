#![forbid(unsafe_code)]

use crate::auth::{jwt, types::AuthError};
use crate::room::settings::{self, CreateRoomRequest, RoomSettings};
use crate::signaling::SignalingServer;
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize)]
pub struct RoomListItem {
    pub id: String,
    pub display_name: String,
    pub topic: Option<String>,
    pub participant_count: usize,
    pub password_protected: bool,
    pub moderated: bool,
}

#[derive(Deserialize)]
pub struct ListParams {
    pub page: Option<u32>,
    pub limit: Option<u32>,
    pub q: Option<String>,
}

/// GET /api/rooms
pub async fn list_rooms(
    State(server): State<SignalingServer>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<RoomListItem>>, Response> {
    let pool = server.db_pool()
        .ok_or_else(|| (StatusCode::SERVICE_UNAVAILABLE, "Database not configured").into_response())?;

    let limit = params.limit.unwrap_or(20).min(100) as i64;
    let offset = ((params.page.unwrap_or(1).max(1) - 1) as i64) * limit;

    let rooms = if let Some(q) = &params.q {
        let escaped = q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        let pattern = format!("%{}%", escaped);
        sqlx::query_as::<_, (String, String, Option<String>, bool, bool)>(
            "SELECT id, display_name, topic, password_hash IS NOT NULL, moderated
             FROM rooms WHERE secret = false
             AND (display_name ILIKE $1 OR topic ILIKE $1)
             ORDER BY created_at DESC LIMIT $2 OFFSET $3"
        )
        .bind(&pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, (String, String, Option<String>, bool, bool)>(
            "SELECT id, display_name, topic, password_hash IS NOT NULL, moderated
             FROM rooms WHERE secret = false
             ORDER BY created_at DESC LIMIT $1 OFFSET $2"
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response())?;

    let items: Vec<RoomListItem> = rooms.into_iter().map(|r| {
        let count = server.room_manager()
            .participant_count_for_room(&r.0);
        RoomListItem {
            id: r.0,
            display_name: r.1,
            topic: r.2,
            participant_count: count,
            password_protected: r.3,
            moderated: r.4,
        }
    }).collect();

    Ok(Json(items))
}

/// POST /api/rooms
pub async fn create_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomRequest>,
) -> Result<Json<RoomSettings>, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    let token = headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)?;

    let claims = jwt::validate_token(token, secret)?;
    let owner_id: uuid::Uuid = claims.sub.parse()
        .map_err(|_| AuthError::InvalidToken)?;

    if req.id.is_empty() || req.id.len() > 128 {
        return Err(AuthError::InvalidCredentials);
    }

    let password_hash = req.password.as_deref()
        .map(crate::auth::password::hash_password)
        .transpose()
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    let room = settings::create_room(pool, &owner_id, &req, password_hash.as_deref())
        .await
        .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    Ok(Json(room))
}

/// DELETE /api/rooms/:id
pub async fn delete_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> Result<StatusCode, AuthError> {
    let pool = server.db_pool().ok_or(AuthError::NotConfigured)?;
    let secret = server.jwt_secret().ok_or(AuthError::NotConfigured)?;

    let token = headers.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)?;

    let claims = jwt::validate_token(token, secret)?;
    let user_id: uuid::Uuid = claims.sub.parse()
        .map_err(|_| AuthError::InvalidToken)?;

    let owner_row = sqlx::query_as::<_, (uuid::Uuid,)>(
        "SELECT owner_id FROM rooms WHERE id = $1"
    )
    .bind(&room_id)
    .fetch_optional(pool)
    .await
    .map_err(|e| AuthError::DatabaseError(e.to_string()))?;

    match owner_row {
        Some(row) if row.0 == user_id => {
            settings::delete_room(pool, &room_id).await
                .map_err(|e| AuthError::DatabaseError(e.to_string()))?;
            Ok(StatusCode::NO_CONTENT)
        }
        Some(_) => Err(AuthError::InvalidCredentials),
        None => Err(AuthError::UserNotFound),
    }
}
