//! Sanctions and standing: camera and text sanctions, kicks, bans, roles and voice
//! requests, each committed through the room's control order.

use super::super::*;

impl RoomManager {
    #[expect(
        clippy::too_many_arguments,
        reason = "moderation boundary keeps authorization identity and mutation fields explicit"
    )]
    pub(in crate::room) async fn update_punitive_state(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        kind: moderation::PunitiveKind,
        enabled: bool,
        reason: Option<&str>,
    ) -> Result<bool> {
        let moderator_id = *moderator_id;
        let expected_sender = expected_sender.clone();
        let target_participant_id = *target_participant_id;
        let reason = reason.map(String::from);
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .update_punitive_state_controlled(
                    &room_id,
                    &moderator_id,
                    &expected_sender,
                    &target_participant_id,
                    kind,
                    enabled,
                    reason.as_deref(),
                    control,
                )
                .await
        })
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "owned moderation keeps authorized identities, sanction fields and control guard explicit"
    )]
    pub(in crate::room) async fn update_punitive_state_controlled(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        kind: moderation::PunitiveKind,
        enabled: bool,
        reason: Option<&str>,
        control: control::RoomControl,
    ) -> Result<bool> {
        if reason.is_some_and(|value| !moderation::valid_reason(value)) {
            anyhow::bail!(
                "Moderation reason must be at most 256 characters without control characters"
            );
        }

        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        let (
            moderator_role,
            moderator_authenticated,
            moderator_name,
            target_authenticated,
            target_ip,
            target_name,
        ) = {
            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;
            (
                moderator.role,
                moderator.authenticated,
                moderator.name.clone(),
                target.authenticated,
                target.ip,
                target.name.clone(),
            )
        };
        let target_participant_ids =
            moderation_cohort(&room, target_participant_id, moderator_role)?;
        let target_lobby_ids =
            guest_ip_lobby_cohort(&room, target_authenticated, target_ip, moderator_role)?;
        let sanction_key =
            SanctionKey::for_identity(target_participant_id, target_authenticated, target_ip);
        if punitive_cohort_matches(
            &room,
            &target_participant_ids,
            &target_lobby_ids,
            &sanction_key,
            kind,
            enabled,
        ) {
            return Ok(false);
        }
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }
        let event = moderation::ModerationEvent::new(
            moderation::ModerationAction::for_punitive(kind, enabled),
            moderation::Actor {
                id: moderator_id,
                name: &moderator_name,
            },
            moderation::Target {
                id: target_participant_id.into(),
                name: &target_name,
                authenticated: target_authenticated,
                ip: target_ip,
            },
            reason,
            None,
            None,
        );

        let persisted = room.persisted;
        drop(room);
        if persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Moderation service is temporarily unavailable"))?;
            let applied_by = moderator_authenticated
                .then(|| moderator_id.as_uuid())
                .ok_or_else(|| anyhow::anyhow!("A registered moderator is required"))?;
            let target_user = if target_authenticated {
                Some(target_participant_id.as_uuid())
            } else {
                None
            };
            if target_user.is_none() && target_ip.is_none() {
                anyhow::bail!("The target cannot be identified for a durable sanction");
            }
            self.persist_room(
                room_id,
                &room_lock,
                moderation::set_punitive_state(
                    pool,
                    room_id,
                    target_user,
                    target_ip,
                    kind,
                    enabled,
                    reason,
                    applied_by,
                    &event,
                ),
            )
            .await
            .map_err(|error| {
                warn!(%room_id, %error, "Failed to persist participant sanction");
                anyhow::anyhow!("Moderation state could not be saved")
            })?;
        }

        let mut room = room_lock.write().await;
        room.ensure_live()?;
        if !persisted {
            room.social.record_moderation_event(event);
        }
        apply_punitive_to_cohort(
            &mut room,
            &target_participant_ids,
            &target_lobby_ids,
            sanction_key,
            kind,
            enabled,
        );
        room.policy_revision = room.policy_revision.wrapping_add(1);

        for affected_participant_id in &target_participant_ids {
            let notification = punitive_notification(kind, enabled, *affected_participant_id);
            room.broadcast_all(&notification);
        }
        for affected_participant_id in &target_lobby_ids {
            let notification = punitive_notification(kind, enabled, *affected_participant_id);
            if let (Some(entry), Ok(json)) = (
                room.lobby.get(affected_participant_id),
                serde_json::to_string(&notification),
            ) {
                let _ = try_send_essential(
                    &room.metrics,
                    &entry.sender,
                    crate::OutboundJson::from(json),
                );
            }
        }

        let revoked = if kind == moderation::PunitiveKind::CamBanned && enabled {
            let mut revoked = Vec::new();
            for affected_participant_id in &target_participant_ids {
                revoked.extend(Self::revoke_unauthorized_producers(
                    &mut room,
                    Some(affected_participant_id),
                    "Camera access revoked by moderator",
                ));
            }
            revoked
        } else {
            Vec::new()
        };
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();
        drop(room);

        control.release();
        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;
        Ok(true)
    }

    /// Force-close all video/screen producers for a target participant
    pub(crate) async fn close_cam(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
    ) -> Result<()> {
        // Collect producer IDs to close (under room lock)
        let (producers_to_close, target_media_session_id): (Vec<String>, uuid::Uuid) = {
            let room_lock = self.get_room(room_id)?;
            let _control = control::lock_room(&room_lock).await;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to moderate this participant");
            }

            // Collect video producers (camera and screen). An authorized retry
            // against an already-closed camera is a successful no-op.
            let producers: Vec<String> = target
                .producers
                .iter()
                .filter(|(_, record)| record.kind == MediaKind::Video)
                .map(|(id, _)| id.clone())
                .collect();
            let target_media_session_id = target.media_session_id;
            if producers.is_empty() {
                return Ok(());
            }
            if !room.admin_mutations.reserve(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            (producers, target_media_session_id)
        };

        // Close producers outside room lock (async mediasoup IPC)
        let target_media_id =
            Self::media_participant_id(room_id, target_participant_id, target_media_session_id);
        for producer_id in &producers_to_close {
            if let Err(e) = self
                .media_server
                .transport_manager()
                .close_producer(&target_media_id, producer_id)
                .await
            {
                warn!(
                    "Failed to close producer {} for close_cam: {}",
                    producer_id, e
                );
            }
        }

        // Remove from room tracking and broadcast
        {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;
            room.ensure_live()?;
            if let Some(target) = room
                .participants
                .get_mut(target_participant_id)
                .filter(|target| target.media_session_id == target_media_session_id)
            {
                for pid in &producers_to_close {
                    target.producers.remove(pid);
                }
                for pid in &producers_to_close {
                    room.producer_to_participant.remove(pid);
                    room.broadcast_all(&ServerMessage::ForceClosedProducer {
                        producer_id: pid.clone(),
                        reason: "Closed by moderator".to_string(),
                    });
                    room.broadcast_all(&ServerMessage::ProducerClosed {
                        producer_id: pid.clone(),
                    });
                }
            }
        }

        info!(
            "close_cam: {} closed {} video producers for {} in room {}",
            moderator_id,
            producers_to_close.len(),
            target_participant_id,
            room_id
        );
        Ok(())
    }

    /// Ban a participant from producing video/screen (close existing + prevent future)
    pub(crate) async fn cam_ban(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        reason: Option<&str>,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::CamBanned,
                true,
                reason,
            )
            .await?;

        if changed {
            info!(
                "cam_ban: {} cam-banned {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Remove cam ban from a participant
    pub(crate) async fn cam_unban(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::CamBanned,
                false,
                None,
            )
            .await?;

        if changed {
            info!(
                "cam_unban: {} cam-unbanned {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Mute a participant's text chat
    pub(crate) async fn text_mute(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::Muted,
                true,
                None,
            )
            .await?;

        if changed {
            info!(
                "text_mute: {} text-muted {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Unmute a participant's text chat
    pub(crate) async fn text_unmute(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
    ) -> Result<()> {
        let changed = self
            .update_punitive_state(
                room_id,
                moderator_id,
                expected_sender,
                target_participant_id,
                moderation::PunitiveKind::Muted,
                false,
                None,
            )
            .await?;

        if changed {
            info!(
                "text_unmute: {} text-unmuted {} in room {}",
                moderator_id, target_participant_id, room_id
            );
        }
        Ok(())
    }

    /// Kick a participant from the room
    pub(crate) async fn kick_participant(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        reason: Option<&str>,
        report_id: Option<uuid::Uuid>,
    ) -> Result<()> {
        // Check permissions and remove under room lock
        let target_media_session_id = {
            let room_lock = self.get_room(room_id)?;
            let _control = control::lock_room(&room_lock).await;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            if !moderator.role.can_moderate(target.role) {
                anyhow::bail!("Insufficient permissions to kick this participant");
            }
            let event = moderation::ModerationEvent::new(
                moderation::ModerationAction::Kick,
                moderation::Actor {
                    id: moderator_id,
                    name: &moderator.name,
                },
                moderation::Target {
                    id: target_participant_id.into(),
                    name: &target.name,
                    authenticated: target.authenticated,
                    ip: target.ip,
                },
                reason,
                None,
                report_id,
            );
            if !room.admin_mutations.reserve(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            // A kick leaves no sanction row, so its history entry (and the
            // report it answers) is written first; the target may leave in the
            // meantime, in which case the entry stands and the kick is moot.
            if room.persisted {
                let pool = self.db_pool.as_ref().ok_or_else(|| {
                    anyhow::anyhow!("Moderation service is temporarily unavailable")
                })?;
                drop(room);
                self.persist_room(
                    room_id,
                    &room_lock,
                    moderation::persist_event(pool, room_id, &event),
                )
                .await
                .map_err(moderation_history_error)?;
                room = room_lock.write().await;
                room.ensure_live()?;
                if !room.participants.contains_key(target_participant_id) {
                    anyhow::bail!("Target participant not found");
                }
            } else {
                if let Some(report_id) = report_id {
                    room.social.link_report(&report_id.to_string())?;
                }
                room.social.record_moderation_event(event);
            }

            // Broadcast kick to everyone (including the target) before removing
            room.broadcast_all(&ServerMessage::ParticipantKicked {
                participant_id: *target_participant_id,
                reason: reason.map(String::from),
            });

            let target = room
                .participants
                .remove(target_participant_id)
                .expect("target was checked under the same room lock");
            let target_media_session_id = target.media_session_id;
            for producer_id in target.producers.keys() {
                room.producer_to_participant.remove(producer_id);
            }

            // Broadcast leave to remaining participants
            room.broadcast_participant_left(target_participant_id);
            room.notify_lobby_status();
            target_media_session_id
        };

        // Clean up media outside lock
        let target_media_id =
            Self::media_participant_id(room_id, target_participant_id, target_media_session_id);
        if let Err(e) = self
            .media_server
            .transport_manager()
            .remove_participant(&target_media_id)
            .await
        {
            warn!(
                "Failed to clean up media for kicked participant {}: {}",
                target_participant_id, e
            );
        }

        info!(
            "kick: {} kicked {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Ban a participant from the room (kick + durable state for persisted rooms)
    #[expect(
        clippy::too_many_arguments,
        reason = "a ban names its room, actor, socket, target, reason, duration and the report it answers"
    )]
    pub(crate) async fn ban_participant(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        reason: Option<&str>,
        duration: Option<u64>,
        report_id: Option<uuid::Uuid>,
    ) -> Result<()> {
        let moderator_id = *moderator_id;
        let expected_sender = expected_sender.clone();
        let target_participant_id = *target_participant_id;
        let reason = reason.map(String::from);
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .ban_participant_controlled(
                    &room_id,
                    &moderator_id,
                    &expected_sender,
                    &target_participant_id,
                    reason.as_deref(),
                    duration,
                    report_id,
                    control,
                )
                .await
        })
        .await
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "owned ban retains the authorized actor, target, duration and control guard"
    )]
    pub(in crate::room) async fn ban_participant_controlled(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        reason: Option<&str>,
        duration: Option<u64>,
        report_id: Option<uuid::Uuid>,
        control: control::RoomControl,
    ) -> Result<()> {
        const MAX_BAN_DURATION_SECS: u64 = 10 * 365 * 24 * 60 * 60;
        if duration.is_some_and(|seconds| seconds == 0 || seconds > MAX_BAN_DURATION_SECS) {
            anyhow::bail!("Ban duration must be between 1 second and 10 years");
        }
        if reason.is_some_and(|value| !moderation::valid_reason(value)) {
            anyhow::bail!("Ban reason must be at most 256 characters without control characters");
        }
        let runtime_expiry = duration.and_then(|seconds| {
            std::time::Instant::now().checked_add(std::time::Duration::from_secs(seconds))
        });

        // The control guard pins authority and membership across unlocked SQL.
        // An unauthenticated target with an IP represents the entire live guest
        // cohort on that IP, matching the durable/in-memory ban key.
        let target_sessions = {
            let room_lock = self.get_room(room_id)?;
            let mut room = room_lock.write().await;

            let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;
            let target = room
                .participants
                .get(target_participant_id)
                .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

            let target_participant_ids =
                moderation_cohort(&room, target_participant_id, moderator.role)?;
            let moderator_authenticated = moderator.authenticated;
            let moderator_name = moderator.name.clone();
            let target_authenticated = target.authenticated;
            let target_ip = target.ip;
            let target_name = target.name.clone();
            let target_lobby_ids =
                guest_ip_lobby_cohort(&room, target_authenticated, target_ip, moderator.role)?;
            room.social.reserve_ban()?;
            if !room.admin_mutations.reserve(std::time::Instant::now()) {
                anyhow::bail!("Room administration changes are rate limited");
            }
            let expires_at = duration.and_then(|seconds| {
                chrono::Utc::now().checked_add_signed(chrono::Duration::seconds(seconds as i64))
            });
            let event = moderation::ModerationEvent::new(
                moderation::ModerationAction::Ban,
                moderation::Actor {
                    id: moderator_id,
                    name: &moderator_name,
                },
                moderation::Target {
                    id: target_participant_id.into(),
                    name: &target_name,
                    authenticated: target_authenticated,
                    ip: target_ip,
                },
                reason,
                expires_at,
                report_id,
            );

            // Persist first so a successful response never represents only a
            // transient ban that disappears when the runtime room is removed.
            let persisted = room.persisted;
            drop(room);
            if persisted {
                let pool = self
                    .db_pool
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("Ban service is temporarily unavailable"))?;
                let applied_by = moderator_authenticated
                    .then(|| moderator_id.as_uuid())
                    .ok_or_else(|| anyhow::anyhow!("A registered moderator is required"))?;
                let target_user = target_authenticated.then(|| target_participant_id.as_uuid());
                if target_user.is_none() && target_ip.is_none() {
                    anyhow::bail!("The target cannot be identified for a durable ban");
                }
                self.persist_room(
                    room_id,
                    &room_lock,
                    moderation::persist_ban(
                        pool,
                        room_id,
                        target_user,
                        target_ip,
                        reason,
                        expires_at,
                        applied_by,
                        &event,
                    ),
                )
                .await
                .map_err(|error| {
                    if matches!(error, sqlx::Error::RowNotFound) {
                        return anyhow::anyhow!("Report not found");
                    }
                    warn!(%room_id, %error, "Failed to persist room ban");
                    anyhow::anyhow!("Ban could not be saved")
                })?;
            }

            let mut room = room_lock.write().await;
            room.ensure_live()?;
            if !persisted {
                if let Some(report_id) = report_id {
                    room.social.link_report(&report_id.to_string())?;
                }
                room.social.record_moderation_event(event);
            }
            // Add to ban list — prevents rejoining
            let banned_ids: Vec<ParticipantId> = target_participant_ids
                .iter()
                .chain(target_lobby_ids.iter())
                .cloned()
                .collect();
            record_banned_cohort(
                &mut room,
                &banned_ids,
                target_authenticated,
                target_ip,
                runtime_expiry,
            );
            room.social.record_ban(
                target_name,
                target_authenticated,
                banned_ids.clone(),
                target_ip,
                reason,
                duration,
            );
            room.policy_revision = room.policy_revision.wrapping_add(1);

            let mut target_sessions = Vec::with_capacity(target_participant_ids.len());
            for affected_participant_id in &target_participant_ids {
                // Notify every socket in the affected IP cohort before removing it.
                room.broadcast_all(&ServerMessage::ParticipantBanned {
                    participant_id: *affected_participant_id,
                    reason: reason.map(String::from),
                });

                let target = room
                    .participants
                    .remove(affected_participant_id)
                    .expect("moderation cohort is stable under the room control guard");
                for producer_id in target.producers.keys() {
                    room.producer_to_participant.remove(producer_id);
                }
                target_sessions.push((*affected_participant_id, target.media_session_id));

                room.broadcast_participant_left(affected_participant_id);
            }
            // The payload has no per-recipient field: serialize it once for the
            // whole cohort, as the other lobby fan-outs do.
            let lobby_denied = serde_json::to_string(&ServerMessage::LobbyDenied {
                reason: Some(reason.map_or_else(|| "Banned from room".to_string(), String::from)),
            })
            .ok()
            .map(crate::OutboundJson::from);
            for affected_participant_id in target_lobby_ids {
                if let Some(entry) = room.lobby.remove(&affected_participant_id)
                    && let Some(json) = lobby_denied.clone()
                {
                    let _ = try_send_essential(&room.metrics, &entry.sender, json);
                }
            }
            room.notify_lobby_status();
            target_sessions
        };
        control.release();

        // Clean up media outside lock
        for (affected_participant_id, media_session_id) in target_sessions {
            let target_media_id =
                Self::media_participant_id(room_id, &affected_participant_id, media_session_id);
            if let Err(e) = self
                .media_server
                .transport_manager()
                .remove_participant(&target_media_id)
                .await
            {
                warn!(
                    "Failed to clean up media for banned participant {}: {}",
                    affected_participant_id, e
                );
            }
        }

        info!(
            "ban: {} banned {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Remove a participant from the room's ban list
    pub(crate) async fn unban_participant(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_user_id: &UserId,
    ) -> Result<()> {
        let moderator_id = *moderator_id;
        let expected_sender = expected_sender.clone();
        let target_user_id = *target_user_id;
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .unban_participant_controlled(
                    &room_id,
                    &moderator_id,
                    &expected_sender,
                    &target_user_id,
                    control,
                )
                .await
        })
        .await
    }

    pub(in crate::room) async fn unban_participant_controlled(
        &self,
        room_id: &RoomId,
        moderator_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_user_id: &UserId,
        control: control::RoomControl,
    ) -> Result<()> {
        let target_participant_id = &ParticipantId::from(target_user_id.as_uuid());
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;
        room.ensure_live()?;

        let moderator = Self::participant_for_sender(&room, moderator_id, expected_sender)?;

        if moderator.role < roles::Role::Admin {
            anyhow::bail!("Insufficient permissions to unban (requires Admin+)");
        }
        let moderator_name = moderator.name.clone();

        room.prune_expired_bans(std::time::Instant::now());
        if room
            .banned_guest_participants
            .contains(target_participant_id)
        {
            anyhow::bail!(
                "Guest bans are identity/IP-scoped and cannot be removed by participant ID"
            );
        }

        let in_memory_exists = room.banned_participants.contains_key(target_participant_id);
        let persisted_target = room.persisted.then(|| target_participant_id.as_uuid());
        if !in_memory_exists && persisted_target.is_none() {
            anyhow::bail!("Participant is not banned");
        }
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        // Also clear any persisted user ban (target id == user uuid for
        // registered users; guests have no stable identity to unban). Keep the
        // control guard through this write so deletion/recreation cannot
        // redirect a delayed unban into a replacement room. Readers remain free.
        drop(room);
        let mut persisted_removed = false;
        if let Some(uid) = persisted_target {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Ban service is temporarily unavailable"))?;
            persisted_removed = self
                .persist_room(
                    room_id,
                    &room_lock,
                    moderation::remove_user_ban(
                        pool,
                        room_id,
                        uid,
                        moderation::Actor {
                            id: moderator_id,
                            name: &moderator_name,
                        },
                    ),
                )
                .await
                .map_err(|error| {
                    warn!(%room_id, %error, "Failed to remove persisted ban");
                    anyhow::anyhow!("Ban state could not be updated")
                })?;
        }

        let mut room = room_lock.write().await;
        room.ensure_live()?;
        let in_memory = room
            .banned_participants
            .remove(target_participant_id)
            .is_some();

        if !in_memory && !persisted_removed {
            anyhow::bail!("Participant is not banned");
        }
        let banned_name = room.social.forget_user_ban(target_participant_id);
        if !room.persisted {
            room.social
                .record_moderation_event(moderation::ModerationEvent::new(
                    moderation::ModerationAction::Unban,
                    moderation::Actor {
                        id: moderator_id,
                        name: &moderator_name,
                    },
                    moderation::Target {
                        id: target_participant_id.into(),
                        name: banned_name.as_deref().unwrap_or("Removed account"),
                        authenticated: true,
                        ip: None,
                    },
                    None,
                    None,
                    None,
                ));
        }
        drop(room);
        control.release();

        info!(
            "unban: {} unbanned {} from room {}",
            moderator_id, target_participant_id, room_id
        );
        Ok(())
    }

    /// Set a participant's role
    pub(crate) async fn set_participant_role(
        &self,
        room_id: &RoomId,
        setter_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        new_role: roles::Role,
    ) -> Result<()> {
        let setter_id = *setter_id;
        let expected_sender = expected_sender.clone();
        let target_participant_id = *target_participant_id;
        self.run_room_control(room_id, move |manager, room_id, control| async move {
            manager
                .set_participant_role_controlled(
                    &room_id,
                    &setter_id,
                    &expected_sender,
                    &target_participant_id,
                    new_role,
                    control,
                )
                .await
        })
        .await
    }

    pub(in crate::room) async fn set_participant_role_controlled(
        &self,
        room_id: &RoomId,
        setter_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
        target_participant_id: &ParticipantId,
        new_role: roles::Role,
        control: control::RoomControl,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let setter = Self::participant_for_sender(&room, setter_id, expected_sender)?;
        let target = room
            .participants
            .get(target_participant_id)
            .ok_or_else(|| anyhow::anyhow!("Target participant not found"))?;

        if !setter.role.can_set_role(target.role, new_role) {
            anyhow::bail!("Insufficient permissions to set this role");
        }
        if target.authenticated && new_role == roles::Role::Guest {
            anyhow::bail!("Registered participants must be assigned at least the user role");
        }
        if target.role == new_role {
            return Ok(());
        }

        let setter_name = setter.id.to_string();
        let setter_authenticated = setter.authenticated;
        let target_authenticated = target.authenticated;
        if !room.admin_mutations.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room administration changes are rate limited");
        }

        let persisted = room.persisted;
        drop(room);
        if persisted {
            if !setter_authenticated || !target_authenticated {
                anyhow::bail!("Persistent roles require registered participants");
            }
            let granted_by = setter_id.as_uuid();
            let target_user = target_participant_id.as_uuid();
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| anyhow::anyhow!("Role service is temporarily unavailable"))?;
            self.persist_room(
                room_id,
                &room_lock,
                roles::set_role(pool, room_id, &target_user, new_role, &granted_by),
            )
            .await
            .map_err(|error| {
                warn!(%room_id, %error, "Failed to persist participant role");
                anyhow::anyhow!("Role could not be saved")
            })?;
        }

        let mut room = room_lock.write().await;
        room.ensure_live()?;
        let target = room
            .participants
            .get_mut(target_participant_id)
            .expect("role target is stable under the room control guard");
        target.role = new_role;
        room.notify_lobby_status();
        room.policy_revision = room.policy_revision.wrapping_add(1);
        let revoked = Self::revoke_unauthorized_producers(
            &mut room,
            Some(target_participant_id),
            "Role no longer permits this producer",
        );
        let active_observer = room.active_speaker_observer.clone();
        let audio_observer = room.audio_level_observer.clone();

        room.broadcast_all(&ServerMessage::RoleChanged {
            participant_id: *target_participant_id,
            new_role,
            granted_by: setter_name,
        });
        drop(room);
        control.release();

        self.close_revoked_producers(room_id, revoked, active_observer, audio_observer)
            .await;

        info!(
            "set_role: {} set {} to {:?} in room {}",
            setter_id, target_participant_id, new_role, room_id
        );
        Ok(())
    }

    /// Request voice in a moderated room (broadcasts to Moderator+ participants)
    pub(crate) async fn request_voice(
        &self,
        room_id: &RoomId,
        participant_id: &ParticipantId,
        expected_sender: &mpsc::Sender<crate::OutboundJson>,
    ) -> Result<()> {
        let room_lock = self.get_room(room_id)?;
        let mut room = room_lock.write().await;

        let participant = Self::participant_for_sender(&room, participant_id, expected_sender)?;

        let display_name = participant.name.clone();

        if !room.voice_requests.reserve(std::time::Instant::now()) {
            anyhow::bail!("Room voice-request rate limit exceeded");
        }

        room.broadcast_to_role(
            roles::Role::Moderator,
            &ServerMessage::VoiceRequested {
                participant_id: *participant_id,
                display_name,
            },
        );

        info!(
            "request_voice: {} requested voice in room {}",
            participant_id, room_id
        );
        Ok(())
    }
}
