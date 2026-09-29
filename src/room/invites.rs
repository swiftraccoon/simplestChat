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
    extract::{Path, State},
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
/// Rooms one account is listed as belonging to.
const MAX_MEMBERSHIPS: i64 = 100;

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct RoomInvite {
    pub code: String,
    #[sqlx(try_from = "i16")]
    pub role: InviteRole,
    pub uses_left: i32,
    pub expires_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
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

/// Mint a code for `room_id`; `None` when the room already holds its limit.
pub async fn create_room_invite(
    pool: &PgPool,
    room_id: &str,
    role: Role,
    uses: i32,
    days: i32,
    created_by: Uuid,
) -> Result<Option<RoomInvite>, sqlx::Error> {
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
        "INSERT INTO invites (code, kind, room_id, role, created_by, uses_left, expires_at)
         VALUES ($1, 'room', $2, $3, $4, $5, now() + make_interval(days => $6))
         RETURNING code, role, uses_left, expires_at, created_at",
    )
    .bind(&code)
    .bind(room_id)
    .bind(role.to_db())
    .bind(created_by)
    .bind(uses)
    .bind(days)
    .fetch_one(&mut *transaction)
    .await?;
    transaction.commit().await?;
    Ok(Some(invite))
}

pub async fn list_room_invites(
    pool: &PgPool,
    room_id: &str,
) -> Result<Vec<RoomInvite>, sqlx::Error> {
    sqlx::query_as(
        "SELECT code, role, uses_left, expires_at, created_at FROM invites
         WHERE room_id = $1 AND uses_left > 0 AND expires_at > now()
         ORDER BY created_at DESC, code",
    )
    .bind(room_id)
    .fetch_all(pool)
    .await
}

pub async fn revoke_room_invite(
    pool: &PgPool,
    room_id: &str,
    code: &str,
) -> Result<bool, sqlx::Error> {
    Ok(
        sqlx::query("DELETE FROM invites WHERE room_id = $1 AND code = $2 AND kind = 'room'")
            .bind(room_id)
            .bind(code)
            .execute(pool)
            .await?
            .rows_affected()
            > 0,
    )
}

/// Spend one use of a room code for `user_id` and grant its role, never a
/// lower one than the account already holds; the owner keeps ownership.
/// `None` when the code is unknown, exhausted or expired.
pub async fn redeem_room_invite(
    pool: &PgPool,
    code: &str,
    user_id: Uuid,
) -> Result<Option<(String, String, Role)>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let redeemed: Option<(String, i16, Uuid)> = sqlx::query_as(
        "UPDATE invites SET uses_left = uses_left - 1
         WHERE code = $1 AND kind = 'room' AND uses_left > 0 AND expires_at > now()
         RETURNING room_id, role, created_by",
    )
    .bind(code)
    .fetch_optional(&mut *transaction)
    .await?;
    let Some((room_id, role, created_by)) = redeemed else {
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
    transaction.commit().await?;
    Ok(Some((room_id, display_name, granted)))
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

/// Rooms the account holds a role in, other than the ones it owns.
pub async fn memberships(
    pool: &PgPool,
    user_id: Uuid,
) -> Result<Vec<(RoomListRow, i16)>, sqlx::Error> {
    let rows: Vec<MembershipRow> = sqlx::query_as(
        "SELECT r.id, r.display_name, r.topic, r.password_hash IS NOT NULL, r.moderated,
                    r.description, r.image_url, r.secret, rr.role
             FROM room_roles rr JOIN rooms r ON r.id = rr.room_id
             WHERE rr.user_id = $1 AND r.owner_id <> $1
             ORDER BY r.display_name, r.id LIMIT $2",
    )
    .bind(user_id)
    .bind(MAX_MEMBERSHIPS)
    .fetch_all(pool)
    .await?;
    Ok(rows
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
        .collect())
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
) -> Result<(HeaderMap, Json<RoomInvite>), RoomApiError> {
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

/// DELETE /api/rooms/:id/invites/:code (Admin+)
pub async fn revoke(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path((room_id, code)): Path<(String, String)>,
) -> Result<StatusCode, RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    if room_role(&pool, &room_id, user).await? < Role::Admin {
        return Err(forbidden("Only room admins manage invitations"));
    }
    let code = invite_codes::normalize(&code).ok_or_else(|| bad_request("Invalid invite code"))?;
    if !revoke_room_invite(&pool, &room_id, &code)
        .await
        .map_err(room_database_error)?
    {
        return Err((StatusCode::NOT_FOUND, "Invite not found")
            .into_response()
            .into());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// POST /api/rooms/invites/:code — accept a room invitation.
pub async fn redeem(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
    Path(code): Path<String>,
) -> Result<(HeaderMap, Json<InviteRedemption>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let code = invite_codes::normalize(&code).ok_or_else(|| bad_request("Invalid invite code"))?;
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

/// GET /api/rooms/memberships — rooms the account belongs to but does not own.
pub async fn list_memberships(
    State(server): State<SignalingServer>,
    headers: HeaderMap,
) -> Result<(HeaderMap, Json<Vec<MembershipItem>>), RoomApiError> {
    let _permit = acquire_room_api_request(&server)?;
    let (user, pool) = caller(&server, &headers).await?;
    let rows = memberships(&pool, user)
        .await
        .map_err(room_database_error)?;
    Ok((
        private_headers(),
        Json(
            rows.into_iter()
                .map(|(row, role)| MembershipItem {
                    room: room_list_item(&server, row),
                    role: Role::from_db(role).name(),
                })
                .collect(),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!((invite.role.0, invite.uses_left), (Role::Member, 2));
        assert_eq!(list_room_invites(&pool, &room_id).await.unwrap().len(), 1);

        // Alice becomes a member; redeeming again changes nothing but the count.
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
                .is_none(),
            "two uses, spent"
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
        sqlx::query("UPDATE invites SET expires_at = now() - interval '1 minute' WHERE code = $1")
            .bind(&fresh.code)
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
            revoke_room_invite(&pool, &room_id, &live.code)
                .await
                .unwrap()
        );
        assert!(
            !revoke_room_invite(&pool, &room_id, &live.code)
                .await
                .unwrap()
        );

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
        let mine = memberships(&pool, alice).await.unwrap();
        assert_eq!(mine.len(), 1);
        assert_eq!(
            (mine[0].0.0.as_str(), mine[0].1),
            (room_id.as_str(), Role::Member.to_db())
        );
        assert!(memberships(&pool, owner).await.unwrap().is_empty());

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
}
