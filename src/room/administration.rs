//! Account-authorized administration of saved rooms without joining a call.
//!
//! Creation, runtime control and database authorization have one order. An
//! admitted mutation owns its commit and publication even when HTTP disconnects.
use super::{IdentityUpdateGuard, Room, RoomManager, api, control, moderation, roles, settings};
use crate::auth::{
    account, sessions,
    types::{AuthError, Claims},
};
use crate::signaling::{SignalingServer, protocol::ServerMessage};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::IntoResponse,
};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::{PgConnection, Postgres, Transaction};
use std::{net::IpAddr, sync::Arc};
use tokio::sync::{OwnedMutexGuard, RwLock};
use uuid::Uuid;

const PAGE_SIZE: usize = 100;
const MAX_OFFSET: u32 = 10_000;

#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SettingsPatch {
    pub moderated: Option<bool>,
    pub lobby_enabled: Option<bool>,
    pub guests_allowed: Option<bool>,
    pub guests_can_broadcast: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub max_broadcasters: Option<Option<i32>>,
    #[serde(default, deserialize_with = "nullable")]
    pub max_participants: Option<Option<i32>>,
    pub allow_screen_sharing: Option<bool>,
    pub allow_chat: Option<bool>,
    pub allow_video: Option<bool>,
    pub require_registration: Option<bool>,
    pub invite_only: Option<bool>,
    pub push_to_talk: Option<bool>,
    pub secret: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    pub password: Option<Option<String>>,
}

fn nullable<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

impl SettingsPatch {
    fn apply(
        &self,
        current: &settings::RoomSettings,
    ) -> Result<settings::RoomSettings, sqlx::Error> {
        for (value, maximum) in [
            (self.max_participants, 10_000),
            (self.max_broadcasters, 1_000),
        ] {
            if value
                .flatten()
                .is_some_and(|value| !(1..=maximum).contains(&value))
            {
                return Err(invalid(
                    "Room limits must be positive and within server bounds",
                ));
            }
        }
        if self
            .password
            .as_ref()
            .and_then(Option::as_ref)
            .is_some_and(|value| !settings::valid_new_room_password(value))
        {
            return Err(invalid(
                "Room password must be 8–256 bytes without control characters",
            ));
        }
        let mut updated = current.clone();
        settings::apply_settings_update(
            &mut updated,
            self.moderated,
            self.lobby_enabled,
            self.guests_allowed,
            self.guests_can_broadcast,
            self.max_broadcasters,
            self.max_participants,
            self.allow_screen_sharing,
            self.allow_chat,
            self.allow_video,
            self.require_registration,
            self.invite_only,
            self.push_to_talk,
            self.secret,
            self.password.clone(),
        );
        if matches!((updated.max_broadcasters, updated.max_participants), (Some(broadcasters), Some(participants)) if broadcasters > participants)
        {
            return Err(invalid(
                "Maximum broadcasters cannot exceed maximum participants",
            ));
        }
        Ok(updated)
    }
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub offset: Option<u32>,
}

impl PageQuery {
    fn offset(&self) -> Result<i64, sqlx::Error> {
        let value = self.offset.unwrap_or(0);
        if value > MAX_OFFSET {
            return Err(invalid("Invalid administration page"));
        }
        Ok(i64::from(value))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleRequest {
    pub role: u8,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReportRequest {
    pub status: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LobbyRequest {
    pub action: String,
    pub reason: Option<String>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SanctionRequest {
    pub target_participant_id: String,
    pub kind: String,
    pub enabled: bool,
    pub reason: Option<String>,
}

struct SanctionPublication {
    target: String,
    participants: Vec<String>,
    lobby: Vec<String>,
    authenticated: bool,
    ip: Option<IpAddr>,
    kind: moderation::PunitiveKind,
    enabled: bool,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModerationRequest {
    pub action: String,
    pub target_participant_id: String,
    pub reason: Option<String>,
    pub duration_seconds: Option<u64>,
    pub report_id: Option<Uuid>,
}

impl ModerationRequest {
    fn validate(&self) -> Result<(), sqlx::Error> {
        if !matches!(self.action.as_str(), "kick" | "ban")
            || self.target_participant_id.is_empty()
            || self.target_participant_id.len() > 128
        {
            return Err(invalid(
                "Choose a participant and a supported moderation action",
            ));
        }
        if self
            .duration_seconds
            .is_some_and(|seconds| seconds == 0 || seconds > 10 * 365 * 24 * 3600)
            || (self.action == "kick" && self.duration_seconds.is_some())
        {
            return Err(invalid(
                "Ban duration must be between 1 second and 10 years",
            ));
        }
        if self
            .reason
            .as_ref()
            .is_some_and(|reason| reason.len() > 256 || reason.chars().any(char::is_control))
        {
            return Err(invalid(
                "Moderation reason must be at most 256 bytes without control characters",
            ));
        }
        Ok(())
    }
}

struct ModerationPublication {
    request: ModerationRequest,
    participants: Vec<String>,
    lobby: Vec<String>,
    authenticated: bool,
    ip: Option<IpAddr>,
    name: String,
}

enum Command {
    Settings(SettingsPatch),
    Role(Uuid, roles::Role),
    Unban(Uuid),
    Report(Uuid, String),
    Moderate(ModerationRequest),
    Lobby(String, LobbyRequest),
    Sanction(SanctionRequest),
}

enum Publication {
    Settings(settings::RoomSettings, Option<Option<String>>),
    Role(Uuid, roles::Role),
    Unban(Option<Uuid>, Option<IpAddr>),
    Moderate(ModerationPublication),
    Sanction(SanctionPublication),
    None,
}

struct Context {
    manager: RoomManager,
    room_id: String,
    user: Uuid,
    session: Uuid,
    expires_at: i64,
    session_expires_at: DateTime<Utc>,
    actor_name: String,
    role: roles::Role,
    settings: settings::RoomSettings,
    runtime: Option<Arc<RwLock<Room>>>,
    transaction: Option<Transaction<'static, Postgres>>,
    _identity: IdentityUpdateGuard,
    _control: Option<OwnedMutexGuard<()>>,
}

fn invalid(message: &str) -> sqlx::Error {
    sqlx::Error::InvalidArgument(message.to_owned())
}

fn response_error(status: StatusCode, message: &'static str) -> api::RoomApiError {
    (status, private_headers(), Json(json!({"error": message})))
        .into_response()
        .into()
}

fn database_error(error: sqlx::Error) -> api::RoomApiError {
    match error {
        sqlx::Error::InvalidArgument(message) => {
            let status = if matches!(
                message.as_str(),
                "Member not found in this room"
                    | "Ban not found in this room"
                    | "Open report not found in this room"
            ) {
                StatusCode::NOT_FOUND
            } else if message == "Sign-in expired; sign in again" {
                StatusCode::UNAUTHORIZED
            } else if message == "Insufficient room permissions" {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, private_headers(), Json(json!({"error": message})))
                .into_response()
                .into()
        }
        sqlx::Error::RowNotFound => {
            response_error(StatusCode::NOT_FOUND, "Room or record not found")
        }
        error => {
            crate::db::record_error(&error);
            // PostgreSQL details can include the failed room row, including
            // its password hash. The aggregate error category is sufficient.
            tracing::warn!("Saved room administration failed");
            response_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Room administration is unavailable; reload before retrying a change",
            )
        }
    }
}

fn private_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("private, no-store"),
    );
    headers
}

impl Context {
    async fn admit(
        manager: RoomManager,
        room_id: String,
        claims: Claims,
    ) -> Result<Self, api::RoomApiError> {
        if !settings::valid_room_id(&room_id) {
            return Err(response_error(StatusCode::BAD_REQUEST, "Invalid room"));
        }
        let creation = tokio::time::timeout(
            control::ADMISSION_TIMEOUT,
            manager.room_creation_lock.clone().lock_owned(),
        )
        .await
        .map_err(|_| {
            response_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Room creation is busy; try again",
            )
        })?;
        if manager.drain.is_draining()
            || manager
                .deleting_rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .contains_key(&room_id)
        {
            return Err(response_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Room is unavailable",
            ));
        }
        if manager
            .identity_updates
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .contains(&room_id)
        {
            return Err(response_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Room administration is busy; try again",
            ));
        }
        let identity = manager.begin_identity_update(&room_id);
        let runtime = manager
            .rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&room_id)
            .cloned();
        drop(creation);
        let control = if let Some(runtime) = runtime.as_ref() {
            let guard =
                tokio::time::timeout(control::ADMISSION_TIMEOUT, control::lock_room(runtime))
                    .await
                    .map_err(|_| {
                        response_error(
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Room control is busy; try again",
                        )
                    })?;
            let room = runtime.read().await;
            if room.deleting || !room.persisted {
                return Err(response_error(
                    StatusCode::NOT_FOUND,
                    "Saved room not found",
                ));
            }
            Some(guard)
        } else {
            None
        };
        let pool = manager
            .db_pool
            .as_ref()
            .ok_or_else(|| AuthError::NotConfigured.into_response())?;
        let database = async {
            let mut transaction = pool.begin().await.map_err(database_error)?;
            sqlx::query("SET LOCAL statement_timeout='5s'")
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            sqlx::query("SET LOCAL lock_timeout='5s'")
                .execute(&mut *transaction)
                .await
                .map_err(database_error)?;
            if Utc::now().timestamp() >= claims.exp as i64 {
                return Err(AuthError::TokenExpired.into_response().into());
            }
            let (user, _) = sessions::lock_current_session(&mut transaction, &claims)
                .await
                .map_err(IntoResponse::into_response)?;
            let owner: Uuid =
                sqlx::query_scalar("SELECT owner_id FROM rooms WHERE id=$1 FOR UPDATE")
                    .bind(&room_id)
                    .fetch_optional(&mut *transaction)
                    .await
                    .map_err(database_error)?
                    .ok_or_else(|| response_error(StatusCode::NOT_FOUND, "Saved room not found"))?;
            let membership: Option<i16> = sqlx::query_scalar(
                "SELECT role FROM room_roles WHERE room_id=$1 AND user_id=$2 FOR UPDATE",
            )
            .bind(&room_id)
            .bind(user)
            .fetch_optional(&mut *transaction)
            .await
            .map_err(database_error)?;
            let role = if owner == user {
                roles::Role::Owner
            } else {
                membership.map_or(roles::Role::User, roles::Role::from_db)
            };
            if role < roles::Role::Moderator {
                return Err(response_error(
                    StatusCode::FORBIDDEN,
                    "Insufficient room permissions",
                ));
            }
            let actor_name: String =
                sqlx::query_scalar("SELECT display_name FROM users WHERE id=$1")
                    .bind(user)
                    .fetch_one(&mut *transaction)
                    .await
                    .map_err(database_error)?;
            let settings = load_settings(&mut transaction, &room_id)
                .await
                .map_err(database_error)?;
            // FOR UPDATE predicates may have been evaluated before a row-lock
            // wait. Check the current clock again after every admission lock.
            let session_expires_at: Option<DateTime<Utc>> = sqlx::query_scalar("SELECT expires_at FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp()")
                .bind(claims.sid).bind(user).fetch_optional(&mut *transaction).await.map_err(database_error)?;
            let session_expires_at =
                session_expires_at.ok_or_else(|| AuthError::InvalidToken.into_response())?;
            if Utc::now().timestamp() >= claims.exp as i64 {
                return Err(AuthError::TokenExpired.into_response().into());
            }
            Ok((
                transaction,
                user,
                actor_name,
                role,
                settings,
                session_expires_at,
            ))
        };
        let (transaction, user, actor_name, role, settings, session_expires_at) =
            tokio::time::timeout(control::ADMISSION_TIMEOUT, database)
                .await
                .map_err(|_| {
                    response_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "Room authorization is busy; try again",
                    )
                })??;
        if manager.drain.is_draining() {
            return Err(response_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "Server is shutting down",
            ));
        }
        if let Some(runtime) = runtime.as_ref() {
            let state = runtime.read().await;
            if state.deleting
                || !manager
                    .rooms
                    .read()
                    .unwrap_or_else(|error| error.into_inner())
                    .get(&room_id)
                    .is_some_and(|current| Arc::ptr_eq(current, runtime))
            {
                return Err(response_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Room is no longer active",
                ));
            }
        }
        Ok(Self {
            manager,
            room_id,
            user,
            session: claims.sid,
            expires_at: claims.exp as i64,
            session_expires_at,
            actor_name,
            role,
            settings,
            runtime,
            transaction: Some(transaction),
            _identity: identity,
            _control: control,
        })
    }

    fn require(&self, minimum: roles::Role) -> Result<(), sqlx::Error> {
        if self.role < minimum {
            return Err(invalid("Insufficient room permissions"));
        }
        Ok(())
    }

    fn connection(&mut self) -> &mut PgConnection {
        self.transaction
            .as_mut()
            .expect("admitted transaction remains owned")
    }

    async fn recheck_expiry(&mut self) -> Result<(), sqlx::Error> {
        let session = self.session;
        let user = self.user;
        let expiry:Option<DateTime<Utc>>=sqlx::query_scalar("SELECT expires_at FROM sessions WHERE id=$1 AND user_id=$2 AND expires_at>clock_timestamp()")
            .bind(session).bind(user).fetch_optional(self.connection()).await?;
        self.session_expires_at =
            expiry.ok_or_else(|| invalid("Sign-in expired; sign in again"))?;
        if Utc::now().timestamp() >= self.expires_at {
            return Err(invalid("Sign-in expired; sign in again"));
        }
        Ok(())
    }

    async fn execute(mut self, command: Command) -> Result<Value, sqlx::Error> {
        let command = match command {
            Command::Lobby(target, request) => return self.lobby_action(target, request).await,
            command => command,
        };
        if let Some(runtime) = self.runtime.as_ref() {
            let mut room = runtime.write().await;
            if !room.reserve_admin_mutation(std::time::Instant::now()) {
                return Err(invalid("Room administration changes are rate limited"));
            }
        }
        let manager = self.manager.clone();
        let room_id = self.room_id.clone();
        let runtime = self.runtime.clone();
        let persistence = async {
            let (publication, response) = self.prepare(command).await?;
            self.recheck_expiry().await?;
            self.transaction
                .take()
                .expect("owned transaction")
                .commit()
                .await?;
            Ok((publication, response))
        };
        let (publication, response) = if let Some(runtime) = runtime.as_ref() {
            manager.persist_room(&room_id, runtime, persistence).await?
        } else {
            let result = tokio::time::timeout(control::PERSISTENCE_TIMEOUT, persistence)
                .await
                .unwrap_or_else(|_| {
                    Err(sqlx::Error::Protocol(
                        "Room administration commit status is unknown".into(),
                    ))
                });
            if result
                .as_ref()
                .is_err_and(control::persistence_is_indeterminate)
            {
                manager
                    .deleting_rooms
                    .write()
                    .unwrap_or_else(|error| error.into_inner())
                    .entry(room_id.clone())
                    .or_insert_with(Uuid::new_v4);
            }
            result?
        };
        self.publish(publication).await;
        Ok(response)
    }

    async fn prepare(&mut self, command: Command) -> Result<(Publication, Value), sqlx::Error> {
        match command {
            Command::Settings(patch) => self.settings_change(patch).await,
            Command::Role(target, role) => self.role_change(target, role).await,
            Command::Unban(id) => self.unban(id).await,
            Command::Report(id, status) => self.resolve_report(id, &status).await,
            Command::Moderate(request) => self.moderate(request).await,
            Command::Sanction(request) => self.sanction(request).await,
            Command::Lobby(..) => {
                unreachable!("lobby uses its read-only authorization transaction")
            }
        }
    }

    async fn lobby_action(
        mut self,
        target: String,
        request: LobbyRequest,
    ) -> Result<Value, sqlx::Error> {
        if !matches!(request.action.as_str(), "admit" | "deny")
            || target.is_empty()
            || target.len() > 128
            || request
                .reason
                .as_ref()
                .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(invalid("Invalid waiting-room action"));
        }
        self.recheck_expiry().await?;
        let control = self
            ._control
            .take()
            .ok_or_else(|| invalid("The waiting room is no longer active"))?;
        let expires = self.expires_at.min(self.session_expires_at.timestamp());
        let authorize = move || {
            anyhow::ensure!(
                Utc::now().timestamp() < expires,
                "Sign-in expired; sign in again"
            );
            Ok(())
        };
        // Retain users/session/room row locks across the helper's preference
        // lookup. Its final synchronous callback runs after the final await.
        let operation = async {
            if request.action == "admit" {
                self.manager
                    .admit_authorized_lobby(&self.room_id, &target, control, authorize)
                    .await
            } else {
                self.manager
                    .deny_authorized_lobby(
                        &self.room_id,
                        &target,
                        request.reason,
                        control,
                        authorize,
                    )
                    .await
            }
        };
        tokio::time::timeout(control::PERSISTENCE_TIMEOUT, operation)
            .await
            .map_err(|_| invalid("Waiting-room change timed out; refresh this room"))?
            .map_err(|_| {
                if Utc::now().timestamp() >= expires {
                    invalid("Sign-in expired; sign in again")
                } else {
                    invalid(
                        "Waiting-room change failed; refresh and check room capacity and access",
                    )
                }
            })?;
        self.transaction
            .take()
            .expect("read-only lobby authorization")
            .commit()
            .await?;
        Ok(json!({"updated":true}))
    }

    async fn sanction(
        &mut self,
        request: SanctionRequest,
    ) -> Result<(Publication, Value), sqlx::Error> {
        let kind = match request.kind.as_str() {
            "camera" => moderation::PunitiveKind::CamBanned,
            "chat" => moderation::PunitiveKind::Muted,
            _ => return Err(invalid("Choose a camera or chat restriction")),
        };
        if request
            .reason
            .as_ref()
            .is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control))
        {
            return Err(invalid(
                "Moderation reason must be at most 256 bytes without control characters",
            ));
        }
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| invalid("The participant is no longer in this room"))?;
        let room = runtime.read().await;
        let person = room
            .participants
            .get(&request.target_participant_id)
            .ok_or_else(|| invalid("The participant is no longer in this room"))?;
        let participants =
            super::moderation_cohort(&room, &request.target_participant_id, self.role)
                .map_err(|_| invalid("Insufficient room permissions"))?;
        let lobby = super::guest_ip_lobby_cohort(&room, person.authenticated, person.ip, self.role)
            .map_err(|_| invalid("Insufficient room permissions"))?;
        let key = super::SanctionKey::for_identity(
            &request.target_participant_id,
            person.authenticated,
            person.ip,
        );
        if super::punitive_cohort_matches(&room, &participants, &lobby, &key, kind, request.enabled)
        {
            return Ok((Publication::None, json!({"updated":true})));
        }
        let actor = self.user.to_string();
        let event = moderation::ModerationEvent::new(
            moderation::ModerationAction::for_punitive(kind, request.enabled),
            moderation::Actor {
                id: &actor,
                name: &self.actor_name,
            },
            moderation::Target {
                id: &request.target_participant_id,
                name: &person.name,
                authenticated: person.authenticated,
                ip: person.ip,
            },
            request.reason.as_deref(),
            None,
            None,
        );
        let publication = SanctionPublication {
            target: request.target_participant_id.clone(),
            participants,
            lobby,
            authenticated: person.authenticated,
            ip: person.ip,
            kind,
            enabled: request.enabled,
        };
        drop(room);
        let room_id = self.room_id.clone();
        let user = publication
            .authenticated
            .then(|| request.target_participant_id.parse::<Uuid>().ok())
            .flatten();
        let ip = if publication.authenticated {
            publication.ip
        } else {
            publication.ip.map(moderation::canonical_guest_ip)
        }
        .map(|ip| ip.to_string());
        let state = if kind == moderation::PunitiveKind::CamBanned {
            "cambanned"
        } else {
            "muted"
        };
        let actor = self.user;
        if let Some(user) = user {
            if request.enabled {
                sqlx::query("INSERT INTO room_states(room_id,user_id,state,ip_address,reason,applied_by) VALUES($1,$2,$3,$4::inet,$5,$6) ON CONFLICT(room_id,user_id,state) DO UPDATE SET ip_address=EXCLUDED.ip_address,reason=EXCLUDED.reason,expires_at=NULL,applied_by=EXCLUDED.applied_by")
                    .bind(&room_id).bind(user).bind(state).bind(&ip).bind(&request.reason).bind(actor).execute(self.connection()).await?;
            } else {
                sqlx::query("DELETE FROM room_states WHERE room_id=$1 AND user_id=$2 AND state=$3")
                    .bind(&room_id)
                    .bind(user)
                    .bind(state)
                    .execute(self.connection())
                    .await?;
            }
        } else if let Some(ip) = ip {
            if request.enabled {
                sqlx::query("INSERT INTO room_states(room_id,user_id,state,ip_address,reason,applied_by) VALUES($1,NULL,$2,$3::inet,$4,$5) ON CONFLICT(room_id,ip_address,state) WHERE user_id IS NULL AND ip_address IS NOT NULL DO UPDATE SET reason=EXCLUDED.reason,expires_at=NULL,applied_by=EXCLUDED.applied_by")
                    .bind(&room_id).bind(state).bind(ip).bind(&request.reason).bind(actor).execute(self.connection()).await?;
            } else {
                sqlx::query("DELETE FROM room_states WHERE room_id=$1 AND user_id IS NULL AND ip_address=$2::inet AND state=$3").bind(&room_id).bind(ip).bind(state).execute(self.connection()).await?;
            }
        } else {
            return Err(invalid(
                "The participant cannot be identified for a saved restriction",
            ));
        }
        moderation::record_event(
            self.connection(),
            &room_id,
            &event,
            moderation::MAX_MODERATION_EVENTS,
        )
        .await?;
        Ok((Publication::Sanction(publication), json!({"updated":true})))
    }

    async fn settings_change(
        &mut self,
        patch: SettingsPatch,
    ) -> Result<(Publication, Value), sqlx::Error> {
        self.require(roles::Role::Admin)?;
        let updated = patch.apply(&self.settings)?;
        let hash = match patch.password.as_ref() {
            Some(Some(password)) => {
                if let Some(runtime) = self.runtime.as_ref() {
                    let mut room = runtime.write().await;
                    let now = std::time::Instant::now();
                    if room.last_password_hash_at.is_some_and(|previous| {
                        now.duration_since(previous) < super::ROOM_PASSWORD_HASH_COOLDOWN
                    }) {
                        return Err(invalid("Room password updates are rate limited"));
                    }
                    room.last_password_hash_at = Some(now);
                }
                let permit = self
                    .manager
                    .password_hash_work
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| invalid("Password service is busy; try again"))?;
                let password = password.clone();
                let hash = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    crate::auth::password::hash_password(&password)
                })
                .await
                .map_err(|_| invalid("Password update failed"))?
                .map_err(|_| invalid("Password update failed"))?;
                Some(Some(hash))
            }
            Some(None) => Some(None),
            None => None,
        };
        let room_id = self.room_id.clone();
        sqlx::query("UPDATE rooms SET moderated=$2,lobby_enabled=$3,guests_allowed=$4,guests_can_broadcast=$5,max_broadcasters=$6,max_participants=$7,allow_screen_sharing=$8,allow_chat=$9,allow_video=$10,require_registration=$11,invite_only=$12,push_to_talk=$13,secret=$14,password_hash=CASE WHEN $15 THEN $16 ELSE password_hash END,updated_at=now() WHERE id=$1")
            .bind(&room_id).bind(updated.moderated).bind(updated.lobby_enabled).bind(updated.guests_allowed).bind(updated.guests_can_broadcast)
            .bind(updated.max_broadcasters).bind(updated.max_participants).bind(updated.allow_screen_sharing).bind(updated.allow_chat).bind(updated.allow_video)
            .bind(updated.require_registration).bind(updated.invite_only).bind(updated.push_to_talk).bind(updated.secret).bind(hash.is_some()).bind(hash.clone().flatten())
            .execute(self.connection()).await?;
        let response = json!({"role":self.role.name(),"settings":updated});
        Ok((Publication::Settings(updated, hash), response))
    }

    async fn role_change(
        &mut self,
        target: Uuid,
        new_role: roles::Role,
    ) -> Result<(Publication, Value), sqlx::Error> {
        let room_id = self.room_id.clone();
        let current: Option<i16> = sqlx::query_scalar("SELECT CASE WHEN $2=owner_id THEN 5 ELSE (SELECT role+1 FROM room_roles WHERE room_id=$1 AND user_id=$2) END::smallint FROM rooms WHERE id=$1 AND ($2=owner_id OR EXISTS(SELECT 1 FROM room_roles WHERE room_id=$1 AND user_id=$2))")
            .bind(&room_id).bind(target).fetch_optional(self.connection()).await?;
        let online = if let Some(runtime) = self.runtime.as_ref() {
            let room = runtime.read().await;
            room.participants
                .get(&target.to_string())
                .filter(|person| person.authenticated)
                .map(|person| person.role)
        } else {
            None
        };
        let current = current
            .and_then(|value| roles::Role::from_u8(value as u8))
            .or(online)
            .ok_or_else(|| invalid("Member not found in this room"))?;
        if new_role < roles::Role::User || !self.role.can_set_role(current, new_role) {
            return Err(invalid("Insufficient room permissions"));
        }
        let setter = self.user;
        if new_role.to_db() == 0 {
            sqlx::query("DELETE FROM room_roles WHERE room_id=$1 AND user_id=$2")
                .bind(&room_id)
                .bind(target)
                .execute(self.connection())
                .await?;
        } else {
            sqlx::query("INSERT INTO room_roles(room_id,user_id,role,granted_by) VALUES($1,$2,$3,$4) ON CONFLICT(room_id,user_id) DO UPDATE SET role=$3,granted_by=$4")
                .bind(&room_id).bind(target).bind(new_role.to_db()).bind(setter).execute(self.connection()).await?;
        }
        Ok((Publication::Role(target, new_role), json!({"updated":true})))
    }

    async fn unban(&mut self, id: Uuid) -> Result<(Publication, Value), sqlx::Error> {
        self.require(roles::Role::Admin)?;
        let room_id = self.room_id.clone();
        let row: Option<(Option<Uuid>,Option<String>,Option<String>)> = sqlx::query_as("DELETE FROM room_states s WHERE s.room_id=$1 AND s.id=$2 AND s.state='banned' RETURNING s.user_id,host(s.ip_address),(SELECT display_name FROM users WHERE id=s.user_id)")
            .bind(&room_id).bind(id).fetch_optional(self.connection()).await?;
        let (user, ip, name) = row.ok_or_else(|| invalid("Ban not found in this room"))?;
        let target = user.unwrap_or(id).to_string();
        let actor = self.user.to_string();
        let address = ip.and_then(|ip| ip.parse::<IpAddr>().ok());
        let event = moderation::ModerationEvent::new(
            moderation::ModerationAction::Unban,
            moderation::Actor {
                id: &actor,
                name: &self.actor_name,
            },
            moderation::Target {
                id: &target,
                name: name.as_deref().unwrap_or("Guest"),
                authenticated: user.is_some(),
                ip: address,
            },
            None,
            None,
            None,
        );
        moderation::record_event(
            self.connection(),
            &room_id,
            &event,
            moderation::MAX_MODERATION_EVENTS,
        )
        .await?;
        Ok((
            Publication::Unban(user, if user.is_none() { address } else { None }),
            json!({"removed":true}),
        ))
    }

    async fn resolve_report(
        &mut self,
        id: Uuid,
        status: &str,
    ) -> Result<(Publication, Value), sqlx::Error> {
        if !matches!(status, "resolved" | "dismissed") {
            return Err(invalid("Choose resolved or dismissed"));
        }
        let room_id = self.room_id.clone();
        let actor = self.user.to_string();
        let row: Option<(String,String,bool)> = sqlx::query_as("UPDATE room_reports r SET status=$3,resolved_at=now(),resolved_by=$4 WHERE r.room_id=$1 AND r.id=$2 AND r.status='open' RETURNING r.target_participant_id,r.target_name,EXISTS(SELECT 1 FROM users u WHERE u.id::text=r.target_participant_id)")
            .bind(&room_id).bind(id).bind(status).bind(&actor).fetch_optional(self.connection()).await?;
        let (target, name, authenticated) =
            row.ok_or_else(|| invalid("Open report not found in this room"))?;
        let action = if status == "resolved" {
            moderation::ModerationAction::ReportResolved
        } else {
            moderation::ModerationAction::ReportDismissed
        };
        let event = moderation::ModerationEvent::new(
            action,
            moderation::Actor {
                id: &actor,
                name: &self.actor_name,
            },
            moderation::Target {
                id: &target,
                name: &name,
                authenticated,
                ip: None,
            },
            None,
            None,
            Some(id),
        );
        moderation::record_event(
            self.connection(),
            &room_id,
            &event,
            moderation::MAX_MODERATION_EVENTS,
        )
        .await?;
        Ok((Publication::None, json!({"reportId":id,"status":status})))
    }

    async fn moderate(
        &mut self,
        request: ModerationRequest,
    ) -> Result<(Publication, Value), sqlx::Error> {
        request.validate()?;
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| invalid("The participant is no longer in this room"))?;
        let room = runtime.read().await;
        let target = room
            .participants
            .get(&request.target_participant_id)
            .ok_or_else(|| invalid("The participant is no longer in this room"))?;
        if !self.role.can_moderate(target.role) {
            return Err(invalid("Insufficient room permissions"));
        }
        let ban = request.action == "ban";
        let participants = if ban {
            super::moderation_cohort(&room, &request.target_participant_id, self.role)
                .map_err(|_| invalid("Insufficient room permissions"))?
        } else {
            vec![request.target_participant_id.clone()]
        };
        let lobby = if ban {
            super::guest_ip_lobby_cohort(&room, target.authenticated, target.ip, self.role)
                .map_err(|_| invalid("Insufficient room permissions"))?
        } else {
            Vec::new()
        };
        let publication = ModerationPublication {
            request: request.clone(),
            participants,
            lobby,
            authenticated: target.authenticated,
            ip: target.ip,
            name: target.name.clone(),
        };
        drop(room);
        if ban {
            runtime
                .write()
                .await
                .social
                .reserve_ban()
                .map_err(|_| invalid("Room ban capacity reached"))?;
        }
        let room_id = self.room_id.clone();
        if let Some(report) = request.report_id {
            let target:Option<String> = sqlx::query_scalar("SELECT target_participant_id FROM room_reports WHERE room_id=$1 AND id=$2 AND status='open' FOR UPDATE")
                .bind(&room_id).bind(report).fetch_optional(self.connection()).await?;
            if target.as_deref() != Some(&request.target_participant_id) {
                return Err(invalid("Open report does not match this participant"));
            }
        }
        let actor = self.user.to_string();
        let expiry = request.duration_seconds.and_then(|duration| {
            Utc::now().checked_add_signed(chrono::Duration::seconds(duration as i64))
        });
        let event = moderation::ModerationEvent::new(
            if ban {
                moderation::ModerationAction::Ban
            } else {
                moderation::ModerationAction::Kick
            },
            moderation::Actor {
                id: &actor,
                name: &self.actor_name,
            },
            moderation::Target {
                id: &request.target_participant_id,
                name: &publication.name,
                authenticated: publication.authenticated,
                ip: publication.ip,
            },
            request.reason.as_deref(),
            expiry,
            request.report_id,
        );
        if ban {
            let actor = self.user;
            let user = publication
                .authenticated
                .then(|| request.target_participant_id.parse::<Uuid>().ok())
                .flatten();
            let ip = if publication.authenticated {
                publication.ip
            } else {
                publication.ip.map(moderation::canonical_guest_ip)
            }
            .map(|ip| ip.to_string());
            if let Some(user) = user {
                sqlx::query("INSERT INTO room_states(room_id,user_id,state,ip_address,reason,expires_at,applied_by) VALUES($1,$2,'banned',$3::inet,$4,$5,$6) ON CONFLICT(room_id,user_id,state) DO UPDATE SET ip_address=EXCLUDED.ip_address,reason=EXCLUDED.reason,expires_at=EXCLUDED.expires_at,applied_by=EXCLUDED.applied_by")
                    .bind(&room_id).bind(user).bind(&ip).bind(&request.reason).bind(expiry).bind(actor).execute(self.connection()).await?;
            } else if let Some(ip) = ip {
                sqlx::query("INSERT INTO room_states(room_id,user_id,state,ip_address,reason,expires_at,applied_by) VALUES($1,NULL,'banned',$2::inet,$3,$4,$5) ON CONFLICT(room_id,ip_address,state) WHERE user_id IS NULL AND ip_address IS NOT NULL DO UPDATE SET reason=EXCLUDED.reason,expires_at=EXCLUDED.expires_at,applied_by=EXCLUDED.applied_by")
                    .bind(&room_id).bind(ip).bind(&request.reason).bind(expiry).bind(actor).execute(self.connection()).await?;
            } else {
                return Err(invalid(
                    "The participant cannot be identified for a saved ban",
                ));
            }
        }
        moderation::record_event(
            self.connection(),
            &room_id,
            &event,
            moderation::MAX_MODERATION_EVENTS,
        )
        .await?;
        Ok((Publication::Moderate(publication), json!({"updated":true})))
    }

    async fn publish(mut self, publication: Publication) {
        let Some(runtime) = self.runtime.as_ref() else {
            return;
        };
        let mut room = runtime.write().await;
        // Control and the creation marker still pin this exact generation.
        let mut affected = None;
        let mut revoked_override = None;
        match publication {
            Publication::Settings(settings, password) => {
                let value =
                    serde_json::to_value(&settings).expect("validated room settings serialize");
                room.settings = Some(settings);
                if let Some(password) = password {
                    room.password_hash = password;
                }
                room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings: value });
            }
            Publication::Role(target, role) => {
                let target = target.to_string();
                if let Some(person) = room.participants.get_mut(&target) {
                    person.role = role;
                }
                if let Some(person) = room.lobby.get_mut(&target) {
                    person.role = role;
                }
                room.broadcast_all(&ServerMessage::RoleChanged {
                    participant_id: target.clone(),
                    new_role: role.name().to_owned(),
                    granted_by: self.user.to_string(),
                });
                affected = Some(target);
            }
            Publication::Unban(user, ip) => {
                let user = user.map(|user| user.to_string());
                let ids = room.social.forget_ban(user.as_deref(), ip);
                if let Some(user) = user {
                    room.banned_participants.remove(&user);
                }
                if let Some(ip) = ip {
                    room.banned_guest_ips.remove(&ip);
                }
                for id in ids {
                    room.banned_participants.remove(&id);
                    room.banned_guest_participants.remove(&id);
                }
                room.policy_revision = room.policy_revision.wrapping_add(1);
                return;
            }
            Publication::Moderate(publication) => {
                let ban = publication.request.action == "ban";
                if ban {
                    let ids: Vec<_> = publication
                        .participants
                        .iter()
                        .chain(publication.lobby.iter())
                        .cloned()
                        .collect();
                    let expiry = publication.request.duration_seconds.and_then(|duration| {
                        std::time::Instant::now()
                            .checked_add(std::time::Duration::from_secs(duration))
                    });
                    super::record_banned_cohort(
                        &mut room,
                        &ids,
                        publication.authenticated,
                        publication.ip,
                        expiry,
                    );
                    room.social.record_ban(
                        publication.name,
                        publication.authenticated,
                        ids,
                        publication.ip,
                        publication.request.reason.as_deref(),
                        publication.request.duration_seconds,
                    );
                }
                let mut sessions = Vec::new();
                for id in publication.participants {
                    let notice = if ban {
                        ServerMessage::ParticipantBanned {
                            participant_id: id.clone(),
                            reason: publication.request.reason.clone(),
                        }
                    } else {
                        ServerMessage::ParticipantKicked {
                            participant_id: id.clone(),
                            reason: publication.request.reason.clone(),
                        }
                    };
                    room.broadcast_all(&notice);
                    if let Some(person) = room.participants.remove(&id) {
                        for producer in person.producers.keys() {
                            room.producer_to_participant.remove(producer);
                        }
                        sessions.push((id.clone(), person.media_session_id));
                        room.broadcast_participant_left(&id);
                    }
                }
                if let Ok(notice) = serde_json::to_string(&ServerMessage::LobbyDenied {
                    reason: Some(
                        publication
                            .request
                            .reason
                            .unwrap_or_else(|| "Banned from room".into()),
                    ),
                }) {
                    let notice = crate::OutboundJson::from(notice);
                    for id in publication.lobby {
                        if let Some(person) = room.lobby.remove(&id) {
                            let _ = super::try_send_essential(
                                &room.metrics,
                                &person.sender,
                                notice.clone(),
                            );
                        }
                    }
                }
                room.policy_revision = room.policy_revision.wrapping_add(1);
                room.notify_lobby_status();
                drop(room);
                self._control.take();
                for (id, session) in sessions {
                    let media = RoomManager::media_participant_id(&self.room_id, &id, session);
                    if let Err(error) = self
                        .manager
                        .media_server
                        .transport_manager()
                        .remove_participant(&media)
                        .await
                    {
                        tracing::warn!(%error,"Administered participant media cleanup failed");
                    }
                }
                return;
            }
            Publication::Sanction(publication) => {
                let key = super::SanctionKey::for_identity(
                    &publication.target,
                    publication.authenticated,
                    publication.ip,
                );
                super::apply_punitive_to_cohort(
                    &mut room,
                    &publication.participants,
                    &publication.lobby,
                    key,
                    publication.kind,
                    publication.enabled,
                );
                for id in &publication.participants {
                    room.broadcast_all(&super::punitive_notification(
                        publication.kind,
                        publication.enabled,
                        id.clone(),
                    ));
                }
                for id in &publication.lobby {
                    if let (Some(person), Ok(notice)) = (
                        room.lobby.get(id),
                        serde_json::to_string(&super::punitive_notification(
                            publication.kind,
                            publication.enabled,
                            id.clone(),
                        )),
                    ) {
                        let _ = super::try_send_essential(
                            &room.metrics,
                            &person.sender,
                            crate::OutboundJson::from(notice),
                        );
                    }
                }
                let mut revoked = Vec::new();
                if publication.kind == moderation::PunitiveKind::CamBanned && publication.enabled {
                    for id in &publication.participants {
                        revoked.extend(RoomManager::revoke_unauthorized_producers(
                            &mut room,
                            Some(id),
                            "Camera access revoked by moderator",
                        ));
                    }
                }
                revoked_override = Some(revoked);
            }
            Publication::None => return,
        }
        room.policy_revision = room.policy_revision.wrapping_add(1);
        room.notify_lobby_status();
        let revoked = revoked_override.unwrap_or_else(|| {
            RoomManager::revoke_unauthorized_producers(
                &mut room,
                affected.as_deref(),
                "Room policy no longer permits this producer",
            )
        });
        let active = room.active_speaker_observer.clone();
        let audio = room.audio_level_observer.clone();
        drop(room);
        // Publication finishes before control admission opens. Native cleanup
        // targets immutable media session ids, so a replacement cannot be closed.
        self._control.take();
        self.manager
            .close_revoked_producers(&self.room_id, revoked, active, audio)
            .await;
    }
}

async fn load_settings(
    connection: &mut PgConnection,
    room_id: &str,
) -> Result<settings::RoomSettings, sqlx::Error> {
    let value: Value = sqlx::query_scalar("SELECT jsonb_build_object('id',id,'ownerId',owner_id,'displayName',display_name,'passwordProtected',password_hash IS NOT NULL,'requireRegistration',require_registration,'maxParticipants',max_participants,'maxBroadcasters',max_broadcasters,'allowScreenSharing',allow_screen_sharing,'allowChat',allow_chat,'allowVideo',allow_video,'moderated',moderated,'inviteOnly',invite_only,'secret',secret,'lobbyEnabled',lobby_enabled,'pushToTalk',push_to_talk,'guestsAllowed',guests_allowed,'guestsCanBroadcast',guests_can_broadcast,'topic',topic,'nameStyle',name_style,'topicStyle',topic_style,'historyRetentionDays',history_retention_days) FROM rooms WHERE id=$1")
        .bind(room_id).fetch_one(connection).await?;
    serde_json::from_value(value).map_err(|error| sqlx::Error::Protocol(error.to_string()))
}

async fn admit(
    server: &SignalingServer,
    headers: &HeaderMap,
    id: String,
) -> Result<Context, api::RoomApiError> {
    let claims = account::authenticated_claims(server, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    Context::admit(server.room_manager().clone(), id, claims).await
}

type Response = (HeaderMap, Json<Value>);

/// GET /api/rooms/:id/administration.
pub async fn overview(
    State(server): State<SignalingServer>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, api::RoomApiError> {
    let _permit = api::acquire_room_api_request(&server)?;
    let mut context = admit(&server, &headers, id).await?;
    let response = json!({"role":context.role.name(),"settings":context.settings});
    context
        .transaction
        .take()
        .expect("read transaction")
        .commit()
        .await
        .map_err(database_error)?;
    Ok((private_headers(), Json(response)))
}

async fn mutate(
    server: SignalingServer,
    headers: HeaderMap,
    id: String,
    command: Command,
) -> Result<Response, api::RoomApiError> {
    let permit = api::acquire_room_api_request(&server)?;
    let context = admit(&server, &headers, id).await?;
    let result = tokio::spawn(async move {
        let _permit = permit;
        context.execute(command).await
    })
    .await
    .map_err(|_| {
        response_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Room administration operation failed",
        )
    })?
    .map_err(database_error)?;
    Ok((private_headers(), Json(result)))
}

pub async fn patch_settings(
    State(server): State<SignalingServer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(patch): Json<SettingsPatch>,
) -> Result<Response, api::RoomApiError> {
    mutate(server, headers, id, Command::Settings(patch)).await
}

pub async fn set_role(
    State(server): State<SignalingServer>,
    Path((id, target)): Path<(String, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<RoleRequest>,
) -> Result<Response, api::RoomApiError> {
    let role = roles::Role::from_u8(request.role)
        .ok_or_else(|| response_error(StatusCode::BAD_REQUEST, "Invalid role"))?;
    mutate(server, headers, id, Command::Role(target, role)).await
}

pub async fn remove_ban(
    State(server): State<SignalingServer>,
    Path((id, ban)): Path<(String, Uuid)>,
    headers: HeaderMap,
) -> Result<Response, api::RoomApiError> {
    mutate(server, headers, id, Command::Unban(ban)).await
}

pub async fn resolve_report(
    State(server): State<SignalingServer>,
    Path((id, report)): Path<(String, Uuid)>,
    headers: HeaderMap,
    Json(request): Json<ReportRequest>,
) -> Result<Response, api::RoomApiError> {
    mutate(server, headers, id, Command::Report(report, request.status)).await
}

pub async fn moderate(
    State(server): State<SignalingServer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<ModerationRequest>,
) -> Result<Response, api::RoomApiError> {
    request.validate().map_err(database_error)?;
    mutate(server, headers, id, Command::Moderate(request)).await
}

pub async fn lobby_action(
    State(server): State<SignalingServer>,
    Path((id, target)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<LobbyRequest>,
) -> Result<Response, api::RoomApiError> {
    mutate(server, headers, id, Command::Lobby(target, request)).await
}

pub async fn sanction(
    State(server): State<SignalingServer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(request): Json<SanctionRequest>,
) -> Result<Response, api::RoomApiError> {
    mutate(server, headers, id, Command::Sanction(request)).await
}

pub async fn lobby(
    State(server): State<SignalingServer>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Response, api::RoomApiError> {
    list(
        State(server),
        Path((id, "lobby".to_owned())),
        headers,
        Query(query),
    )
    .await
}

/// GET /api/rooms/:id/administration/:section?offset=N.
pub async fn list(
    State(server): State<SignalingServer>,
    Path((id, section)): Path<(String, String)>,
    headers: HeaderMap,
    Query(query): Query<PageQuery>,
) -> Result<Response, api::RoomApiError> {
    let _permit = api::acquire_room_api_request(&server)?;
    let offset = query.offset().map_err(database_error)?;
    if !matches!(
        section.as_str(),
        "members" | "bans" | "reports" | "history" | "lobby"
    ) {
        return Err(response_error(
            StatusCode::NOT_FOUND,
            "Administration page not found",
        ));
    }
    let mut context = admit(&server, &headers, id).await?;
    let response =
        tokio::time::timeout(control::PERSISTENCE_TIMEOUT, context.list(&section, offset))
            .await
            .map_err(|_| {
                response_error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "Administration page timed out",
                )
            })?
            .map_err(database_error)?;
    context
        .transaction
        .take()
        .expect("read transaction")
        .commit()
        .await
        .map_err(database_error)?;
    Ok((private_headers(), Json(response)))
}

impl Context {
    async fn list(&mut self, section: &str, offset: i64) -> Result<Value, sqlx::Error> {
        let room_id = self.room_id.clone();
        let mut rows: Vec<Value> = match section {
            "lobby"=> {
                let mut rows=if let Some(runtime)=self.runtime.as_ref() {
                    runtime.read().await.lobby.values().map(|entry|json!({"participantId":entry.participant_id,"displayName":entry.name,"authenticated":entry.authenticated,"role":entry.role.name()})).collect::<Vec<_>>()
                } else {Vec::new()};
                rows.sort_by(|left,right|left["displayName"].as_str().cmp(&right["displayName"].as_str()).then(left["participantId"].as_str().cmp(&right["participantId"].as_str())));
                rows.into_iter().skip(offset as usize).take(PAGE_SIZE+1).collect()
            }
            "members" => {
                let online = if let Some(runtime)=self.runtime.as_ref() { runtime.read().await.participants.values().map(|person|json!({"id":person.id,"name":person.name,"role":person.role.name(),"authenticated":person.authenticated})).collect::<Vec<_>>() } else {Vec::new()};
                // Merge saved membership and current participants before paging.
                sqlx::query_scalar("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,(rr.role+1)::smallint FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id),live AS(SELECT * FROM jsonb_to_recordset($3) AS p(id text,name text,role text,authenticated boolean)),combined AS(SELECT u.id::text AS id,u.display_name AS name,CASE roster.role WHEN 5 THEN 'owner' WHEN 4 THEN 'admin' WHEN 3 THEN 'moderator' ELSE 'member' END AS role,true AS authenticated,EXISTS(SELECT FROM live WHERE live.id=u.id::text) AS online FROM roster JOIN users u ON u.id=roster.user_id UNION ALL SELECT id,name,role,authenticated,true FROM live WHERE NOT EXISTS(SELECT FROM roster WHERE roster.user_id::text=live.id)) SELECT jsonb_build_object('userId',id,'displayName',name,'role',role,'online',online,'authenticated',authenticated) FROM combined ORDER BY name,id LIMIT 101 OFFSET $2")
                    .bind(&room_id).bind(offset).bind(sqlx::types::Json(online)).fetch_all(self.connection()).await?
            }
            "bans" => {
                self.require(roles::Role::Admin)?;
                sqlx::query_scalar("SELECT jsonb_strip_nulls(jsonb_build_object('banId',s.id,'displayName',COALESCE(u.display_name,'Guest'),'authenticated',s.user_id IS NOT NULL,'reason',s.reason,'expiresAt',s.expires_at)) FROM room_states s LEFT JOIN users u ON u.id=s.user_id WHERE s.room_id=$1 AND s.state='banned' AND(s.expires_at IS NULL OR s.expires_at>now()) ORDER BY s.created_at DESC,s.id LIMIT 101 OFFSET $2")
                    .bind(&room_id).bind(offset).fetch_all(self.connection()).await?
            }
            "reports" => sqlx::query_scalar("SELECT jsonb_strip_nulls(jsonb_build_object('reportId',r.id,'reporterId',r.reporter_id,'reporterName',r.reporter_name,'targetParticipantId',r.target_participant_id,'targetName',r.target_name,'reason',r.reason,'status',r.status,'createdAt',r.created_at,'resolvedAt',r.resolved_at,'outcome',CASE WHEN o.action IS NULL THEN NULL ELSE jsonb_build_object('action',o.action,'createdAt',o.created_at) END)) FROM room_reports r LEFT JOIN LATERAL(SELECT action,created_at FROM moderation_events e WHERE e.report_id=r.id ORDER BY created_at DESC,id LIMIT 1)o ON true WHERE r.room_id=$1 ORDER BY r.created_at DESC,r.id LIMIT 101 OFFSET $2")
                .bind(&room_id).bind(offset).fetch_all(self.connection()).await?,
            "history" => {
                let owner = self.role==roles::Role::Owner;
                // list_events uses a pool; the same query here stays inside the
                // current users -> sessions -> room authorization transaction.
                sqlx::query_scalar("SELECT jsonb_strip_nulls(jsonb_build_object('eventId',id,'action',action,'actorId',actor_id,'actorName',actor_name,'targetId',target_id,'targetName',target_name,'targetAuthenticated',target_authenticated,'reason',reason,'expiresAt',expires_at,'reportId',report_id,'createdAt',created_at,'targetIp',CASE WHEN $3 THEN host(target_ip) ELSE NULL END)) FROM moderation_events WHERE room_id=$1 ORDER BY created_at DESC,id LIMIT 101 OFFSET $2")
                    .bind(&room_id).bind(offset).bind(owner).fetch_all(self.connection()).await?
            }
            _=>return Err(invalid("Invalid administration page")),
        };
        let has_more = rows.len() > PAGE_SIZE;
        rows.truncate(PAGE_SIZE);
        let key = if section == "history" {
            "events"
        } else {
            section
        };
        Ok(json!({(key):rows,"hasMore":has_more}))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patch_preserves_nullable_contract_and_refuses_unknown_fields() {
        let unchanged: SettingsPatch = serde_json::from_value(json!({})).unwrap();
        assert!(unchanged.password.is_none());
        let cleared: SettingsPatch =
            serde_json::from_value(json!({"password":null,"maxParticipants":null})).unwrap();
        assert_eq!(cleared.password, Some(None));
        assert_eq!(cleared.max_participants, Some(None));
        assert!(
            serde_json::from_value::<SettingsPatch>(json!({"ownerId":Uuid::new_v4()})).is_err()
        );
        assert!(serde_json::from_value::<SettingsPatch>(json!({"allowChat":"yes"})).is_err());
    }

    #[test]
    fn patch_checks_effective_combined_limits_without_changing_other_settings() {
        let current = RoomManager::default_room_settings("saved");
        let patch: SettingsPatch =
            serde_json::from_value(json!({"maxParticipants":2,"maxBroadcasters":3})).unwrap();
        assert!(patch.apply(&current).is_err());
        let patch: SettingsPatch = serde_json::from_value(json!({"maxParticipants":0})).unwrap();
        assert!(patch.apply(&current).is_err());
        let patch: SettingsPatch = serde_json::from_value(json!({"password":"short"})).unwrap();
        assert!(patch.apply(&current).is_err());
        let patch: SettingsPatch = serde_json::from_value(json!({"guestsAllowed":false})).unwrap();
        let updated = patch.apply(&current).unwrap();
        assert!(!updated.guests_allowed);
        assert_eq!(updated.allow_chat, current.allow_chat);
        assert_eq!(updated.owner_id, current.owner_id);
    }

    #[test]
    fn pagination_and_role_hierarchy_are_bounded() {
        assert_eq!(PageQuery::default().offset().unwrap(), 0);
        assert!(
            PageQuery {
                offset: Some(MAX_OFFSET + 1)
            }
            .offset()
            .is_err()
        );
        assert!(!roles::Role::Moderator.can_set_role(roles::Role::Admin, roles::Role::User));
        assert!(!roles::Role::Admin.can_set_role(roles::Role::User, roles::Role::Admin));
        assert!(roles::Role::Owner.can_set_role(roles::Role::Admin, roles::Role::Moderator));
    }
}

#[cfg(test)]
#[path = "administration_tests.rs"]
mod integration_tests;
