//! Lobby admission: capacity checks, visit notes and admitting or denying waiting
//! participants.

use super::super::*;

impl RoomManager {
    /// Admit a participant from the lobby into the room.
    ///
    /// Moves the target from `room.lobby` to `room.participants`, sends
    /// `LobbyAdmitted` + `RoomJoined` to the admitted participant via their
    /// stored sender, and broadcasts `ParticipantJoined` to existing participants.
    /// Refuses one more participant once the room's own limit or the
    /// deployment's ceiling is reached. Joins and lobby admissions share it,
    /// so a participant queued while the room had space cannot be admitted
    /// past either limit later.
    pub(in crate::room) fn ensure_room_has_space(&self, room: &Room) -> Result<()> {
        if let Some(maximum) = room
            .settings
            .as_ref()
            .and_then(|settings| settings.max_participants)
        {
            let Ok(maximum) = usize::try_from(maximum) else {
                anyhow::bail!("Room capacity is unavailable");
            };
            if maximum == 0 || room.participants.len() >= maximum {
                anyhow::bail!("Room is full");
            }
        }
        // The deployment's ceiling applies to every room, ad hoc or saved,
        // beneath whatever the room itself allows.
        if let Some(ceiling) = self.max_participants_per_room
            && room.participants.len() >= ceiling
        {
            anyhow::bail!("Room is full");
        }
        Ok(())
    }

    /// Call only after a successful admission and after releasing room locks.
    pub(crate) fn note_room_visit(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        authenticated: bool,
    ) {
        if authenticated && let Some(pool) = self.db_pool.as_ref() {
            crate::auth::discovery::note_room_visit(
                pool,
                participant_id.as_uuid(),
                room_id.as_str(),
            );
        }
    }

    pub(crate) async fn admit_from_lobby(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_id: &ParticipantId,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let _control = control::lock_room(&room_lock).await;
        let room = room_lock.read().await;

        // Verify moderator exists and has permission
        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to admit from lobby");
        }
        drop(room);
        self.admit_authorized_lobby(
            room_id,
            target_id,
            control::RoomControl::from_guard(_control),
            || Ok(()),
        )
        .await
    }

    /// Complete admission under an existing control guard. The caller validates
    /// its own actor; the final callback runs after all preference/state waits.
    pub(in crate::room) async fn admit_authorized_lobby(
        &self,
        room_id: &RoomId,
        target_id: &ParticipantId,
        _control: control::RoomControl,
        authorize: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;
        if !room.lobby.contains_key(target_id) {
            anyhow::bail!("Participant not found in lobby");
        }
        self.ensure_room_has_space(&room)?;

        room.prune_expired_bans(std::time::Instant::now());
        if room
            .lobby
            .get(target_id)
            .is_some_and(|entry| room.identity_is_banned(target_id, entry.authenticated, entry.ip))
        {
            anyhow::bail!("Participant is banned from this room");
        }

        if room
            .lobby
            .get(target_id)
            .is_some_and(|entry| entry.sender.is_closed())
        {
            anyhow::bail!("Participant has already disconnected");
        }
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        let authenticated = room.lobby.get(target_id).unwrap().authenticated;
        drop(room);
        let account_preferences = self
            .membership_chat_preferences(authenticated, target_id)
            .await?;
        let mut room = room_lock.write().await;
        authorize()?;
        let admission = self.drain.admit()?;
        room.ensure_live()?;
        if room
            .lobby
            .get(target_id)
            .is_none_or(|entry| entry.sender.is_closed())
        {
            anyhow::bail!("Participant has already disconnected");
        }

        // Remove from lobby
        let entry = room
            .lobby
            .remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Build the participants list (before inserting the new one)
        let participants: Vec<ParticipantInfo> = room
            .participants
            .values()
            .map(|p| ParticipantInfo {
                id: p.id,
                name: p.name.clone(),
                producers: p
                    .producers
                    .iter()
                    .map(|(id, (kind, source))| ProducerMetadata {
                        id: id.clone(),
                        kind: *kind,
                        source: source.clone(),
                    })
                    .collect(),
                role: p.role,
                authenticated: p.authenticated,
                chat_style: p.chat_style.clone(),
            })
            .collect();

        let (admitted_role, admitted_punitive) = lobby_admission_state(&room, &entry);

        // Insert into participants
        let mut social = social::ParticipantSocial::new(room.social.next_sequence);
        if let Some(preferences) = &account_preferences {
            social.apply_account_preferences(preferences);
        }
        let participant = Participant {
            id: entry.participant_id,
            social,
            name: entry.name.clone(),
            sender: entry.sender.clone(),
            media_session_id: entry.media_session_id,
            producers: HashMap::new(),
            role: admitted_role,
            punitive: admitted_punitive,
            authenticated: entry.authenticated,
            ip: entry.ip,
            chat_style: entry.chat_style.clone(),
        };
        room.participants.insert(entry.participant_id, participant);

        self.metrics.inc_lobby_admission();
        // Clear the connection task's lobby guard BEFORE notifying the client,
        // so its follow-up media-setup messages are not rejected.
        entry.in_lobby_flag.store(false, Ordering::Release);
        drop(admission);

        // Send LobbyAdmitted to the admitted participant
        if let Ok(json) = serde_json::to_string(&ServerMessage::LobbyAdmitted) {
            let _ = try_send_essential(
                &room.metrics,
                &entry.sender,
                crate::OutboundJson::from(json),
            );
        }

        // Send RoomJoined to the admitted participant (use stored reconnect token)
        let reconnect_token = if entry.reconnect_token.is_empty() {
            uuid::Uuid::new_v4().to_string()
        } else {
            entry.reconnect_token.clone()
        };
        let room_settings = room.settings.clone();
        if let Ok(json) = serde_json::to_string(&ServerMessage::RoomJoined {
            participant_id: entry.participant_id,
            participants,
            reconnect_token: reconnect_token.into(),
            your_role: admitted_role,
            your_name: entry.name.clone(),
            your_chat_style: entry.chat_style.clone(),
            room_settings,
        }) {
            let _ = try_send_essential(
                &room.metrics,
                &entry.sender,
                crate::OutboundJson::from(json),
            );
        }

        // Broadcast ParticipantJoined to existing participants (excluding the admitted one)
        room.broadcast_except(
            &entry.participant_id,
            &ServerMessage::ParticipantJoined {
                participant_id: entry.participant_id,
                participant_name: entry.name.clone(),
                role: admitted_role,
                authenticated: entry.authenticated,
                chat_style: entry.chat_style.clone(),
            },
        );

        info!("Admitted {} to room {}", target_id, room_id);
        room.notify_lobby_status();
        drop(room);
        _control.release();
        self.note_room_visit(room_id, &entry.participant_id, entry.authenticated);
        Ok(())
    }

    /// Deny a participant from the lobby.
    ///
    /// Removes the target from `room.lobby` and sends `LobbyDenied` to them.
    pub(crate) async fn deny_from_lobby(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_id: &ParticipantId,
        reason: Option<String>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let _control = control::lock_room(&room_lock).await;
        let room = room_lock.read().await;

        // Verify moderator exists and has permission
        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
        if !moderator.role.can_admit_lobby() {
            anyhow::bail!("Insufficient permissions to deny from lobby");
        }
        drop(room);
        self.deny_authorized_lobby(
            room_id,
            target_id,
            reason,
            control::RoomControl::from_guard(_control),
            || Ok(()),
        )
        .await
    }

    pub(in crate::room) async fn deny_authorized_lobby(
        &self,
        room_id: &RoomId,
        target_id: &ParticipantId,
        reason: Option<String>,
        _control: control::RoomControl,
        authorize: impl FnOnce() -> Result<()>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;
        authorize()?;
        if !room.lobby.contains_key(target_id) {
            anyhow::bail!("Participant not found in lobby");
        }
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // Remove from lobby
        let entry = room
            .lobby
            .remove(target_id)
            .ok_or_else(|| anyhow::anyhow!("Participant not found in lobby"))?;

        // Send LobbyDenied to the denied participant
        if let Ok(json) = serde_json::to_string(&ServerMessage::LobbyDenied { reason }) {
            let _ = try_send_essential(
                &room.metrics,
                &entry.sender,
                crate::OutboundJson::from(json),
            );
        }

        info!("Denied {} from room {}", target_id, room_id);
        Ok(())
    }
}
