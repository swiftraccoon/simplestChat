//! Chat history over the socket: pages and read markers, the room's retention
//! choice, edits, pins, removals and reactions, each committed to the saved
//! history before the room sees it.

use super::*;
use crate::room::refusal::RoomError;

impl RoomManager {
    pub(super) async fn chat_history(
        &self,
        request: &mut SocialRequest<'_>,
        query: history::HistoryQuery,
    ) -> Result<SocialResponse> {
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_authenticated = request.actor_authenticated;
        let room = request.room();
        let days = room
            .settings
            .as_ref()
            .map_or(0, |settings| settings.history_retention_days);
        query.validate().map_err(invalid)?;
        if days == 0 || !room.persisted {
            return Ok(SocialResponse::GetChatHistory(history::HistoryPage {
                messages: Vec::new(),
                next_cursor: None,
                newer_cursor: None,
                first_unread_message_id: None,
                read_message_id: None,
                retention_days: 0,
            }));
        }
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| unavailable("History unavailable"))?;
        let account = actor_authenticated.then(|| participant_id.as_uuid());
        request.release();
        let mut page = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            history::page(
                pool,
                &history::public_conversation(room_id.as_str()),
                account,
                days,
                &query,
            ),
        )
        .await
        .map_err(|_| timed_out("History request timed out"))??;
        request.reacquire().await?;
        let ignored = &request
            .room()
            .participants
            .get(participant_id)
            .unwrap()
            .social
            .ignored;
        page.messages
            .retain(|message| !ignored.contains(&message.participant_id));
        Ok(SocialResponse::GetChatHistory(page))
    }

    pub(super) async fn mark_chat_read(
        &self,
        request: &mut SocialRequest<'_>,
        message_id: &MessageId,
    ) -> Result<SocialResponse> {
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_authenticated = request.actor_authenticated;
        let room = request.room();
        if !actor_authenticated
            || !room.persisted
            || !room
                .settings
                .as_ref()
                .is_some_and(|settings| settings.history_retention_days > 0)
        {
            return Err(unavailable("Saved room history is unavailable"));
        }
        let account = participant_id.as_uuid();
        let message = message_id.as_uuid();
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| unavailable("History unavailable"))?;
        request.release();
        let read = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            history::mark_read(
                pool,
                &history::public_conversation(room_id.as_str()),
                account,
                message,
            ),
        )
        .await
        .map_err(|_| failed("Read marker could not be saved"))?
        .map_err(|error| {
            if matches!(error, RoomError::Database(sqlx::Error::RowNotFound)) {
                unavailable("Message unavailable")
            } else {
                error.into()
            }
        })?;
        request.reacquire().await?;
        Ok(SocialResponse::MarkChatRead {
            read_message_id: Some(read.read_message_id),
        })
    }

    pub(super) async fn set_room_history(
        &self,
        request: &mut SocialRequest<'_>,
        retention_days: i32,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Owner)?;
        let room_id = request.room_id;
        if !request.room().persisted {
            return Err(forbidden("Only saved rooms can keep chat history"));
        }
        if !history::ROOM_HISTORY_RETENTION_DAYS.contains(&retention_days) {
            return Err(invalid("Choose off, 1, 7, 30 or 90 days"));
        }
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| unavailable("History unavailable"))?;
        request.release();
        self.persist_room(
            room_id,
            request.room_lock,
            history::set_retention(pool, room_id.as_str(), retention_days),
        )
        .await?;
        request.reacquire().await?;
        let room = request.room();
        let settings = room
            .settings
            .as_mut()
            .ok_or_else(|| unavailable("Room settings unavailable"))?;
        settings.history_retention_days = retention_days;
        let settings = settings.clone();
        room.broadcast_all(&ServerMessage::RoomSettingsChanged { settings });
        if retention_days == 0 {
            room.social.pins.clear();
        }
        publish_pins(room)?;
        Ok(SocialResponse::SetRoomHistory { retention_days })
    }

    pub(super) async fn edit_chat_message(
        &self,
        request: &mut SocialRequest<'_>,
        message_id: &MessageId,
        edit: history::EditRequest,
    ) -> Result<SocialResponse> {
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let expected_sender = request.expected_sender;
        let actor_authenticated = request.actor_authenticated;
        let account: Uuid = participant_id.as_uuid();
        let room = request.room();
        let actor = Self::participant_for_sender(room, participant_id, expected_sender)?;
        let guest_session = (!actor.authenticated).then_some(actor.media_session_id);
        let mut edited = prepare_edit(room, actor, message_id, &edit)?;
        let keeps_history = room.persisted
            && room
                .settings
                .as_ref()
                .is_some_and(|s| s.history_retention_days > 0);
        if edited.is_none() && keeps_history {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("History unavailable"))?;
            request.release();
            let found = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                history::lookup_public(pool, room_id.as_str(), message_id),
            )
            .await
            .map_err(|_| timed_out("History request timed out"))??;
            edited = found.map(|(message, _)| message);
            request.reacquire().await?;
        }
        let mut edited = edited.ok_or_else(|| unavailable("Message unavailable"))?;
        if edited.participant_id != *participant_id {
            return Err(forbidden("Only the author can edit this message"));
        }
        let room = request.room();
        let durable = self.db_pool.is_some()
            && if let Some(peer) = &edited.recipient_id {
                actor_authenticated && room.participants.get(peer).is_some_and(|p| p.authenticated)
            } else {
                keeps_history
            };
        if durable {
            let pool = self.db_pool.as_ref().unwrap();
            request.release();
            edited = self
                .persist_room(
                    room_id,
                    request.room_lock,
                    history::edit_saved(
                        pool,
                        Some(room_id.as_str()),
                        account,
                        guest_session,
                        &edited,
                        &edit,
                    ),
                )
                .await
                .map_err(|error| match error {
                    RoomError::Refused(refused) => super::refused(refused),
                    RoomError::Database(error) => error.into(),
                })?;
            self.account_updates().private_message(&edited);
            request.reacquire().await?;
        } else {
            history::apply_edit(&mut edited, &edit).map_err(invalid)?;
        }
        publish_edit(request.room(), &edited)?;
        drop(self.schedule_inbox_edit(&edited));
        Ok(SocialResponse::EditChatMessage { message: edited })
    }

    /// Lists the pins, after applying `change` (a message to pin or unpin) when
    /// a moderator asked for one.
    pub(super) async fn pinned_messages(
        &self,
        request: &mut SocialRequest<'_>,
        change: Option<(&MessageId, bool)>,
    ) -> Result<SocialResponse> {
        if change.is_some() {
            require_role(request.actor_role, roles::Role::Moderator)?;
        }
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let expected_sender = request.expected_sender;
        let room = request.room();
        let durable = room.persisted
            && room
                .settings
                .as_ref()
                .is_some_and(|s| s.history_retention_days > 0);
        if durable {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("History unavailable"))?;
            request.release();
            if let Some((message, pinned)) = change {
                self.persist_room(
                    room_id,
                    request.room_lock,
                    history::set_pin(pool, room_id.as_str(), message, pinned),
                )
                .await
                .map_err(|error| match error {
                    RoomError::Refused(refused) => super::refused(refused),
                    RoomError::Database(error) => error.into(),
                })?;
            }
            let pins = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                history::pinned_messages(pool, room_id.as_str()),
            )
            .await
            .map_err(|_| timed_out("Pinned messages request timed out"))??;
            request.reacquire().await?;
            request.room().social.pins = pins;
        } else if let Some((message, pinned)) = change {
            if pinned && !room.social.pins.iter().any(|m| m.message_id == *message) {
                if room.social.pins.len() >= 3 {
                    return Err(forbidden(
                        "Unpin a message before pinning another; rooms keep up to three",
                    ));
                }
                let original = room
                    .social
                    .history
                    .iter()
                    .find(|entry| {
                        entry.message.message_id == *message
                            && entry.message.recipient_id.is_none()
                            && entry.message.removed_at.is_none()
                    })
                    .ok_or_else(|| unavailable("Public message unavailable"))?
                    .message
                    .clone();
                room.social.pins.insert(0, original);
            } else if !pinned {
                room.social.pins.retain(|m| m.message_id != *message);
            }
        }
        let room = request.room();
        if change.is_some() {
            publish_pins(room)?;
        }
        let actor = Self::participant_for_sender(room, participant_id, expected_sender)?;
        let messages: Vec<_> = room
            .social
            .pins
            .iter()
            .filter(|message| !actor.social.ignored.contains(&message.participant_id))
            .cloned()
            .collect();
        Ok(if change.is_some() {
            SocialResponse::SetPinnedMessage { messages }
        } else {
            SocialResponse::GetPinnedMessages { messages }
        })
    }

    pub(super) async fn remove_chat_message(
        &self,
        request: &mut SocialRequest<'_>,
        message_id: &MessageId,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        let room = request.room();
        let mut original = room
            .social
            .history
            .iter()
            .find(|entry| {
                entry.message.message_id == *message_id && entry.message.recipient_id.is_none()
            })
            .map(|entry| (entry.message.clone(), entry.sender_authenticated));
        let persisted = room.persisted;
        let keeps_history = room
            .settings
            .as_ref()
            .is_some_and(|s| s.history_retention_days > 0);
        let pool = self.db_pool.clone();
        if original.is_none() && persisted && keeps_history {
            let pool = pool
                .as_ref()
                .ok_or_else(|| unavailable("Chat history unavailable"))?;
            request.release();
            original = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                super::history::lookup_public(pool, room_id.as_str(), message_id),
            )
            .await
            .map_err(|_| timed_out("Chat history timed out"))??;
            request.reacquire().await?;
        }
        let (original, authenticated) = original.ok_or_else(|| not_found("Message not found"))?;
        let removed_at = original.removed_at.unwrap_or_else(chrono::Utc::now);
        if original.removed_at.is_none() {
            let event = ModerationEvent::new(
                ModerationAction::MessageRemoved,
                Actor {
                    id: participant_id,
                    name: &actor_name,
                },
                Target {
                    id: original.participant_id.into(),
                    name: &original.participant_name,
                    authenticated,
                    ip: None,
                },
                Some(&format!("Message {message_id}")),
                None,
                None,
            );
            if persisted {
                let pool = pool
                    .as_ref()
                    .ok_or_else(|| unavailable("Moderation unavailable"))?;
                request.release();
                self.persist_room(room_id, request.room_lock, async {
                    let mut transaction = pool.begin().await?;
                    if keeps_history {
                        super::history::remove_public(
                            &mut transaction,
                            room_id.as_str(),
                            message_id,
                            removed_at,
                        )
                        .await?;
                    }
                    sqlx::query(
                        "DELETE FROM attachments WHERE room_id=$1 AND ephemeral_message_id=$2",
                    )
                    .bind(room_id)
                    .bind(message_id.as_uuid())
                    .execute(&mut *transaction)
                    .await?;
                    record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                    Ok(transaction.commit().await?)
                })
                .await?;
                request.reacquire().await?;
            } else {
                if let Some(pool) = &pool {
                    request.release();
                    self.persist_room(room_id, request.room_lock, async {
                        sqlx::query(
                            "DELETE FROM attachments WHERE room_id=$1 AND ephemeral_message_id=$2",
                        )
                        .bind(room_id)
                        .bind(message_id.as_uuid())
                        .execute(pool)
                        .await?;
                        Ok(())
                    })
                    .await?;
                    request.reacquire().await?;
                }
                request.room().social.record_moderation_event(event);
            }
        }
        let room = request.room();
        room.social.remove_message(message_id, removed_at);
        room.broadcast_all(&ServerMessage::ChatMessageRemoved {
            message_id: *message_id,
            removed_at,
        });
        publish_pins(room)?;
        Ok(SocialResponse::RemoveChatMessage {
            message_id: *message_id,
            removed_at,
        })
    }

    pub(super) async fn react_to_message(
        &self,
        request: &mut SocialRequest<'_>,
        message_id: &MessageId,
        emoji: &str,
    ) -> Result<SocialResponse> {
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let room = request.room();
        let saved_public = room.persisted
            && room
                .settings
                .as_ref()
                .is_some_and(|settings| settings.history_retention_days > 0)
            && room
                .social
                .history
                .iter()
                .find(|entry| entry.message.message_id == *message_id)
                .is_none_or(|entry| entry.message.recipient_id.is_none());
        if saved_public {
            let ignored: Vec<_> = room
                .participants
                .get(participant_id)
                .unwrap()
                .social
                .ignored
                .iter()
                .cloned()
                .collect();
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("History unavailable"))?;
            request.release();
            let saved = self
                .persist_room(
                    room_id,
                    request.room_lock,
                    history::react_public(
                        pool,
                        room_id.as_str(),
                        message_id,
                        participant_id,
                        &ignored,
                        emoji,
                    ),
                )
                .await
                .map_err(|error| match error {
                    RoomError::Refused(refused) => super::refused(refused),
                    RoomError::Database(error) => error.into(),
                })?;
            request.reacquire().await?;
            if let Some(message) = saved {
                publish_saved_reaction(request.room(), &message)?;
                return Ok(SocialResponse::ReactToMessage {
                    message_id: *message_id,
                    reactions: message.reactions.clone(),
                });
            }
        }
        let room = request.room();
        let reactions = prepare_reaction(room, participant_id, message_id, emoji)?;
        let private_message = room
            .social
            .history
            .iter()
            .find(|entry| entry.message.message_id == *message_id)
            .map(|entry| entry.message.clone());
        if !saved_public && let Some(pool) = &self.db_pool {
            request.release();
            self.persist_room(
                room_id,
                request.room_lock,
                history::save_reactions(pool, message_id, &reactions),
            )
            .await?;
            if let Some(message) = &private_message {
                self.account_updates().private_message(message);
            }
            request.reacquire().await?;
        }
        let reactions = publish_reaction(request.room(), message_id, reactions)?;
        Ok(SocialResponse::ReactToMessage {
            message_id: *message_id,
            reactions,
        })
    }
}
