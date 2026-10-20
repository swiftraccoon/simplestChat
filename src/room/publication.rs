//! What a moderation decision changes in a live room, published once for the
//! socket and the HTTP path after their own authorization and persistence: the
//! cohort's state, the notices everyone receives, and the producers or media
//! sessions the caller closes after releasing the room lock.

use super::{
    RevokedProducer, Room, RoomManager, SanctionKey, apply_punitive_to_cohort,
    punitive_notification, record_banned_cohort, try_send_essential,
};
use crate::ids::ParticipantId;
use crate::room::{moderation, roles};
use crate::signaling::protocol::ServerMessage;
use std::net::IpAddr;
use std::time::{Duration, Instant};
use uuid::Uuid;

/// A mute or camera ban, or its lifting, resolved to the live identities it covers.
pub(in crate::room) struct SanctionPublication {
    pub(in crate::room) target: ParticipantId,
    pub(in crate::room) participants: Vec<ParticipantId>,
    pub(in crate::room) lobby: Vec<ParticipantId>,
    pub(in crate::room) authenticated: bool,
    pub(in crate::room) ip: Option<IpAddr>,
    pub(in crate::room) kind: moderation::PunitiveKind,
    pub(in crate::room) enabled: bool,
}

/// A kick or ban resolved to the live identities it removes.
pub(in crate::room) struct RemovalPublication {
    pub(in crate::room) ban: bool,
    pub(in crate::room) participants: Vec<ParticipantId>,
    pub(in crate::room) lobby: Vec<ParticipantId>,
    pub(in crate::room) authenticated: bool,
    pub(in crate::room) ip: Option<IpAddr>,
    pub(in crate::room) name: String,
    pub(in crate::room) reason: Option<String>,
    pub(in crate::room) duration_seconds: Option<u64>,
}

/// A removed participant's media session, to close after the room lock is released.
pub(in crate::room) struct RemovedSession {
    pub(in crate::room) participant_id: ParticipantId,
    pub(in crate::room) media_session_id: Uuid,
}

impl Room {
    /// Apply a sanction to its cohort and tell everyone. Returns the producers a
    /// camera ban revoked; the caller closes them after releasing the room lock.
    pub(in crate::room) fn publish_sanction(
        &mut self,
        publication: &SanctionPublication,
    ) -> Vec<RevokedProducer> {
        let key = SanctionKey::for_identity(
            &publication.target,
            publication.authenticated,
            publication.ip,
        );
        apply_punitive_to_cohort(
            self,
            &publication.participants,
            &publication.lobby,
            key,
            publication.kind,
            publication.enabled,
        );
        for id in &publication.participants {
            self.broadcast_all(&punitive_notification(
                publication.kind,
                publication.enabled,
                *id,
            ));
        }
        for id in &publication.lobby {
            if let (Some(entry), Ok(json)) = (
                self.lobby.get(id),
                serde_json::to_string(&punitive_notification(
                    publication.kind,
                    publication.enabled,
                    *id,
                )),
            ) {
                let _ = try_send_essential(
                    &self.metrics,
                    &entry.sender,
                    crate::OutboundJson::from(json),
                );
            }
        }
        let mut revoked = Vec::new();
        if publication.kind == moderation::PunitiveKind::CamBanned && publication.enabled {
            for id in &publication.participants {
                revoked.extend(RoomManager::revoke_unauthorized_producers(
                    self,
                    Some(id),
                    "Camera access revoked by moderator",
                ));
            }
        }
        self.policy_revision = self.policy_revision.wrapping_add(1);
        self.notify_lobby_status();
        revoked
    }

    /// Remove a kicked or banned cohort, remember a ban, and tell everyone.
    /// Returns the media sessions to clean up after releasing the room lock.
    pub(in crate::room) fn publish_removal(
        &mut self,
        publication: RemovalPublication,
    ) -> Vec<RemovedSession> {
        if publication.ban {
            let ids: Vec<ParticipantId> = publication
                .participants
                .iter()
                .chain(publication.lobby.iter())
                .copied()
                .collect();
            let runtime_expiry = publication
                .duration_seconds
                .and_then(|seconds| Instant::now().checked_add(Duration::from_secs(seconds)));
            record_banned_cohort(
                self,
                &ids,
                publication.authenticated,
                publication.ip,
                runtime_expiry,
            );
            self.social.record_ban(
                publication.name.clone(),
                publication.authenticated,
                ids,
                publication.ip,
                publication.reason.as_deref(),
                publication.duration_seconds,
            );
        }
        let mut sessions = Vec::new();
        for id in &publication.participants {
            let notice = if publication.ban {
                ServerMessage::ParticipantBanned {
                    participant_id: *id,
                    reason: publication.reason.clone(),
                }
            } else {
                ServerMessage::ParticipantKicked {
                    participant_id: *id,
                    reason: publication.reason.clone(),
                }
            };
            self.broadcast_all(&notice);
            if let Some(person) = self.participants.remove(id) {
                for producer in person.producers.keys() {
                    self.producer_to_participant.remove(producer);
                }
                sessions.push(RemovedSession {
                    participant_id: *id,
                    media_session_id: person.media_session_id,
                });
                self.broadcast_participant_left(id);
            }
        }
        if publication.ban
            && let Ok(notice) = serde_json::to_string(&ServerMessage::LobbyDenied {
                reason: Some(
                    publication
                        .reason
                        .clone()
                        .unwrap_or_else(|| "Banned from room".into()),
                ),
            })
        {
            let notice = crate::OutboundJson::from(notice);
            for id in &publication.lobby {
                if let Some(person) = self.lobby.remove(id) {
                    let _ = try_send_essential(&self.metrics, &person.sender, notice.clone());
                }
            }
        }
        self.policy_revision = self.policy_revision.wrapping_add(1);
        self.notify_lobby_status();
        sessions
    }

    /// Change a member's role wherever the room holds it and tell everyone.
    /// Returns the producers the new role no longer permits.
    pub(in crate::room) fn publish_role(
        &mut self,
        target: &ParticipantId,
        role: roles::Role,
        granted_by: String,
    ) -> Vec<RevokedProducer> {
        if let Some(person) = self.participants.get_mut(target) {
            person.role = role;
        }
        if let Some(entry) = self.lobby.get_mut(target) {
            entry.role = role;
        }
        self.notify_lobby_status();
        self.policy_revision = self.policy_revision.wrapping_add(1);
        let revoked = RoomManager::revoke_unauthorized_producers(
            self,
            Some(target),
            "Role no longer permits this producer",
        );
        self.broadcast_all(&ServerMessage::RoleChanged {
            participant_id: *target,
            new_role: role,
            granted_by,
        });
        revoked
    }

    /// Forget a lifted ban: the account or guest cohort it named and, for a
    /// runtime ban already taken out of the social record, the identities it held.
    pub(in crate::room) fn publish_unban(
        &mut self,
        user: Option<ParticipantId>,
        guest_ip: Option<IpAddr>,
        released: Vec<ParticipantId>,
    ) {
        let mut affected = released;
        affected.extend(self.social.forget_ban(user.as_ref(), guest_ip));
        if let Some(user) = user {
            self.banned_participants.remove(&user);
        }
        if let Some(ip) = guest_ip {
            self.banned_guest_ips.remove(&ip);
        }
        for id in affected {
            self.banned_participants.remove(&id);
            self.banned_guest_participants.remove(&id);
        }
        self.policy_revision = self.policy_revision.wrapping_add(1);
    }
}
