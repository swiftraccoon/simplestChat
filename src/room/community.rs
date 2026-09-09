#![forbid(unsafe_code)]

use super::{RoomManager, settings};
use crate::auth::account::{validate_image_data_url, validate_text};
use crate::auth::types::AuthError;
use crate::signaling::protocol::ServerMessage;
use serde::Deserialize;
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
    pub fn broadcaster_count_for_room(&self, room_id: &str) -> usize {
        let rooms = self.rooms.read().unwrap_or_else(|error| error.into_inner());
        rooms
            .get(room_id)
            .and_then(|room| room.try_read().ok())
            .map(|room| {
                room.participants
                    .values()
                    .filter(|participant| !participant.producers.is_empty())
                    .count()
            })
            .unwrap_or(0)
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
        let rooms = self.rooms.clone();
        let creation_lock = self.room_creation_lock.clone();
        // Complete database/runtime synchronization even if the HTTP caller
        // disconnects. Use the existing creation -> runtime -> database order,
        // preventing a concurrent join/delete from installing stale identity.
        let update = async move {
            let _creation_guard = creation_lock.lock().await;
            let runtime = rooms
                .read()
                .unwrap_or_else(|error| error.into_inner())
                .get(&room_id)
                .cloned();
            let mut runtime = match runtime.as_ref() {
                Some(room) => Some(room.write().await),
                None => None,
            };
            if runtime
                .as_ref()
                .is_some_and(|room| room.deleting || !room.persisted)
            {
                return Ok(false);
            }
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
                return Ok(false);
            }
            transaction.commit().await?;
            if let Some(room) = runtime.as_mut() {
                if let Some(settings) = room.settings.as_mut() {
                    settings.display_name = identity.display_name.trim().to_owned();
                    settings.topic = identity.topic.clone();
                }
                room.policy_revision = room.policy_revision.wrapping_add(1);
                if let Some(settings) = room.settings.as_ref() {
                    let mut value = serde_json::to_value(settings)
                        .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
                    value["description"] = serde_json::json!(identity.description);
                    value["imageUrl"] = serde_json::json!(identity.image_url);
                    room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings: value });
                }
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
