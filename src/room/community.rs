#![forbid(unsafe_code)]

use super::{RoomManager, control, settings};
use crate::auth::account::{validate_image_data_url, validate_text};
use crate::auth::types::AuthError;
use crate::signaling::protocol::ServerMessage;
use serde::Deserialize;
use std::time::Duration;
use uuid::Uuid;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoomIdentityUpdate {
    pub display_name: String,
    pub topic: Option<String>,
    pub description: String,
    pub image_url: Option<String>,
}

impl RoomIdentityUpdate {
    pub fn validate(&self) -> Result<(), AuthError> {
        if self.display_name.trim().is_empty()
            || !validate_text(&self.display_name, settings::MAX_DISPLAY_NAME_LEN, false)
        {
            return Err(AuthError::InvalidInput(
                "Room name must be 1–128 bytes without control characters",
            ));
        }
        if self
            .topic
            .as_ref()
            .is_some_and(|topic| !validate_text(topic, settings::MAX_TOPIC_LEN, false))
        {
            return Err(AuthError::InvalidInput(
                "Topic must be at most 512 bytes without control characters",
            ));
        }
        if !validate_text(&self.description, 1024, true) {
            return Err(AuthError::InvalidInput(
                "Description must be at most 1024 bytes",
            ));
        }
        validate_image_data_url(self.image_url.as_deref())
    }
}

impl RoomManager {
    /// Broadcaster count for a room, or `None` when it is not live or its
    /// state lock is busy; see `participant_count_for_room`.
    pub fn broadcaster_count_for_room(&self, room_id: &str) -> Option<usize> {
        let rooms = self.rooms.read().unwrap_or_else(|error| error.into_inner());
        rooms
            .get(room_id)
            .and_then(Self::readable_broadcaster_count)
    }

    pub async fn update_room_identity(
        &self,
        room_id: &str,
        owner_id: Uuid,
        identity: RoomIdentityUpdate,
    ) -> Result<bool, sqlx::Error> {
        let pool = self
            .db_pool
            .clone()
            .ok_or_else(|| sqlx::Error::InvalidArgument("Database not configured".into()))?;
        let room_id = room_id.to_owned();
        let manager = self.clone();
        let creation_guard = tokio::time::timeout(
            Duration::from_secs(5),
            manager.room_creation_lock.clone().lock_owned(),
        )
        .await
        .map_err(|_| sqlx::Error::Protocol("Room creation is busy; try again".into()))?;
        if manager.drain.is_draining()
            || manager
                .deleting_rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .contains_key(&room_id)
        {
            return Ok(false);
        }
        // Exclude creation of this one room for the update's duration. The
        // process-wide creation lock is released before any SQL so a database
        // stall here cannot block every other room's first join.
        let identity_guard = manager.begin_identity_update(&room_id);
        drop(creation_guard);
        let runtime = manager
            .rooms
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&room_id)
            .cloned();
        let control_guard = match runtime.as_ref() {
            Some(room) => Some(
                tokio::time::timeout(Duration::from_secs(5), control::lock_room(room))
                    .await
                    .map_err(|_| sqlx::Error::Protocol("Room control is busy; try again".into()))?,
            ),
            None => None,
        };
        // Complete database/runtime synchronization even if the HTTP caller
        // disconnects after admission. Queued admission remains cancellable.
        // Creation -> control prevents stale runtime installation, without
        // retaining the room state lock while waiting for PostgreSQL.
        let update = async move {
            let _identity_guard = identity_guard;
            let _control = control_guard;
            if manager.drain.is_draining() {
                return Ok(false);
            }
            let publication = if let Some(room_lock) = runtime.as_ref() {
                let room = room_lock.read().await;
                if room.deleting || !room.persisted {
                    return Ok(false);
                }
                let Some(settings) = room.settings.as_ref() else {
                    return Ok(false);
                };
                if settings.owner_id != owner_id {
                    return Ok(false);
                }
                let mut settings = settings.clone();
                settings.display_name = identity.display_name.trim().to_owned();
                settings.topic = identity.topic.clone();
                let mut value = serde_json::to_value(&settings)
                    .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
                value["description"] = serde_json::json!(identity.description);
                value["imageUrl"] = serde_json::json!(identity.image_url);
                Some((settings, value))
            } else {
                // Authorize before any uncertain write can reserve this ID.
                // Read failure has no durable side effects to quarantine.
                let saved_owner: Option<Uuid> = tokio::time::timeout(
                    Duration::from_secs(5),
                    sqlx::query_scalar("SELECT owner_id FROM rooms WHERE id=$1")
                        .bind(&room_id)
                        .fetch_optional(&pool),
                )
                .await
                .map_err(|_| sqlx::Error::Protocol("Room owner lookup timed out".into()))??;
                if saved_owner != Some(owner_id) {
                    return Ok(false);
                }
                None
            };
            let persistence = async {
                let mut transaction = pool.begin().await?;
                sqlx::query("SET LOCAL statement_timeout = '5s'")
                    .execute(&mut *transaction)
                    .await?;
                sqlx::query("SET LOCAL lock_timeout = '5s'")
                    .execute(&mut *transaction)
                    .await?;
                let updated = sqlx::query("UPDATE rooms SET display_name = $3, topic = $4, description = $5, image_url = $6 WHERE id = $1 AND owner_id = $2")
                    .bind(&room_id).bind(owner_id).bind(identity.display_name.trim()).bind(&identity.topic)
                    .bind(&identity.description).bind(&identity.image_url).execute(&mut *transaction).await?;
                if updated.rows_affected() == 0 {
                    // A live room already verified this owner under control.
                    // Its missing durable row cannot leave cached policy live.
                    return if runtime.is_some() {
                        Err(sqlx::Error::RowNotFound)
                    } else {
                        Ok(false)
                    };
                }
                transaction.commit().await?;
                Ok(true)
            };
            let updated = if let Some(room_lock) = runtime.as_ref() {
                manager
                    .persist_room(&room_id, room_lock, persistence)
                    .await?
            } else {
                let result = tokio::time::timeout(control::PERSISTENCE_TIMEOUT, persistence)
                    .await
                    .unwrap_or_else(|_| {
                        Err(sqlx::Error::Protocol(
                            "Room identity persistence deadline exceeded; commit status is unknown"
                                .into(),
                        ))
                    });
                if let Err(error) = &result
                    && control::persistence_is_indeterminate(error)
                {
                    // Keep creation excluded until this reservation is visible.
                    // No runtime may load policy from an uncertain commit.
                    manager
                        .deleting_rooms
                        .write()
                        .unwrap_or_else(|error| error.into_inner())
                        .entry(room_id.clone())
                        .or_insert_with(Uuid::new_v4);
                    tracing::warn!(
                        room_id,
                        "Room identity persistence outcome uncertain; retaining room-ID reservation until restart"
                    );
                }
                result?
            };
            if !updated {
                return Ok(false);
            }
            if let Some(room_lock) = runtime.as_ref()
                && let Some((settings, value)) = publication
            {
                // Publication is mandatory after COMMIT, not part of its timeout.
                let mut room = room_lock.write().await;
                room.settings = Some(settings);
                room.policy_revision = room.policy_revision.wrapping_add(1);
                room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings: value });
            }
            Ok(true)
        };
        tokio::spawn(update)
            .await
            .map_err(|error| sqlx::Error::Protocol(format!("Room identity task failed: {error}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn room_identity_requires_bounded_plain_text_and_raster_image() {
        let mut identity = RoomIdentityUpdate {
            display_name: "Our room".into(),
            topic: None,
            description: "A place to talk".into(),
            image_url: None,
        };
        assert!(identity.validate().is_ok());
        identity.display_name = " ".into();
        assert!(identity.validate().is_err());
        identity.display_name = "Valid".into();
        identity.topic = Some("x".repeat(513));
        assert!(identity.validate().is_err());
        identity.topic = None;
        identity.image_url = Some("data:image/svg+xml;base64,PHN2Zz4=".into());
        assert!(identity.validate().is_err());
    }
}
