#![forbid(unsafe_code)]
//! Room invitations and memberships: an admin mints a code that grants a role
//! in the room, whoever redeems it becomes a member (never demoted), and an
//! account can list the rooms it belongs to.

use crate::auth::{account, types::AuthError};
use crate::invite_codes;
use crate::room::api::{
    RoomApiError, RoomListItem, RoomListRow, acquire_room_api_request, room_database_error,
    room_list_item,
};
use crate::room::roles::{self, Role};
use crate::room::settings;
use crate::signaling::SignalingServer;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

/// Live (unused, unexpired) codes one room may hold.
pub const MAX_LIVE_ROOM_INVITES: i64 = 20;
pub const MAX_INVITE_USES: i32 = 100;
pub const MAX_INVITE_DAYS: i32 = 30;
const DEFAULT_INVITE_DAYS: i32 = 7;
/// Memberships returned on one page; additional memberships use an ID cursor.
const MEMBERSHIP_PAGE_SIZE: usize = 100;
/// Successful redemptions remain retryable until this many days after expiry.
pub(crate) const INVITE_RECEIPT_RETENTION_DAYS: i32 = 7;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RoomInvite {
    pub id: Uuid,
    #[sqlx(try_from = "i16")]
    pub role: InviteRole,
    pub uses_left: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

/// The secret is returned once; subsequent listings contain metadata only.
#[derive(Debug, Serialize)]
pub struct CreatedRoomInvite {
    #[serde(flatten)]
    pub details: RoomInvite,
    pub code: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InviteCodeRequest {
    pub code: String,
}

/// A stored role value as its name; the table's CHECK keeps it in range.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InviteRole(pub Role);

impl TryFrom<i16> for InviteRole {
    type Error = String;
    fn try_from(value: i16) -> Result<Self, Self::Error> {
        Ok(Self(Role::from_db(value)))
    }
}

impl Serialize for InviteRole {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.name())
    }
}

#[derive(Debug, Deserialize)]
pub struct CreateRoomInviteRequest {
    pub role: u8,
    pub uses: Option<i32>,
    pub days: Option<i32>,
}

#[derive(Debug, Serialize)]
pub struct InviteRedemption {
    pub room_id: String,
    pub display_name: String,
    pub role: &'static str,
}

#[derive(Serialize)]
pub struct MembershipItem {
    #[serde(flatten)]
    pub room: RoomListItem,
    pub role: &'static str,
}

#[derive(Serialize)]
pub struct MembershipPage {
    pub items: Vec<MembershipItem>,
    pub next_cursor: Option<String>,
}

/// Existing open clients receive their array shape until they opt into paging.
#[derive(Serialize)]
#[serde(untagged)]
pub enum MembershipResponse {
    Legacy(Vec<MembershipItem>),
    Page(MembershipPage),
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MembershipParams {
    pub after: Option<String>,
    #[serde(default)]
    pub paginated: bool,
}

fn membership_response(params: &MembershipParams, page: MembershipPage) -> MembershipResponse {
    if params.paginated || params.after.is_some() {
        MembershipResponse::Page(page)
    } else {
        MembershipResponse::Legacy(page.items)
    }
}

/// Mint a code for `room_id`; `None` when the room already holds its limit.
pub async fn create_room_invite(
    pool: &PgPool,
    room_id: &str,
    role: Role,
    uses: i32,
    days: i32,
    created_by: Uuid,
) -> Result<Option<CreatedRoomInvite>, sqlx::Error> {
    let code =
        invite_codes::generate().map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
    let mut transaction = pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 71340))")
        .bind(room_id)
        .execute(&mut *transaction)
        .await?;
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM invites
         WHERE room_id = $1 AND uses_left > 0 AND expires_at > now()",
    )
    .bind(room_id)
    .fetch_one(&mut *transaction)
    .await?;
    if live >= MAX_LIVE_ROOM_INVITES {
        return Ok(None);
    }
    let invite: RoomInvite = sqlx::query_as(
        "INSERT INTO invites (code_hash, kind, room_id, role, created_by, uses_left, expires_at)
         VALUES ($1, 'room', $2, $3, $4, $5, now() + make_interval(days => $6))
         RETURNING id, role, uses_left, expires_at, created_at",
    )
    .bind(invite_codes::digest(&code))
    .bind(room_id)
    .bind(role.to_db())
    .bind(created_by)
    .bind(uses)
    .bind(days)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Some(CreatedRoomInvite {
        details: invite,
        code,
    }))
}

pub async fn list_room_invites(
    pool: &PgPool,
    room_id: &str,
) -> Result<Vec<RoomInvite>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, role, uses_left, expires_at, created_at FROM invites
         WHERE room_id = $1 AND uses_left > 0 AND expires_at > now()
         ORDER BY created_at DESC, id",
    )
    .bind(room_id)
    .fetch_all(pool)
    .await
}

pub async fn revoke_room_invite(
    pool: &PgPool,
    room_id: &str,
    id: Uuid,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM invites WHERE room_id = $1 AND id = $2 AND kind = 'room'")
            .bind(room_id)
            .bind(id)
            .execute(pool)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Spend one use of a room code for `user_id` and grant its role, never a
/// lower one than the account already holds; the owner keeps ownership.
/// A successful account/code pair replays its receipt until seven days after
/// expiry, without spending again or changing a subsequently edited membership.
/// The returned role is the original result, not a current authorization check.
/// `None` when no receipt exists and the code is unknown, exhausted or expired;
/// revocation and receipt expiry also make previous successes unavailable.
pub async fn redeem_room_invite(
    pool: &PgPool,
    code: &str,
    user_id: Uuid,
) -> Result<Option<(String, String, Role)>, sqlx::Error> {
    let hash = invite_codes::digest(code);
    let mut transaction = pool.begin().await?;
    // Serialize all attempts against this code before checking the receipt.
    // A repeated successful request never spends again or re-grants a role that
    // a moderator has subsequently changed.
    let invitation: Option<(String, i16, Uuid, i32, bool)> = sqlx::query_as(
        "SELECT room_id, role, created_by, uses_left, expires_at > clock_timestamp()
         FROM invites WHERE code_hash = $1 AND kind = 'room'
           AND expires_at > clock_timestamp() - make_interval(days => $2)
         FOR UPDATE",
    )
    .bind(&hash)
    .bind(INVITE_RECEIPT_RETENTION_DAYS)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((room_id, role, created_by, uses_left, live)) = invitation else {
        return Ok(None);
    };
    let room: Option<(Uuid, String)> =
        sqlx::query_as("SELECT owner_id, display_name FROM rooms WHERE id = $1")
            .bind(&room_id)
            .fetch_optional(&mut *transaction)
            .await?;
    let Some((owner_id, display_name)) = room else {
        return Ok(None);
    };
    let receipt: Option<i16> = sqlx::query_scalar(
        "SELECT granted_role FROM invite_redemptions WHERE invite_hash = $1 AND user_id = $2",
    )
    .bind(&hash)
    .bind(user_id)
    .fetch_optional(&mut *transaction)
    .await?;
    if let Some(role) = receipt {
        let role = u8::try_from(role)
            .ok()
            .and_then(Role::from_u8)
            .ok_or_else(|| sqlx::Error::Protocol("invalid invitation receipt role".into()))?;
        transaction.commit().await?;
        return Ok(Some((room_id, display_name, role)));
    }
    if !live || uses_left == 0 {
        return Ok(None);
    }
    let granted = if owner_id == user_id {
        Role::Owner
    } else {
        let stored: i16 = sqlx::query_scalar(
            "INSERT INTO room_roles (room_id, user_id, role, granted_by)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (room_id, user_id) DO UPDATE
               SET role = GREATEST(room_roles.role, EXCLUDED.role),
                   granted_by = CASE WHEN EXCLUDED.role > room_roles.role
                                     THEN EXCLUDED.granted_by ELSE room_roles.granted_by END
             RETURNING role",
        )
        .bind(&room_id)
        .bind(user_id)
        .bind(role)
        .bind(created_by)
        .fetch_one(&mut *transaction)
        .await?;
        Role::from_db(stored)
    };
    sqlx::query("UPDATE invites SET uses_left = uses_left - 1 WHERE code_hash = $1")
        .bind(&hash)
        .execute(&mut *transaction)
        .await?;
    sqlx::query(
        "INSERT INTO invite_redemptions (invite_hash, user_id, granted_role) VALUES ($1, $2, $3)",
    )
    .bind(&hash)
    .bind(user_id)
    .bind(granted as i16)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Some((room_id, display_name, granted)))
}

/// Read-only preview; possession of the code and an authenticated account may
/// reveal the destination label and offered role, but never joins or grants it.
/// A retained receipt remains previewable for the same account on a retry.
async fn preview_room_invite(
    pool: &PgPool,
    code: &str,
    user_id: Uuid,
) -> Result<Option<(String, String, i16)>, sqlx::Error> {
    sqlx::query_as(
        "SELECT i.room_id, r.display_name, i.role FROM invites i
         JOIN rooms r ON r.id = i.room_id
         LEFT JOIN invite_redemptions receipt ON receipt.invite_hash = i.code_hash AND receipt.user_id = $2
         WHERE i.code_hash = $1 AND i.kind = 'room'
           AND ((i.uses_left > 0 AND i.expires_at > now())
             OR (receipt.user_id IS NOT NULL AND i.expires_at > now() - make_interval(days => $3)))",
    )
    .bind(invite_codes::digest(code))
    .bind(user_id)
    .bind(INVITE_RECEIPT_RETENTION_DAYS)
    .fetch_optional(pool)
    .await
}

/// A directory row joined with the account's stored role.
type MembershipRow = (
    String,
    String,
    Option<String>,
    bool,
    bool,
    String,
    Option<String>,
    bool,
    i16,
);

pub struct MembershipBatch {
    pub rows: Vec<(RoomListRow, i16)>,
    pub next_cursor: Option<String>,
}

/// A stable ID-ordered membership page, excluding rooms owned by the account.
/// Room renames do not shift the pagination boundary.
pub async fn memberships(
    pool: &PgPool,
    user_id: Uuid,
    after: Option<&str>,
) -> Result<MembershipBatch, sqlx::Error> {
    let mut rows: Vec<MembershipRow> = sqlx::query_as(
        "SELECT r.id, r.display_name, r.topic, r.password_hash IS NOT NULL, r.moderated,
                    r.description, r.image_url, r.secret, rr.role
             FROM room_roles rr JOIN rooms r ON r.id = rr.room_id
             WHERE rr.user_id = $1 AND r.owner_id <> $1
               AND ($3::text IS NULL OR r.id > $3)
             ORDER BY r.id LIMIT $2",
    )
    .bind(user_id)
    .bind(MEMBERSHIP_PAGE_SIZE as i64 + 1)
    .bind(after)
    .fetch_all(pool)
    .await?;
    let has_more = rows.len() > MEMBERSHIP_PAGE_SIZE;
    rows.truncate(MEMBERSHIP_PAGE_SIZE);
    let next_cursor = has_more.then(|| rows.last().expect("full membership page").0.clone());
    Ok(MembershipBatch {
        next_cursor,
        rows: rows
            .into_iter()
            .map(
                |(id, name, topic, password, moderated, description, image, secret, role)| {
                    (
                        (
                            id,
                            name,
                            topic,
                            password,
                            moderated,
                            description,
                            image,
                            secret,
                        ),
                        role,
                    )
                },
            )
            .collect(),
    })
}

fn not_found() -> RoomApiError {
    (StatusCode::NOT_FOUND, "Room not found")
        .into_response()
        .into()
}

fn forbidden(message: &'static str) -> RoomApiError {
    (StatusCode::FORBIDDEN, message).into_response().into()
}

fn bad_request(message: &'static str) -> RoomApiError {
    (StatusCode::BAD_REQUEST, message).into_response().into()
}

fn private_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers
}

async fn caller(
    server: &SignalingServer,
    headers: &HeaderMap,
) -> Result<(Uuid, PgPool), RoomApiError> {
    let claims = account::authenticated_claims(server, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    let user: Uuid = claims
        .sub
        .parse()
        .map_err(|_| AuthError::InvalidToken.into_response())?;
    let pool = server
        .db_pool()
        .ok_or_else(|| AuthError::NotConfigured.into_response())?
        .clone();
    Ok((user, pool))
}

/// The caller's role in the room, or a 404 when there is no such room.
async fn room_role(pool: &PgPool, room_id: &str, user: Uuid) -> Result<Role, RoomApiError> {
    if !settings::valid_room_id(room_id) {
        return Err(not_found());
    }
    let owner: Option<Uuid> = sqlx::query_scalar("SELECT owner_id FROM rooms WHERE id = $1")
        .bind(room_id)
        .fetch_optional(pool)
        .await
        .map_err(room_database_error)?;
    let owner = owner.ok_or_else(not_found)?;
    roles::resolve_role(pool, room_id, Some(&user), &owner, true)
        .await
        .map_err(|error| RoomApiError::from(room_database_error(error)))
}

/// GET /api/rooms/:id/invites (Admin+)
pub async fn list(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
) -> Result<(HeaderMap, Json<Vec<RoomInvite>>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    if room_role(&pool, &room_id, user).await? < Role::Admin {
        return Err(forbidden("Only room admins manage invitations"));
    }
    let invites = list_room_invites(&pool, &room_id)
        .await
        .map_err(room_database_error)?;
    Ok((private_headers(), Json(invites)))
}

/// POST /api/rooms/:id/invites (Admin+; a role below the caller's own)
pub async fn create(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(room_id): Path<String>,
    Json(request): Json<CreateRoomInviteRequest>,
) -> Result<(HeaderMap, Json<CreatedRoomInvite>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let granting = Role::from_u8(request.role).ok_or_else(|| bad_request("Choose a role"))?;
    if !matches!(granting, Role::Member | Role::Moderator | Role::Admin) {
        return Err(bad_request(
            "An invitation grants member, moderator or admin",
        ));
    }
    let uses = request.uses.unwrap_or(1);
    let days = request.days.unwrap_or(DEFAULT_INVITE_DAYS);
    if !(1..=MAX_INVITE_USES).contains(&uses) || !(1..=MAX_INVITE_DAYS).contains(&days) {
        return Err(bad_request("Uses must be 1–100 and validity 1–30 days"));
    }
    let role = room_role(&pool, &room_id, user).await?;
    if role < Role::Admin || !role.can_set_role(Role::User, granting) {
        return Err(forbidden(
            "Only room admins invite, and only to roles below their own",
        ));
    }
    let invite = create_room_invite(&pool, &room_id, granting, uses, days, user)
        .await
        .map_err(room_database_error)?
        .ok_or_else(|| {
            Response::from(
                (
                    StatusCode::CONFLICT,
                    "This room already has 20 unused invitations",
                )
                    .into_response(),
            )
        })?;
    Ok((private_headers(), Json(invite)))
}

/// DELETE /api/rooms/:id/invites/:invite_id (Admin+)
pub async fn revoke(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path((room_id, invite_id)): Path<(String, Uuid)>,
) -> Result<StatusCode, RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    if room_role(&pool, &room_id, user).await? < Role::Admin {
        return Err(forbidden("Only room admins manage invitations"));
    }
    if !revoke_room_invite(&pool, &room_id, invite_id)
        .await
        .map_err(room_database_error)?
    {
        return Err((StatusCode::NOT_FOUND, "Invite not found")
            .into_response()
            .into());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/rooms/invites/redeem — explicitly accept a body-only secret.
pub async fn redeem(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<InviteCodeRequest>,
) -> Result<(HeaderMap, Json<InviteRedemption>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let code =
        invite_codes::normalize(&request.code).ok_or_else(|| bad_request("Invalid invite code"))?;
    let (room_id, display_name, role) = redeem_room_invite(&pool, &code, user)
        .await
        .map_err(room_database_error)?
        .ok_or_else(|| {
            Response::from(
                (
                    StatusCode::NOT_FOUND,
                    "This invitation is invalid, used up or expired",
                )
                    .into_response(),
            )
        })?;
    Ok((
        private_headers(),
        Json(InviteRedemption {
            room_id,
            display_name,
            role: role.name(),
        }),
    ))
}

/// POST /api/rooms/invites/preview — inspect before explicit acceptance.
pub async fn preview(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Json(request): Json<InviteCodeRequest>,
) -> Result<(HeaderMap, Json<InviteRedemption>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let code =
        invite_codes::normalize(&request.code).ok_or_else(|| bad_request("Invalid invite code"))?;
    let (room_id, display_name, role) = preview_room_invite(&pool, &code, user)
        .await
        .map_err(room_database_error)?
        .ok_or_else(|| {
            RoomApiError::from(
                (
                    StatusCode::NOT_FOUND,
                    "This invitation is invalid, used up or expired",
                )
                    .into_response(),
            )
        })?;
    Ok((
        private_headers(),
        Json(InviteRedemption {
            room_id,
            display_name,
            role: Role::from_db(role).name(),
        }),
    ))
}

/// GET /api/rooms/memberships — rooms the account belongs to but does not own.
pub async fn list_memberships(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Query(params): Query<MembershipParams>,
) -> Result<(HeaderMap, Json<MembershipResponse>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    if params
        .after
        .as_deref()
        .is_some_and(|cursor| !settings::valid_room_id(cursor))
    {
        return Err(bad_request("Invalid membership cursor"));
    }
    let batch = memberships(&pool, user, params.after.as_deref())
        .await
        .map_err(room_database_error)?;
    Ok((
        private_headers(),
        Json(membership_response(
            &params,
            MembershipPage {
                next_cursor: batch.next_cursor,
                items: batch
                    .rows
                    .into_iter()
                    .map(|(row, role)| MembershipItem {
                        room: room_list_item(&server, row),
                        role: Role::from_db(role).name(),
                    })
                    .collect(),
            },
        )),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_paging_is_opt_in_for_older_open_clients() {
        for (uri, paged) in [
            ("/api/rooms/memberships", false),
            ("/api/rooms/memberships?paginated=false", false),
            ("/api/rooms/memberships?paginated=true", true),
            ("/api/rooms/memberships?after=room-100", true),
            (
                "/api/rooms/memberships?paginated=false&after=room-100",
                true,
            ),
        ] {
            let Query(params) =
                Query::<MembershipParams>::try_from_uri(&uri.parse().unwrap()).unwrap();
            let response = membership_response(
                &params,
                MembershipPage {
                    items: Vec::new(),
                    next_cursor: None,
                },
            );
            let json = serde_json::to_value(response).unwrap();
            if paged {
                assert_eq!(json, serde_json::json!({"items": [], "next_cursor": null}));
            } else {
                assert_eq!(json, serde_json::json!([]));
            }
        }
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_room_invites_grant_roles_once_each_without_demoting_anyone() {
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let owner = Uuid::new_v4();
        let alice = Uuid::new_v4();
        let bob = Uuid::new_v4();
        for (id, name) in [(owner, "Owner"), (alice, "Alice"), (bob, "Bob")] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,$3)")
                .bind(id)
                .bind(format!("{id}@invites.invalid"))
                .bind(name)
                .execute(&pool)
                .await
                .unwrap();
        }
        let room_id = format!("invites-{}", Uuid::new_v4());
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) VALUES($1,$2,'Invited room')")
            .bind(&room_id)
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();
        roles::set_role(&pool, &room_id, &bob, Role::Moderator, &owner)
            .await
            .unwrap();

        let invite = create_room_invite(&pool, &room_id, Role::Member, 2, 7, owner)
            .await
            .unwrap()
            .unwrap();
        assert!(invite_codes::is_valid(&invite.code));
        assert_eq!(
            (invite.details.role.0, invite.details.uses_left),
            (Role::Member, 2)
        );
        assert_eq!(list_room_invites(&pool, &room_id).await.unwrap().len(), 1);

        let listed =
            serde_json::to_value(list_room_invites(&pool, &room_id).await.unwrap()).unwrap();
        assert!(listed[0].get("code").is_none());
        assert!(listed[0].get("code_hash").is_none());
        assert_eq!(listed[0]["id"], invite.details.id.to_string());
        let stored: String = sqlx::query_scalar("SELECT code_hash FROM invites WHERE id = $1")
            .bind(invite.details.id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(stored, invite_codes::digest(&invite.code));
        assert_ne!(stored, invite.code);
        for _ in 0..2 {
            assert_eq!(
                preview_room_invite(&pool, &invite.code, alice)
                    .await
                    .unwrap(),
                Some((room_id.clone(), "Invited room".into(), Role::Member.to_db()))
            );
        }
        assert_eq!(
            list_room_invites(&pool, &room_id).await.unwrap()[0].uses_left,
            2
        );
        assert_eq!(
            roles::resolve_role(&pool, &room_id, Some(&alice), &owner, true)
                .await
                .unwrap(),
            Role::User
        );

        // Alice becomes a member; a lost-response retry must not spend twice.
        let (redeemed_room, name, role) = redeem_room_invite(&pool, &invite.code, alice)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (redeemed_room.as_str(), name.as_str(), role),
            (room_id.as_str(), "Invited room", Role::Member)
        );
        assert_eq!(
            roles::resolve_role(&pool, &room_id, Some(&alice), &owner, true)
                .await
                .unwrap(),
            Role::Member
        );
        let repeated = redeem_room_invite(&pool, &invite.code, alice)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            repeated,
            (room_id.clone(), "Invited room".into(), Role::Member)
        );
        assert_eq!(
            list_room_invites(&pool, &room_id).await.unwrap()[0].uses_left,
            1
        );
        // Bob is already a moderator: the code never demotes him.
        let (_, _, role) = redeem_room_invite(&pool, &invite.code, bob)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role, Role::Moderator);
        assert!(
            redeem_room_invite(&pool, &invite.code, alice)
                .await
                .unwrap()
                .is_some(),
            "a spent code still replays its successful receipt"
        );
        assert!(list_room_invites(&pool, &room_id).await.unwrap().is_empty());

        // The owner stays the owner, an expired code is refused, revocation removes a live one.
        let expired = create_room_invite(&pool, &room_id, Role::Admin, 1, 1, owner)
            .await
            .unwrap()
            .unwrap();
        let (_, _, role) = redeem_room_invite(&pool, &expired.code, owner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(role, Role::Owner);
        let fresh = create_room_invite(&pool, &room_id, Role::Admin, 1, 1, owner)
            .await
            .unwrap()
            .unwrap();
        sqlx::query(
            "UPDATE invites SET expires_at = now() - interval '1 minute' WHERE code_hash = $1",
        )
        .bind(invite_codes::digest(&fresh.code))
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            redeem_room_invite(&pool, &fresh.code, alice)
                .await
                .unwrap()
                .is_none()
        );
        let live = create_room_invite(&pool, &room_id, Role::Moderator, 5, 30, owner)
            .await
            .unwrap()
            .unwrap();
        assert!(
            revoke_room_invite(&pool, &room_id, live.details.id)
                .await
                .unwrap()
        );
        assert!(
            !revoke_room_invite(&pool, &room_id, live.details.id)
                .await
                .unwrap()
        );

        // Concurrent retries of a single-use code both observe one committed receipt.
        let retry = create_room_invite(&pool, &room_id, Role::Member, 1, 1, owner)
            .await
            .unwrap()
            .unwrap();
        let (first, second) = tokio::join!(
            redeem_room_invite(&pool, &retry.code, alice),
            redeem_room_invite(&pool, &retry.code, alice),
        );
        let first = first
            .unwrap()
            .expect("initial single-use redemption succeeds");
        assert_eq!(first.2, Role::Member);
        assert_eq!(Some(first), second.unwrap());
        let remaining: i32 =
            sqlx::query_scalar("SELECT uses_left FROM invites WHERE code_hash = $1")
                .bind(invite_codes::digest(&retry.code))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(remaining, 0);
        // A role removed after redemption is never restored by a retry.
        sqlx::query("DELETE FROM room_roles WHERE room_id = $1 AND user_id = $2")
            .bind(&room_id)
            .bind(alice)
            .execute(&pool)
            .await
            .unwrap();
        assert!(
            redeem_room_invite(&pool, &retry.code, alice)
                .await
                .unwrap()
                .is_some()
        );
        let roles: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM room_roles WHERE room_id = $1 AND user_id = $2",
        )
        .bind(&room_id)
        .bind(alice)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(roles, 0);
        // Receipt retries survive expiry for seven days; new users cannot redeem them.
        sqlx::query(
            "UPDATE invites SET expires_at = now() - interval '1 day' WHERE code_hash = $1",
        )
        .bind(invite_codes::digest(&retry.code))
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            redeem_room_invite(&pool, &retry.code, alice)
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            redeem_room_invite(&pool, &retry.code, bob)
                .await
                .unwrap()
                .is_none()
        );
        sqlx::query(
            "UPDATE invites SET expires_at = now() - interval '8 days' WHERE code_hash = $1",
        )
        .bind(invite_codes::digest(&retry.code))
        .execute(&pool)
        .await
        .unwrap();
        assert!(
            redeem_room_invite(&pool, &retry.code, alice)
                .await
                .unwrap()
                .is_none()
        );
        // Explicit revocation immediately invalidates an existing receipt too.
        assert!(
            revoke_room_invite(&pool, &room_id, invite.details.id)
                .await
                .unwrap()
        );
        assert!(
            redeem_room_invite(&pool, &invite.code, alice)
                .await
                .unwrap()
                .is_none()
        );
        let receipts: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM invite_redemptions WHERE invite_hash = $1")
                .bind(invite_codes::digest(&invite.code))
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(receipts, 0);
        sqlx::query("INSERT INTO room_roles(room_id,user_id,role,granted_by) VALUES($1,$2,$3,$4)")
            .bind(&room_id)
            .bind(alice)
            .bind(Role::Member.to_db())
            .bind(owner)
            .execute(&pool)
            .await
            .unwrap();

        // The room holds twenty live codes at most.
        for _ in 0..MAX_LIVE_ROOM_INVITES {
            assert!(
                create_room_invite(&pool, &room_id, Role::Member, 1, 7, owner)
                    .await
                    .unwrap()
                    .is_some()
            );
        }
        assert!(
            create_room_invite(&pool, &room_id, Role::Member, 1, 7, owner)
                .await
                .unwrap()
                .is_none()
        );

        // Memberships list the rooms an account belongs to, not the ones it owns.
        let mine = memberships(&pool, alice, None).await.unwrap();
        assert_eq!(mine.rows.len(), 1);
        assert!(mine.next_cursor.is_none());
        assert_eq!(
            (mine.rows[0].0.0.as_str(), mine.rows[0].1),
            (room_id.as_str(), Role::Member.to_db())
        );
        assert!(
            memberships(&pool, owner, None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );

        sqlx::query("DELETE FROM rooms WHERE id=$1")
            .bind(&room_id)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id = ANY($1)")
            .bind(vec![owner, alice, bob])
            .execute(&pool)
            .await
            .unwrap();
    }

    #[tokio::test]
    #[ignore = "requires TEST_DATABASE_URL pointing to a migrated disposable PostgreSQL database"]
    async fn database_memberships_paginate_every_room_and_ignore_display_name_changes() {
        let pool =
            sqlx::PgPool::connect(&std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL"))
                .await
                .unwrap();
        let owner = Uuid::new_v4();
        let member = Uuid::new_v4();
        for id in [owner, member] {
            sqlx::query("INSERT INTO users(id,email,display_name) VALUES($1,$2,'Paging account')")
                .bind(id)
                .bind(format!("{id}@membership.invalid"))
                .execute(&pool)
                .await
                .unwrap();
        }
        let prefix = format!("pages-{}-", Uuid::new_v4());
        let rooms: Vec<String> = (0..205)
            .map(|index| format!("{prefix}{index:03}"))
            .collect();
        sqlx::query("INSERT INTO rooms(id,owner_id,display_name) SELECT id,$2,'Same name' FROM unnest($1::text[]) AS id")
            .bind(&rooms).bind(owner).execute(&pool).await.unwrap();
        sqlx::query("INSERT INTO room_roles(room_id,user_id,role,granted_by) SELECT id,$2,1,$3 FROM unnest($1::text[]) AS id")
            .bind(&rooms).bind(member).bind(owner).execute(&pool).await.unwrap();
        let first = memberships(&pool, member, None).await.unwrap();
        assert_eq!(first.rows.len(), 100);
        assert_eq!(first.next_cursor.as_ref(), Some(&rooms[99]));
        sqlx::query("UPDATE rooms SET display_name='A new name',updated_at=now() WHERE id=ANY($1)")
            .bind(&rooms)
            .execute(&pool)
            .await
            .unwrap();
        let second = memberships(&pool, member, first.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(second.rows.len(), 100);
        assert_eq!(second.next_cursor.as_ref(), Some(&rooms[199]));
        let third = memberships(&pool, member, second.next_cursor.as_deref())
            .await
            .unwrap();
        assert_eq!(third.rows.len(), 5);
        assert!(third.next_cursor.is_none());
        let all: Vec<String> = first
            .rows
            .into_iter()
            .chain(second.rows)
            .chain(third.rows)
            .map(|(row, _)| row.0)
            .collect();
        assert_eq!(all, rooms);
        assert!(
            memberships(&pool, member, rooms.last().map(String::as_str))
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        assert!(
            memberships(&pool, owner, None)
                .await
                .unwrap()
                .rows
                .is_empty()
        );
        sqlx::query("DELETE FROM rooms WHERE id=ANY($1)")
            .bind(&rooms)
            .execute(&pool)
            .await
            .unwrap();
        sqlx::query("DELETE FROM users WHERE id=ANY($1)")
            .bind(vec![owner, member])
            .execute(&pool)
            .await
            .unwrap();
        pool.close().await;
    }
}
