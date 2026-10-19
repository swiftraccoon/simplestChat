//! Room settings and topic changes: authorization, validation, persistence as the
//! commit point, then publication and producer revocation.

use super::super::*;

impl RoomManager {
    pub(in crate::room) fn validate_capacity(
        label: &str,
        value: Option<Option<i32>>,
        maximum: i32,
    ) -> Result<()> {
        if let Some(Some(value)) = value
            && !(1..=maximum).contains(&value)
        {
            anyhow::bail!("{label} must be between 1 and {maximum}");
        }
        Ok(())
    }

    /// Update room settings at runtime (Admin+ only).
    ///
    /// Persists a partial update for database-backed rooms before publishing the
    /// settings, revoking disallowed producers, and broadcasting the change.
    /// Admitted work survives cancellation of the requesting connection.
    pub(crate) async fn update_room_settings(
        &self,
        room_id: &RoomId,
        admin_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        patch: SettingsPatch,
    ) -> Result<()> {
        let admin_id = *admin_id;
        let expected_sender = expected_sender.clone();
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .update_room_settings_controlled(
                    &room_id,
                    &admin_id,
                    &expected_sender,
                    patch,
                    control,
                )
                .await
        })
        .await
    }

    pub(in crate::room) async fn update_room_settings_controlled(
        &self,
        room_id: &RoomId,
        admin_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        patch: SettingsPatch,
        control: control::RoomControl,
    ) -> Result<()> {
        // Authorize before expensive password work. Control retains this
        // membership/permission order while brief state locks allow chat/media.
        let room_lock = self.get_room(room_id)?;
        {
            let room = room_lock.read().await;
            let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
            }
        }

        Self::validate_capacity("max participants", patch.max_participants, 10_000)?;
        Self::validate_capacity("max broadcasters", patch.max_broadcasters, 1_000)?;
        let mut admin_mutation_reserved = false;
        if let Some(Some(value)) = patch.password.as_ref() {
            if !settings::valid_new_room_password(value.expose_secret()) {
                anyhow::bail!("Room password must be 8-256 bytes without control characters");
            }

            let mut room = room_lock.write().await;
            let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
            if !admin.role.can_change_settings() {
                anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
            }
            let now = std::time::Instant::now();
            if room
                .last_password_hash_at
                .is_some_and(|previous| now.duration_since(previous) < ROOM_PASSWORD_HASH_COOLDOWN)
            {
                anyhow::bail!("Room password updates are rate limited");
            }
            if !room.admin_mutations.reserve(now) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            admin_mutation_reserved = true;
            room.last_password_hash_at = Some(now);
        }

        // Argon2 is deliberately expensive; keep it off the async executor and
        // bound concurrent work so settings updates cannot exhaust CPU threads.
        let password_hash = match patch.password.as_ref() {
            Some(Some(value)) => {
                let value = value.clone();
                let permit = self
                    .password_hash_work
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| anyhow::anyhow!("Password service is busy; try again"))?;
                let hash = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    crate::auth::password::hash_password(value.expose_secret())
                })
                .await
                .map_err(|error| {
                    warn!(%room_id, %error, "Password hash task failed");
                    anyhow::anyhow!("Password update failed")
                })?
                .map_err(|error| {
                    warn!(%room_id, %error, "Password hashing failed");
                    anyhow::anyhow!("Password update failed")
                })?;
                Some(Some(hash))
            }
            Some(None) => Some(None),
            None => None,
        };

        let mut room = room_lock.write().await;
        let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
        if !admin.role.can_change_settings() {
            anyhow::bail!("Insufficient permissions to change room settings (requires Admin+)");
        }

        let current = room
            .settings
            .clone()
            .unwrap_or_else(|| settings::RoomSettings::defaults(room_id));
        let password_changed = match patch.password.as_ref() {
            Some(Some(_)) => true,
            Some(None) => room.password_hash.is_some() || current.password_protected,
            None => false,
        };
        let mut updated = current.clone();
        settings::apply_settings_update(&mut updated, &patch);
        if matches!(
            (updated.max_broadcasters, updated.max_participants),
            (Some(broadcasters), Some(participants)) if broadcasters > participants
        ) {
            anyhow::bail!("Maximum broadcasters cannot exceed maximum participants");
        }
        if !password_changed && updated == current {
            return Ok(());
        }
        if !admin_mutation_reserved && !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // For DB-backed rooms persistence is the commit point. Do not advertise
        // or enforce a setting that failed to persist.
        let persisted = room.persisted;
        drop(room);
        if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room settings are temporarily unavailable"))?;
            self.persist_room(
                room_id,
                &room_lock,
                settings::update_room_settings(pool, room_id, &patch, password_hash.clone()),
            )
            .await
            .map_err(|error| {
                warn!(%room_id, %error, "Failed to persist room settings");
                anyhow::anyhow!("Room settings could not be saved")
            })?;
        }

        let mut room = room_lock.write().await;
        room.ensure_live()?;
        if let Some(hash) = password_hash {
            room.password_hash = hash;
        }
        room.settings = Some(updated.clone());
        room.policy_revision = room.policy_revision.wrapping_add(1);
        let revoked = Self::revoke_unauthorized_producers(
            &mut room,
            None,
            "Room policy no longer permits this producer",
        );
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();
        room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings: updated });
        drop(room);
        control.release();

        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;
        info!("Room settings updated for room {} by {}", room_id, admin_id);
        Ok(())
    }

    /// Set the room topic (Admin+ only).
    ///
    /// Updates the in-memory topic and broadcasts TopicChanged to all participants.
    pub(crate) async fn set_topic(
        &self,
        room_id: &RoomId,
        admin_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        topic: String,
    ) -> Result<()> {
        let admin_id = *admin_id;
        let expected_sender = expected_sender.clone();
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .set_topic_controlled(&room_id, &admin_id, &expected_sender, topic, control)
                .await
        })
        .await
    }

    pub(in crate::room) async fn set_topic_controlled(
        &self,
        room_id: &RoomId,
        admin_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        topic: String,
        control: control::RoomControl,
    ) -> Result<()> {
        if topic.len() > 512 || topic.chars().any(char::is_control) {
            anyhow::bail!("Room topic must be at most 512 characters without control characters");
        }
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let admin = Self::participant_for_sender(&room, admin_id, expected_sender)?;
        if !admin.role.can_change_settings() {
            anyhow::bail!("Insufficient permissions to change room topic (requires Admin+)");
        }
        if room
            .settings
            .as_ref()
            .and_then(|settings| settings.topic.as_deref())
            == Some(topic.as_str())
        {
            return Ok(());
        }
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        let persisted = room.persisted;
        drop(room);
        if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Room topic is temporarily unavailable"))?;
            self.persist_room(room_id, &room_lock, async {
                let result =
                    sqlx::query("UPDATE rooms SET topic = $1, updated_at = now() WHERE id = $2")
                        .bind(&topic)
                        .bind(room_id)
                        .execute(pool)
                        .await?;
                if result.rows_affected() != 1 {
                    return Err(sqlx::Error::RowNotFound);
                }
                Ok(())
            })
            .await
            .map_err(|error| {
                warn!(%room_id, %error, "Failed to persist room topic");
                anyhow::anyhow!("Room topic could not be saved")
            })?;
        }

        let mut room = room_lock.write().await;
        room.ensure_live()?;
        room.settings
            .get_or_insert_with(|| settings::RoomSettings::defaults(room_id))
            .topic = Some(topic.clone());
        room.broadcast_all(&ServerMessage::TopicChanged {
            topic: topic.clone(),
            changed_by: admin_id.to_string(),
        });
        drop(room);
        control.release();
        info!("Topic set for room {} by {}", room_id, admin_id);
        Ok(())
    }
}
