#![forbid(unsafe_code)]

use crate::auth::{jwt, types::AuthError};
use crate::room::DeleteRoomResult;
use crate::room::settings::{self, CreateRoomRequest, RoomSettings};
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tracing::warn;

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

const MAX_ROOM_LIST_PAGE: u32 = 1_000;
const MAX_ROOM_LIST_LIMIT_INPUT: u32 = 1_000;
const MAX_ROOM_LIST_RESULTS: u32 = 100;
const MIN_ROOM_SEARCH_TRIGRAM_LEN: usize = 3;
const MAX_ROOM_SEARCH_CHARACTERS: usize = 128;

fn acquire_room_api_request(
    server: &SignalingServer,
) -> Result<tokio::sync::OwnedSemaphorePermit, Response> {
    server.try_acquire_room_api_request().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            "Service busy",
        )
            .into_response()
    })
}

fn is_ascii_alphanumeric(character: char) -> bool {
    character.is_ascii_alphanumeric()
}

fn has_indexable_search_trigram(query: &str) -> bool {
    query
        .split(|character| !is_ascii_alphanumeric(character))
        .any(|word| word.len() >= MIN_ROOM_SEARCH_TRIGRAM_LEN)
}

fn room_search_pattern(query: &str) -> Result<Option<String>, &'static str> {
    let query = query.trim();
    if query.is_empty() {
        return Ok(None);
    }

    if query.chars().count() > MAX_ROOM_SEARCH_CHARACTERS
        || query.chars().any(char::is_control)
        // pg_trgm ignores punctuation and may not classify non-ASCII letters as
        // word characters under a C locale. Requiring an ASCII word trigram
        // prevents inputs such as "%%%" from degenerating into a full scan.
        || !has_indexable_search_trigram(query)
    {
        return Err("Invalid search query");
    }

    let mut pattern = String::with_capacity(query.len() + 2);
    pattern.push('%');
    for character in query.chars() {
        if matches!(character, '\\' | '%' | '_') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');

    Ok(Some(pattern))
}

fn list_window(params: &ListParams) -> Result<(i64, i64), &'static str> {
    let page = params.page.unwrap_or(1);
    if !(1..=MAX_ROOM_LIST_PAGE).contains(&page) {
        return Err("Invalid page");
    }

    let requested_limit = params.limit.unwrap_or(20);
    if !(1..=MAX_ROOM_LIST_LIMIT_INPUT).contains(&requested_limit) {
        return Err("Invalid limit");
    }

    let limit = requested_limit.min(MAX_ROOM_LIST_RESULTS) as i64;
    let offset = i64::from(page - 1) * limit;
    Ok((limit, offset))
}

/// GET /api/rooms
pub async fn list_rooms(
    State(server): State<SignalingServer>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<RoomListItem>>, Response> {
    let search_pattern = params
        .q
        .as_deref()
        .map(room_search_pattern)
        .transpose()
        .map_err(|message| (StatusCode::BAD_REQUEST, message).into_response())?
        .flatten();
    let (limit, offset) = list_window(&params)
        .map_err(|message| (StatusCode::BAD_REQUEST, message).into_response())?;
    let _request_permit = acquire_room_api_request(&server)?;
    let pool = server.db_pool().ok_or_else(|| {
        (StatusCode::SERVICE_UNAVAILABLE, "Database not configured").into_response()
    })?;

    let rooms = if let Some(pattern) = search_pattern {
        // The materialized search stage prevents ORDER BY/LIMIT from making
        // PostgreSQL walk the created_at index and test every row on a miss.
        sqlx::query_as::<_, (String, String, Option<String>, bool, bool)>(
            r#"WITH matching_rooms AS MATERIALIZED (
                 SELECT id, display_name, topic,
                        password_hash IS NOT NULL AS password_protected,
                        moderated, created_at
                 FROM rooms
                 WHERE secret = false
                   AND (display_name || E'\n' || COALESCE(topic, ''))
                       ILIKE $1 ESCAPE E'\\'
             )
             SELECT id, display_name, topic, password_protected, moderated
             FROM matching_rooms
             ORDER BY created_at DESC LIMIT $2 OFFSET $3"#,
        )
        .bind(pattern)
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    } else {
        sqlx::query_as::<_, (String, String, Option<String>, bool, bool)>(
            "SELECT id, display_name, topic, password_hash IS NOT NULL, moderated
             FROM rooms WHERE secret = false
             ORDER BY created_at DESC LIMIT $1 OFFSET $2",
        )
        .bind(limit)
        .bind(offset)
        .fetch_all(pool)
        .await
    }
    .map_err(|error| {
        warn!(%error, "Failed to list rooms");
        (StatusCode::INTERNAL_SERVER_ERROR, "Unable to list rooms").into_response()
    })?;

    let items: Vec<RoomListItem> = rooms
        .into_iter()
        .map(|r| {
            let count = server.room_manager().participant_count_for_room(&r.0);
            RoomListItem {
                id: r.0,
                display_name: r.1,
                topic: r.2,
                participant_count: count,
                password_protected: r.3,
                moderated: r.4,
            }
        })
        .collect();

    Ok(Json(items))
}

/// POST /api/rooms
pub async fn create_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomRequest>,
) -> Result<Json<RoomSettings>, Response> {
    let _pool = server
        .db_pool()
        .ok_or(AuthError::NotConfigured)
        .map_err(IntoResponse::into_response)?;
    let secret = server
        .jwt_secret()
        .ok_or(AuthError::NotConfigured)
        .map_err(IntoResponse::into_response)?;

    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)
        .map_err(IntoResponse::into_response)?;

    let claims = jwt::validate_token(token, secret).map_err(IntoResponse::into_response)?;
    let owner_id: uuid::Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    if !server.allow_room_creation(&claims.sub) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Room creation is rate limited",
        )
            .into_response());
    }

    if let Err(message) = settings::validate_create_request(&req) {
        return Err((StatusCode::BAD_REQUEST, message).into_response());
    }
    let _request_permit = acquire_room_api_request(&server)?;

    let password_hash = match req.password.clone() {
        Some(password) => Some(
            server
                .room_manager()
                .hash_room_password(password)
                .await
                .map_err(|error| {
                    warn!(%error, "Failed to hash room password");
                    (StatusCode::SERVICE_UNAVAILABLE, "Unable to create room").into_response()
                })?,
        ),
        None => None,
    };

    let room = server
        .room_manager()
        .create_persisted_room(&owner_id, &req, password_hash.as_deref())
        .await
        .map_err(|error| {
            if settings::is_global_persisted_room_quota_error(&error) {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Persisted room capacity has been reached",
                )
                    .into_response();
            }
            if settings::is_persisted_room_quota_error(&error) {
                return (
                    StatusCode::CONFLICT,
                    format!(
                        "Persisted room quota reached (maximum {})",
                        settings::MAX_PERSISTED_ROOMS_PER_OWNER
                    ),
                )
                    .into_response();
            }
            warn!(room_id = %req.id, %error, "Failed to create room");
            AuthError::DatabaseError("room creation failed".to_string()).into_response()
        })?
        .ok_or_else(|| {
            (
                StatusCode::CONFLICT,
                "A live ad-hoc room already uses this room ID",
            )
                .into_response()
        })?;

    Ok(Json(room))
}

/// DELETE /api/rooms/:id
pub async fn delete_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> Result<StatusCode, Response> {
    if !settings::valid_room_id(&room_id) {
        return Err((StatusCode::NOT_FOUND, "Room not found").into_response());
    }
    if server.db_pool().is_none() {
        return Err(AuthError::NotConfigured.into_response());
    }
    let secret = server
        .jwt_secret()
        .ok_or(AuthError::NotConfigured)
        .map_err(IntoResponse::into_response)?;

    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .ok_or(AuthError::MissingToken)
        .map_err(IntoResponse::into_response)?;

    let claims = jwt::validate_token(token, secret).map_err(IntoResponse::into_response)?;
    let user_id: uuid::Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    let _request_permit = acquire_room_api_request(&server)?;

    let result = server
        .room_manager()
        .delete_persisted_room(&room_id, &user_id)
        .await
        .map_err(|error| {
            warn!(room_id, %error, "Failed to delete room");
            AuthError::DatabaseError("room deletion failed".to_string()).into_response()
        })?;

    match result {
        DeleteRoomResult::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteRoomResult::Forbidden | DeleteRoomResult::NotFound => {
            Err((StatusCode::NOT_FOUND, "Room not found").into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_window_bounds_page_and_requested_limit() {
        let valid = ListParams {
            page: Some(MAX_ROOM_LIST_PAGE),
            limit: Some(MAX_ROOM_LIST_LIMIT_INPUT),
            q: None,
        };
        assert_eq!(list_window(&valid), Ok((100, 99_900)));

        let page_too_large = ListParams {
            page: Some(MAX_ROOM_LIST_PAGE + 1),
            limit: None,
            q: None,
        };
        assert_eq!(list_window(&page_too_large), Err("Invalid page"));

        let limit_too_large = ListParams {
            page: None,
            limit: Some(MAX_ROOM_LIST_LIMIT_INPUT + 1),
            q: None,
        };
        assert_eq!(list_window(&limit_too_large), Err("Invalid limit"));
    }

    #[test]
    fn list_window_rejects_zero_values() {
        let zero_page = ListParams {
            page: Some(0),
            limit: None,
            q: None,
        };
        assert!(list_window(&zero_page).is_err());

        let zero_limit = ListParams {
            page: None,
            limit: Some(0),
            q: None,
        };
        assert!(list_window(&zero_limit).is_err());
    }

    #[test]
    fn room_search_escapes_like_metacharacters_exactly() {
        assert_eq!(
            room_search_pattern(r"  abc%_\room  "),
            Ok(Some(r"%abc\%\_\\room%".to_string()))
        );
        assert_eq!(
            room_search_pattern("three ' quoted"),
            Ok(Some("%three ' quoted%".to_string()))
        );
    }

    #[test]
    fn room_search_requires_an_indexable_trigram() {
        for query in ["a", "ab", "%%%", "a-b-c", "___", "東京会議"] {
            assert_eq!(room_search_pattern(query), Err("Invalid search query"));
        }

        for query in ["abc", "New York", "abc%", "room_123"] {
            assert!(room_search_pattern(query).unwrap().is_some(), "{query}");
        }

        assert_eq!(room_search_pattern("   "), Ok(None));
    }

    #[test]
    fn room_search_bounds_characters_and_rejects_controls() {
        assert_eq!(
            room_search_pattern(&"a".repeat(MAX_ROOM_SEARCH_CHARACTERS + 1)),
            Err("Invalid search query")
        );
        assert_eq!(
            room_search_pattern("valid\nquery"),
            Err("Invalid search query")
        );
    }
}
