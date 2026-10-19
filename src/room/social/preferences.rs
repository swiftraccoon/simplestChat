//! A participant's own social settings: private-message and ignore
//! preferences, the room nickname and the chat look.

use super::*;

impl RoomManager {
    pub(super) async fn set_chat_preferences(
        &self,
        request: &mut SocialRequest<'_>,
        allow_private_messages: bool,
        ignored_participant_ids: &[ParticipantId],
    ) -> Result<SocialResponse> {
        let participant_id = request.participant_id;
        let actor_authenticated = request.actor_authenticated;
        if ignored_participant_ids.len() > MAX_IGNORED
            || ignored_participant_ids
                .iter()
                .any(|id| id == participant_id)
        {
            return Err(invalid(
                "Choose at most 100 valid participant identities to ignore",
            ));
        }
        if actor_authenticated {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Account preferences unavailable"))?;
            let account = participant_id.as_uuid();
            request.release();
            let preferences = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                crate::auth::account::load_chat_preferences(pool, account),
            )
            .await
            .map_err(|_| timed_out("Account preferences timed out"))??;
            request.reacquire().await?;
            let room = request.room();
            let actor = room.participants.get(participant_id).unwrap();
            let guests = ignored_participant_ids
                .iter()
                .filter(|id| {
                    room.participants
                        .get(*id)
                        .is_some_and(|person| !person.authenticated)
                        || actor.social.guest_ignored.contains(*id)
                })
                .cloned()
                .collect();
            let actor = room.participants.get_mut(participant_id).unwrap();
            actor.social.guest_ignored = guests;
            actor.social.apply_account_preferences(&preferences);
        } else {
            let actor = request.room().participants.get_mut(participant_id).unwrap();
            actor.social.allow_private_messages = allow_private_messages;
            actor.social.ignored = ignored_participant_ids.iter().cloned().collect();
        }
        let actor = request.room().participants.get(participant_id).unwrap();
        Ok(SocialResponse::SetChatPreferences {
            allow_private_messages: actor.social.allow_private_messages,
            ignored_participant_ids: actor.social.ignored.iter().cloned().collect(),
        })
    }

    pub(super) fn change_nickname(
        &self,
        request: &mut SocialRequest<'_>,
        nickname: &str,
    ) -> Result<SocialResponse> {
        let participant_id = request.participant_id;
        let nickname = nickname.trim();
        if nickname.is_empty()
            || nickname.len() > 64
            || !crate::labels::is_plain(nickname)
            || crate::labels::is_reserved_name(nickname)
        {
            return Err(invalid(
                "Nickname must be 1–64 bytes of plain text; \"You\" is taken",
            ));
        }
        let room = request.room();
        if room.name_in_use(nickname, Some(participant_id)) {
            return Err(invalid("That name is already in use in this room"));
        }
        room.participants.get_mut(participant_id).unwrap().name = nickname.to_string();
        room.broadcast_all(&ServerMessage::NicknameChanged {
            participant_id: *participant_id,
            nickname: nickname.to_string(),
        });
        Ok(SocialResponse::ChangeNickname {
            nickname: nickname.to_string(),
        })
    }

    pub(super) async fn set_chat_style(
        &self,
        request: &mut SocialRequest<'_>,
        chat_style: &ChatStyle,
    ) -> Result<SocialResponse> {
        let participant_id = request.participant_id;
        let chat_style = chat_style
            .validated()
            .ok_or_else(|| invalid("Choose a chat color from the palette"))?;
        if request.actor_authenticated
            && let Some(pool) = self.db_pool.clone()
        {
            let user: Uuid = participant_id.as_uuid();
            // The account keeps the look before anyone sees it.
            request.release();
            tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                save_chat_style(&pool, user, &chat_style),
            )
            .await
            .map_err(|_| failed("Your chat look could not be saved"))?
            .map_err(|_| failed("Your chat look could not be saved"))?;
            request.reacquire().await?;
        }
        let room = request.room();
        room.participants
            .get_mut(participant_id)
            .ok_or_else(|| not_found("Participant not found"))?
            .chat_style = chat_style.clone();
        room.broadcast_all(&ServerMessage::ChatStyleChanged {
            participant_id: *participant_id,
            chat_style: chat_style.clone(),
        });
        Ok(SocialResponse::SetChatStyle {
            chat_style: chat_style.clone(),
        })
    }
}
