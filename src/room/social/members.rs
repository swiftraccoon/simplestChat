//! Membership administration over the socket: ban listing and removal, the
//! member roster and offline role changes.

use super::*;

impl RoomManager {
    pub(super) async fn list_room_bans<'a>(
        &self,
        request: &SocialRequest<'a>,
        mut room: HeldRoom<'a>,
        offset: Option<u32>,
    ) -> Result<SocialResponse, RoomError> {
        require_role(request.actor_role, roles::Role::Admin)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let mut bans: Vec<BanEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Ban service unavailable".into()))?;
            let released = release(room);
            let bans = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                moderation::list_bans(pool, room_id, offset as i64, (PAGE_SIZE + 1) as i64),
            )
            .await
            .map_err(|_| Refusal::TimedOut("Ban service timed out".into()))??;
            drop(request.reacquire(released).await?);
            bans
        } else {
            room.social
                .bans
                .retain(|_, b| b.expiry.is_none_or(|e| e > std::time::Instant::now()));
            let mut bans: Vec<_> = room.social.bans.values().map(|b| b.entry.clone()).collect();
            bans.sort_by(|a, b| a.ban_id.cmp(&b.ban_id));
            bans.into_iter().skip(offset).take(PAGE_SIZE + 1).collect()
        };
        let has_more = bans.len() > PAGE_SIZE;
        bans.truncate(PAGE_SIZE);
        Ok(SocialResponse::ListRoomBans { bans, has_more })
    }

    pub(super) async fn remove_room_ban<'a>(
        &self,
        request: &SocialRequest<'a>,
        mut room: HeldRoom<'a>,
        ban_id: &str,
    ) -> Result<SocialResponse, RoomError> {
        require_role(request.actor_role, roles::Role::Admin)?;
        let id: Uuid = ban_id
            .parse()
            .map_err(|_| Refusal::Invalid("Invalid ban".into()))?;
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        let (user_id, guest_ip, ids) = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Ban service unavailable".into()))?;
            let released = release(room);
            let actor_id = *participant_id;
            let removed = request
                .persist(self, &released, async {
                    let mut transaction = pool.begin().await?;
                    let removed = moderation::remove_ban(
                        &mut transaction,
                        room_id,
                        id,
                        Actor {
                            id: &actor_id,
                            name: &actor_name,
                        },
                    )
                    .await?;
                    transaction.commit().await?;
                    Ok(removed)
                })
                .await?;
            room = request.reacquire(released).await?;
            let removed = removed.ok_or_else(|| Refusal::NotFound("Ban not found".into()))?;
            (
                removed.user_id.map(ParticipantId::from),
                removed.user_id.is_none().then_some(removed.ip).flatten(),
                Vec::new(),
            )
        } else {
            let ban = room
                .social
                .bans
                .remove(ban_id)
                .ok_or_else(|| Refusal::NotFound("Ban not found".into()))?;
            let target_id = ban
                .entry
                .authenticated
                .then(|| ban.participant_ids.first().cloned())
                .flatten();
            room.social.record_moderation_event(ModerationEvent::new(
                ModerationAction::Unban,
                Actor {
                    id: participant_id,
                    name: &actor_name,
                },
                Target {
                    id: target_id.map_or_else(
                        || ModerationTargetId::Ban(ban_id.to_string()),
                        ModerationTargetId::from,
                    ),
                    name: &ban.entry.display_name,
                    authenticated: ban.entry.authenticated,
                    ip: ban.guest_ip,
                },
                None,
                None,
                None,
            ));
            (target_id, ban.guest_ip, ban.participant_ids)
        };
        room.publish_unban(user_id, guest_ip, ids);
        Ok(SocialResponse::RemoveRoomBan { removed: true })
    }

    pub(super) async fn list_room_members<'a>(
        &self,
        request: &SocialRequest<'a>,
        room: HeldRoom<'a>,
        offset: Option<u32>,
    ) -> Result<SocialResponse, RoomError> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let mut members: Vec<MemberEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| Refusal::Unavailable("Member service unavailable".into()))?;
            let released = release(room);
            let rows: Vec<(Uuid, String, roles::Role)> = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                sqlx::query_as("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,rr.role FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id) SELECT u.id,u.display_name,roster.role FROM roster JOIN users u ON u.id=roster.user_id ORDER BY u.display_name,u.id LIMIT 101 OFFSET $2")
                    .bind(room_id).bind(offset as i64).fetch_all(pool),
            ).await.map_err(|_| Refusal::TimedOut("Member service timed out".into()))??;
            let room = request.reacquire(released).await?;
            rows.into_iter()
                .map(|(id, name, role)| MemberEntry {
                    online: room.participants.contains_key(&ParticipantId::from(id)),
                    user_id: UserId::from(id),
                    display_name: name,
                    role,
                    authenticated: true,
                })
                .collect()
        } else {
            let mut members: Vec<_> = room
                .participants
                .values()
                .map(|p| MemberEntry {
                    user_id: UserId::from(p.id.as_uuid()),
                    display_name: p.name.clone(),
                    role: p.role,
                    online: true,
                    authenticated: p.authenticated,
                })
                .collect();
            members.sort_by(|a, b| {
                a.display_name
                    .cmp(&b.display_name)
                    .then(a.user_id.cmp(&b.user_id))
            });
            members
                .into_iter()
                .skip(offset)
                .take(PAGE_SIZE + 1)
                .collect()
        };
        let has_more = members.len() > PAGE_SIZE;
        members.truncate(PAGE_SIZE);
        Ok(SocialResponse::ListRoomMembers { members, has_more })
    }

    /// The offline role path; an online target is handled through room control
    /// before the request takes the room.
    pub(super) async fn set_member_role<'a>(
        &self,
        request: &SocialRequest<'a>,
        room: HeldRoom<'a>,
        target_user_id: &UserId,
        role: roles::Role,
    ) -> Result<SocialResponse, RoomError> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_role = request.actor_role;
        let actor_authenticated = request.actor_authenticated;
        if !room.persisted || !actor_authenticated {
            return Err(
                Refusal::Forbidden("Offline roles require a persistent room".into()).into(),
            );
        }
        let target: Uuid = target_user_id.as_uuid();
        let setter: Uuid = participant_id.as_uuid();
        let new_role = role;
        if new_role < roles::Role::User {
            return Err(Refusal::Forbidden(
                "Registered accounts require at least the user role".into(),
            )
            .into());
        }
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| Refusal::Unavailable("Member service unavailable".into()))?;
        let released = release(room);
        // Absent targets must already belong to this room's roster; no account enumeration.
        let current = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            roles::current_role(pool, room_id, target),
        )
        .await
        .map_err(|_| Refusal::TimedOut("Member service timed out".into()))??;
        let current = current.ok_or_else(|| Refusal::NotFound("Room member not found".into()))?;
        if !actor_role.can_set_role(current, new_role) {
            return Err(Refusal::Forbidden("Insufficient role permissions".into()).into());
        }
        request
            .persist(
                self,
                &released,
                roles::set_role(pool, room_id, &target, new_role, &setter),
            )
            .await?;
        let mut room = request.reacquire(released).await?;
        room.policy_revision = room.policy_revision.wrapping_add(1);
        Ok(SocialResponse::SetMemberRole { updated: true })
    }
}
