//! A participant's own social settings: private-message and ignore
//! preferences, the room nickname and the chat look.

use super::*;

impl RoomManager {
    pub(super) async fn set_chat_preferences<'a>(
        &self,
        request: &SocialRequest<'a>,
        mut room: HeldRoom<'a>,
        allow_private_messages: bool,
        ignored_participant_ids: &[ParticipantId],
    ) -> Result<SocialResponse, RoomError> {
        let participant_id = request.participant_id;
        let actor_authenticated = request.actor_authenticated;
        if ignored_participant_ids.len() > MAX_IGNORED
            || ignored_participant_ids
                .iter()
                .any(|id| id == participant_id)
        {
            return Err(Refusal::Invalid(
                "Choose at most 100 valid participant identities to ignore".into(),
            )
            .into());
        }
        if actor_authenticated {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Account preferences unavailable".into()))?;
            let account = participant_id.as_uuid();
            let released = release(room);
            let preferences = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                crate::auth::account::load_chat_preferences(pool, account),
            )
            .await
            .map_err(|_| Refusal::TimedOut("Account preferences timed out".into()))??;
            room = request.reacquire(released).await?;
            let actor = request.actor(&room)?;
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
            let actor = request.actor_mut(&mut room)?;
            actor.social.guest_ignored = guests;
            actor.social.apply_account_preferences(&preferences);
        } else {
            let actor = request.actor_mut(&mut room)?;
            actor.social.allow_private_messages = allow_private_messages;
            actor.social.ignored = ignored_participant_ids.iter().cloned().collect();
        }
        let actor = request.actor(&room)?;
        Ok(SocialResponse::SetChatPreferences {
            allow_private_messages: actor.social.allow_private_messages,
            ignored_participant_ids: actor.social.ignored.iter().cloned().collect(),
        })
    }

    pub(super) fn change_nickname(
        &self,
        request: &SocialRequest<'_>,
        mut room: HeldRoom<'_>,
        nickname: &str,
    ) -> Result<SocialResponse, RoomError> {
        let participant_id = request.participant_id;
        let nickname = nickname.trim();
        if nickname.is_empty()
            || nickname.len() > 64
            || !crate::labels::is_plain(nickname)
            || crate::labels::is_reserved_name(nickname)
        {
            return Err(Refusal::Invalid(
                "Nickname must be 1–64 bytes of plain text; \"You\" is taken".into(),
            )
            .into());
        }
        if room.name_in_use(nickname, Some(participant_id)) {
            return Err(Refusal::Invalid("That name is already in use in this room".into()).into());
        }
        request.actor_mut(&mut room)?.name = nickname.to_string();
        room.broadcast_all(&ServerMessage::NicknameChanged {
            participant_id: *participant_id,
            nickname: nickname.to_string(),
        });
        Ok(SocialResponse::ChangeNickname {
            nickname: nickname.to_string(),
        })
    }

    pub(super) async fn set_chat_style<'a>(
        &self,
        request: &SocialRequest<'a>,
        mut room: HeldRoom<'a>,
        chat_style: &ChatStyle,
    ) -> Result<SocialResponse, RoomError> {
        let participant_id = request.participant_id;
        let chat_style = chat_style
            .validated()
            .ok_or_else(|| Refusal::Invalid("Choose a chat color from the palette".into()))?;
        if request.actor_authenticated
            && let Some(pool) = self.db_pool.clone()
        {
            let user: Uuid = participant_id.as_uuid();
            // The account keeps the look before anyone sees it.
            let released = release(room);
            tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                save_chat_style(&pool, user, &chat_style),
            )
            .await
            .map_err(|_| {
                warn!(room = %request.room_id.log_handle(), "Chat look save timed out");
                Refusal::TimedOut("Your chat look may not have been saved".into())
            })?
            .map_err(|error| control::Save::ChatLook.failure(request.room_id, error.into()))?;
            room = request.reacquire(released).await?;
        }
        request.actor_mut(&mut room)?.chat_style = chat_style.clone();
        room.broadcast_all(&ServerMessage::ChatStyleChanged {
            participant_id: *participant_id,
            chat_style: chat_style.clone(),
        });
        Ok(SocialResponse::SetChatStyle {
            chat_style: chat_style.clone(),
        })
    }
}
