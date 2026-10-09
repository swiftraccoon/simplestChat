#![forbid(unsafe_code)]

use crate::auth::{account, types::AuthError};
use crate::room::DeleteRoomResult;
use crate::room::community::RoomIdentityUpdate;
use crate::room::settings::{self, CreateRoomRequest, RoomSettings};
use crate::signaling::SignalingServer;
use crate::signaling::protocol::ChatStyle;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use tracing::warn;

/// Keep handler futures small without changing their HTTP error responses.
#[derive(Debug)]
pub struct RoomApiError(Box<Response>);

impl From<Response> for RoomApiError {
    fn from(response: Response) -> Self {
        Self(Box::new(response))
    }
}

impl IntoResponse for RoomApiError {
    fn into_response(self) -> Response {
        *self.0
    }
}

#[derive(Serialize)]
pub struct RoomListItem {
    pub id: String,
    pub display_name: String,
    pub topic: Option<String>,
    /// `null` when the room's state lock was busy at listing time.
    pub participant_count: Option<usize>,
    pub password_protected: bool,
    pub moderated: bool,
    /// `null` when the room's state lock was busy at listing time.
    pub broadcaster_count: Option<usize>,
    pub description: String,
    pub image_url: Option<String>,
    pub secret: bool,
    pub name_style: ChatStyle,
    pub topic_style: ChatStyle,
}

pub(crate) type RoomListRow = (
    String,
    String,
    Option<String>,
    bool,
    bool,
    String,
    Option<String>,
    bool,
    sqlx::types::Json<ChatStyle>,
    sqlx::types::Json<ChatStyle>,
);

pub(crate) fn room_list_item(server: &SignalingServer, row: RoomListRow) -> RoomListItem {
    RoomListItem {
        participant_count: server.room_manager().participant_count_for_room(&row.0),
        broadcaster_count: server.room_manager().broadcaster_count_for_room(&row.0),
        id: row.0,
        display_name: row.1,
        topic: row.2,
        password_protected: row.3,
        moderated: row.4,
        description: row.5,
        image_url: row.6,
        secret: row.7,
        name_style: row.8.0,
        topic_style: row.9.0,
    }
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

pub(crate) fn acquire_room_api_request(
    server: &SignalingServer,
) -> Result<tokio::sync::OwnedSemaphorePermit, RoomApiError> {
    server.try_acquire_room_api_request().ok_or_else(|| {
        (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            "Service busy",
        )
            .into_response()
            .into()
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

pub(crate) fn room_database_error(error: sqlx::Error) -> Response {
    crate::db::record_error(&error);
    AuthError::DatabaseError(error.to_string()).into_response()
}

/// GET /api/rooms
pub async fn list_rooms(
    State(server): State<SignalingServer>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<RoomListItem>>, RoomApiError> {
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

    let live = server.room_manager().live_room_counts();
    let rooms = fetch_directory_page(pool, &live, search_pattern.as_deref(), limit, offset)
        .await
        .map_err(|error| {
            crate::db::record_error(&error);
            warn!(%error, "Failed to list rooms");
            (StatusCode::INTERNAL_SERVER_ERROR, "Unable to list rooms").into_response()
        })?;

    let items: Vec<RoomListItem> = rooms
        .into_iter()
        .map(|row| room_list_item(&server, row))
        .collect();

    Ok(Json(items))
}

/// Live rooms come first, busiest first; a room whose count is unknown (its
/// lock was held) follows every counted room rather than looking empty.
pub(crate) fn busiest_first(a: Option<usize>, b: Option<usize>) -> std::cmp::Ordering {
    match (a, b) {
        (Some(a), Some(b)) => b.cmp(&a),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => std::cmp::Ordering::Equal,
    }
}

/// How one page splits between the ordered live rooms and the dormant rows the
/// database pages through after them.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct PageWindow {
    pub live: std::ops::Range<usize>,
    pub dormant_limit: i64,
    pub dormant_offset: i64,
}

pub(crate) fn page_window(live_len: usize, limit: i64, offset: i64) -> PageWindow {
    let limit = usize::try_from(limit).unwrap_or(0);
    let offset = usize::try_from(offset).unwrap_or(0);
    let start = offset.min(live_len);
    let end = offset.saturating_add(limit).min(live_len);
    PageWindow {
        live: start..end,
        dormant_limit: i64::try_from(limit - (end - start)).unwrap_or(i64::MAX),
        dormant_offset: i64::try_from(offset.saturating_sub(live_len)).unwrap_or(i64::MAX),
    }
}

/// One directory page: the live rooms first, busiest first, then every room
/// nobody is in, newest first. `live` is the room manager's snapshot of rooms
/// with people in them (public or not, listed or not: the queries filter) and
/// `pattern` a prepared search pattern. The live set is small and bounded by
/// `MAX_ROOMS`, so it is fetched whole and paged in memory; the dormant rows
/// keep the indexed `created_at` order and exclude every live id.
pub(crate) async fn fetch_directory_page(
    pool: &sqlx::PgPool,
    live: &[(String, Option<usize>)],
    pattern: Option<&str>,
    limit: i64,
    offset: i64,
) -> Result<Vec<RoomListRow>, sqlx::Error> {
    let live_ids: Vec<String> = live.iter().map(|(id, _)| id.clone()).collect();
    let mut live_rows = if live_ids.is_empty() {
        Vec::new()
    } else if let Some(pattern) = pattern {
        sqlx::query_as::<_, RoomListRow>(
            r#"SELECT id, display_name, topic, password_hash IS NOT NULL, moderated, description, image_url, secret, name_style, topic_style
               FROM rooms
               WHERE secret = false AND id = ANY($1)
                 AND (display_name || E'\n' || COALESCE(topic, '')) ILIKE $2 ESCAPE E'\\'
               ORDER BY created_at DESC, id DESC"#,
        )
        .bind(&live_ids)
        .bind(pattern)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, RoomListRow>(
            "SELECT id, display_name, topic, password_hash IS NOT NULL, moderated, description, image_url, secret, name_style, topic_style
             FROM rooms WHERE secret = false AND id = ANY($1)
             ORDER BY created_at DESC, id DESC",
        )
        .bind(&live_ids)
        .fetch_all(pool)
        .await?
    };
    let counts: std::collections::HashMap<&str, Option<usize>> = live
        .iter()
        .map(|(id, count)| (id.as_str(), *count))
        .collect();
    // Stable: rooms with the same count keep the newest-first database order.
    live_rows.sort_by(|a, b| {
        busiest_first(
            counts.get(a.0.as_str()).copied().flatten(),
            counts.get(b.0.as_str()).copied().flatten(),
        )
    });
    let window = page_window(live_rows.len(), limit, offset);
    let mut page: Vec<RoomListRow> = live_rows.drain(window.live).collect();
    if window.dormant_limit == 0 {
        return Ok(page);
    }

    let dormant = if let Some(pattern) = pattern {
        // The materialized search stage prevents ORDER BY/LIMIT from making
        // PostgreSQL walk the created_at index and test every row on a miss. It
        // carries only ids and sort keys: descriptions and images are fetched
        // for the selected page alone, so a broad term never spills wide rows.
        sqlx::query_as::<_, RoomListRow>(
            r#"WITH matching_rooms AS MATERIALIZED (
                 SELECT id, created_at
                 FROM rooms
                 WHERE secret = false
                   AND NOT (id = ANY($4))
                   AND (display_name || E'\n' || COALESCE(topic, ''))
                       ILIKE $1 ESCAPE E'\\'
             ),
             page AS (
                 SELECT id, created_at FROM matching_rooms
                 ORDER BY created_at DESC, id DESC LIMIT $2 OFFSET $3
             )
             SELECT rooms.id, rooms.display_name, rooms.topic,
                    rooms.password_hash IS NOT NULL AS password_protected,
                    rooms.moderated, rooms.description, rooms.image_url, rooms.secret, rooms.name_style, rooms.topic_style
             FROM page JOIN rooms ON rooms.id = page.id
             ORDER BY page.created_at DESC, page.id DESC"#,
        )
        .bind(pattern)
        .bind(window.dormant_limit)
        .bind(window.dormant_offset)
        .bind(&live_ids)
        .fetch_all(pool)
        .await?
    } else {
        sqlx::query_as::<_, RoomListRow>(
            "SELECT id, display_name, topic, password_hash IS NOT NULL, moderated, description, image_url, secret, name_style, topic_style
             FROM rooms WHERE secret = false AND NOT (id = ANY($3))
             ORDER BY created_at DESC, id DESC LIMIT $1 OFFSET $2",
        )
        .bind(window.dormant_limit)
        .bind(window.dormant_offset)
        .bind(&live_ids)
        .fetch_all(pool)
        .await?
    };
    page.extend(dormant);
    Ok(page)
}

/// POST /api/rooms
pub async fn create_room(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(req): Json<CreateRoomRequest>,
) -> Result<Json<RoomSettings>, RoomApiError> {
    let _request_permit = acquire_room_api_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers)
        .await
        .map_err(IntoResponse::into_response)?;
    let owner_id: uuid::Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    if !server.allow_room_creation(&claims.sub) {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            "Room creation is rate limited",
        )
            .into_response()
            .into());
    }

    if let Err(message) = settings::validate_create_request(&req) {
        return Err((StatusCode::BAD_REQUEST, message).into_response().into());
    }
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
            if server.room_manager().drain_signal().is_draining() {
                return (StatusCode::SERVICE_UNAVAILABLE, "Server shutting down").into_response();
            }
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
            crate::db::record_error(&error);
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
) -> Result<StatusCode, RoomApiError> {
    if !settings::valid_room_id(&room_id) {
        return Err((StatusCode::NOT_FOUND, "Room not found")
            .into_response()
            .into());
    }
    if server.db_pool().is_none() {
        return Err(AuthError::NotConfigured.into_response().into());
    }
    let _request_permit = acquire_room_api_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers)
        .await
        .map_err(IntoResponse::into_response)?;
    let user_id: uuid::Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    let result = server
        .room_manager()
        .delete_persisted_room(&room_id, &user_id)
        .await
        .map_err(|error| {
            crate::db::record_error(&error);
            warn!(room_id, %error, "Failed to delete room");
            AuthError::DatabaseError("room deletion failed".to_string()).into_response()
        })?;

    match result {
        DeleteRoomResult::Deleted => Ok(StatusCode::NO_CONTENT),
        DeleteRoomResult::Forbidden | DeleteRoomResult::NotFound => {
            Err((StatusCode::NOT_FOUND, "Room not found")
                .into_response()
                .into())
        }
    }
}

/// GET /api/rooms/mine — includes the owner's private rooms, capped by their quota.
pub async fn owned_rooms(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Vec<RoomListItem>>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers)
        .await
        .map_err(IntoResponse::into_response)?;
    let owner: uuid::Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    let rows = sqlx::query_as::<_, RoomListRow>("SELECT id, display_name, topic, password_hash IS NOT NULL, moderated, description, image_url, secret, name_style, topic_style FROM rooms WHERE owner_id = $1 ORDER BY created_at DESC LIMIT $2")
        .bind(owner).bind(settings::MAX_PERSISTED_ROOMS_PER_OWNER).fetch_all(server.db_pool().ok_or_else(|| AuthError::NotConfigured.into_response())?)
        .await.map_err(room_database_error)?;
    let mut response_headers = HeaderMap::new();
    response_headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    Ok((
        response_headers,
        Json(
            rows.into_iter()
                .map(|row| room_list_item(&server, row))
                .collect(),
        ),
    ))
}

/// PATCH /api/rooms/:id/identity — ownership is rechecked by the database mutation.
pub async fn update_room_identity(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(request): Json<RoomIdentityUpdate>,
) -> Result<Json<RoomListItem>, RoomApiError> {
    if !settings::valid_room_id(&id) {
        return Err((StatusCode::NOT_FOUND, "Room not found")
            .into_response()
            .into());
    }
    let _permit = acquire_room_api_request(&server)?;
    let claims = account::authenticated_claims(&server, &headers)
        .await
        .map_err(IntoResponse::into_response)?;
    request.validate().map_err(IntoResponse::into_response)?;
    let owner =
        uuid::Uuid::parse_str(&claims.sub).map_err(|_| AuthError::InvalidToken.into_response())?;
    if !server
        .room_manager()
        .update_room_identity(&id, owner, request)
        .await
        .map_err(room_database_error)?
    {
        return Err((StatusCode::NOT_FOUND, "Room not found")
            .into_response()
            .into());
    }
    let row = sqlx::query_as::<_, RoomListRow>("SELECT id, display_name, topic, password_hash IS NOT NULL, moderated, description, image_url, secret, name_style, topic_style FROM rooms WHERE id = $1 AND owner_id = $2")
        .bind(&id).bind(owner).fetch_optional(server.db_pool().ok_or_else(|| AuthError::NotConfigured.into_response())?).await
        .map_err(room_database_error)?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Room not found").into_response())?;
    Ok(Json(room_list_item(&server, row)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn compact_api_error_preserves_http_response() {
        let original = (
            StatusCode::SERVICE_UNAVAILABLE,
            [
                (header::RETRY_AFTER, "1"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            "Service busy",
        )
            .into_response();
        let expected_headers = original.headers().clone();
        let response = RoomApiError::from(original).into_response();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(response.headers(), &expected_headers);
        assert_eq!(
            axum::body::to_bytes(response.into_body(), 1024)
                .await
                .unwrap(),
            "Service busy",
        );
    }

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

    /// Live rooms lead every page in busiest-first order, then the dormant rooms
    /// continue newest first, so "busiest first" survives Load more.
    #[tokio::test]
    #[ignore = "requires migrated disposable TEST_DATABASE_URL"]
    async fn database_directory_pages_live_rooms_busiest_first_then_newest() {
        use std::str::FromStr;
        assert_eq!(
            std::env::var("DISPOSABLE_TEST_DATABASE").as_deref(),
            Ok("1")
        );
        let options = sqlx::postgres::PgConnectOptions::from_str(
            &std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"),
        )
        .expect("valid test PostgreSQL URL");
        assert!(matches!(options.get_host(), "127.0.0.1" | "::1"));
        assert!(
            options
                .get_database()
                .is_some_and(|name| name.ends_with("_test"))
        );
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options)
            .await
            .unwrap();
        let owner = uuid::Uuid::new_v4();
        let prefix = format!("directory-{}", &owner.to_string()[..8]);
        sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Directory owner')")
            .bind(owner)
            .bind(format!("{owner}@directory.invalid"))
            .execute(&pool)
            .await
            .unwrap();
        // Six public rooms created oldest to newest, plus a secret one.
        let ids: Vec<String> = (0..6).map(|index| format!("{prefix}-{index}")).collect();
        for id in &ids {
            sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,$1)")
                .bind(id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
        }
        let secret = format!("{prefix}-secret");
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name,secret) VALUES($1,$2,$1,true)")
            .bind(&secret)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        // Live: room 3 is busiest, room 1 next, room 0's count is unknown; the
        // secret room and an ad-hoc room never list.
        let live = vec![
            (ids[1].clone(), Some(2)),
            (ids[3].clone(), Some(5)),
            (ids[0].clone(), None),
            (secret.clone(), Some(9)),
            ("ad-hoc-room-not-in-the-database".to_string(), Some(4)),
        ];
        let pattern = format!("%{prefix}%");
        let page = |limit: i64, offset: i64| {
            let pool = pool.clone();
            let live = live.clone();
            let pattern = pattern.clone();
            async move {
                fetch_directory_page(&pool, &live, Some(&pattern), limit, offset)
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| row.0)
                    .collect::<Vec<String>>()
            }
        };
        let expected = [
            vec![ids[3].clone(), ids[1].clone()],
            vec![ids[0].clone(), ids[5].clone()],
            vec![ids[4].clone(), ids[2].clone()],
            vec![],
        ];
        let mut pages = Vec::new();
        for offset in [0, 2, 4, 6] {
            pages.push(page(2, offset).await);
        }
        let unfiltered = fetch_directory_page(&pool, &live, None, 3, 0)
            .await
            .unwrap()
            .into_iter()
            .map(|row| row.0)
            .collect::<Vec<String>>();
        sqlx::query("DELETE FROM rooms WHERE id LIKE $1")
            .bind(format!("{prefix}%"))
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = $1")
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(pages, expected);
        assert_eq!(
            unfiltered,
            vec![ids[3].clone(), ids[1].clone(), ids[0].clone()],
            "without a search the live rooms still lead the first page"
        );
    }

    #[test]
    fn busiest_first_orders_known_counts_descending_and_unknown_last() {
        let mut counts = vec![Some(2), None, Some(7), Some(0), None, Some(7)];
        counts.sort_by(|a, b| busiest_first(*a, *b));
        assert_eq!(counts, vec![Some(7), Some(7), Some(2), Some(0), None, None]);
    }

    #[test]
    fn page_window_takes_live_rooms_before_dormant_rows() {
        // Three live rooms in pages of two: live+live, live+dormant, dormant+dormant.
        assert_eq!(
            page_window(3, 2, 0),
            PageWindow {
                live: 0..2,
                dormant_limit: 0,
                dormant_offset: 0
            }
        );
        assert_eq!(
            page_window(3, 2, 2),
            PageWindow {
                live: 2..3,
                dormant_limit: 1,
                dormant_offset: 0
            }
        );
        assert_eq!(
            page_window(3, 2, 4),
            PageWindow {
                live: 3..3,
                dormant_limit: 2,
                dormant_offset: 1
            }
        );
        assert_eq!(
            page_window(0, 20, 40),
            PageWindow {
                live: 0..0,
                dormant_limit: 20,
                dormant_offset: 40
            }
        );
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
