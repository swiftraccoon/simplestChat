//! The room-control commands of one connection: moderation, roles, voice
//! requests, settings, the topic and the waiting room. The dispatcher
//! acknowledges each of these after the command applied.

use super::*;

/// Runs one room-control command for the connection that owns `connection`; the
/// dispatcher has already checked the lobby and membership preconditions.
pub(super) async fn handle(
    message: &ClientMessage,
    connection: Dispatch<'_, '_>,
) -> anyhow::Result<()> {
    let Dispatch {
        participant_id,
        current_room_id,
        reply,
        room_manager,
        ..
    } = connection;
    let sender = reply.sender;
    match message {
        ClientMessage::CloseCam {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .close_cam(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::CamBan {
            target_participant_id,
            reason,
        } => {
            let room_id = joined(current_room_id)?;
            validate_durable_reason(reason.as_deref())?;
            room_manager
                .cam_ban(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                )
                .await?;
        }

        ClientMessage::CamUnban {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .cam_unban(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::TextMute {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .text_mute(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::TextUnmute {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .text_unmute(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::Kick {
            target_participant_id,
            reason,
            report_id,
        } => {
            let room_id = joined(current_room_id)?;
            validate_reason(reason.as_deref())?;
            let report_id = parse_report_id(report_id.as_deref())?;
            room_manager
                .kick_participant(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                    report_id,
                )
                .await?;
        }

        ClientMessage::Ban {
            target_participant_id,
            reason,
            duration,
            report_id,
        } => {
            let room_id = joined(current_room_id)?;
            validate_durable_reason(reason.as_deref())?;
            let report_id = parse_report_id(report_id.as_deref())?;
            room_manager
                .ban_participant(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    reason.as_deref(),
                    *duration,
                    report_id,
                )
                .await?;
        }

        ClientMessage::Unban { target_user_id } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .unban_participant(room_id, participant_id, sender, target_user_id)
                .await?;
        }

        ClientMessage::SetRole {
            target_participant_id,
            role,
        } => {
            let room_id = joined(current_room_id)?;
            let new_role = *role;
            room_manager
                .set_participant_role(
                    room_id,
                    participant_id,
                    sender,
                    target_participant_id,
                    new_role,
                )
                .await?;
        }

        ClientMessage::RequestVoice => {
            let room_id = joined(current_room_id)?;
            room_manager
                .request_voice(room_id, participant_id, sender)
                .await?;
        }

        // === Room management ===
        ClientMessage::UpdateRoomSettings { patch } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .update_room_settings(room_id, participant_id, sender, patch.clone())
                .await?;
        }

        ClientMessage::SetTopic { topic } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .set_topic(room_id, participant_id, sender, topic.clone())
                .await?;
        }

        // === Lobby ===
        ClientMessage::AdmitFromLobby {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .admit_from_lobby(room_id, participant_id, sender, target_participant_id)
                .await?;
        }

        ClientMessage::DenyFromLobby {
            target_participant_id,
        } => {
            let room_id = joined(current_room_id)?;
            room_manager
                .deny_from_lobby(room_id, participant_id, sender, target_participant_id, None)
                .await?;
        }
        _ => anyhow::bail!("Not a room-control command"),
    }
    Ok(())
}
