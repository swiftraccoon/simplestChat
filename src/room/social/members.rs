//! Membership administration over the socket: ban listing and removal, the
//! member roster and offline role changes.

use super::*;

impl RoomManager {
    pub(super) async fn list_room_bans(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Admin)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let room = request.room();
        let mut bans: Vec<BanEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Ban service unavailable"))?;
            request.release();
            let rows: Vec<BanRow> = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                sqlx::query_as(
                    "SELECT s.id,u.display_name,s.user_id IS NOT NULL AS authenticated,s.reason,s.expires_at FROM room_states s LEFT JOIN users u ON u.id=s.user_id WHERE s.room_id=$1 AND s.state='banned' AND (s.expires_at IS NULL OR s.expires_at>now()) ORDER BY s.created_at DESC,s.id LIMIT 101 OFFSET $2")
                    .bind(room_id).bind(offset as i64).fetch_all(pool),
            ).await.map_err(|_| timed_out("Ban service timed out"))??;
            request.reacquire().await?;
            rows.into_iter()
                .map(|row| BanEntry {
                    ban_id: row.id.to_string(),
                    display_name: row.display_name.unwrap_or_else(|| "Guest".to_string()),
                    authenticated: row.authenticated,
                    reason: row.reason,
                    expires_at: row.expires_at.map(|e| e.to_rfc3339()),
                })
                .collect()
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

    pub(super) async fn remove_room_ban(
        &self,
        request: &mut SocialRequest<'_>,
        ban_id: &str,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Admin)?;
        let id: Uuid = ban_id.parse().map_err(|_| invalid("Invalid ban"))?;
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        let room = request.room();
        let (user_id, guest_ip, ids) = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Ban service unavailable"))?;
            request.release();
            let actor_id = *participant_id;
            let row: Option<(Option<Uuid>, Option<String>)> = self
                .persist_room(room_id, request.room_lock, async {
                    let mut transaction = pool.begin().await?;
                    let row: Option<(Option<Uuid>, Option<String>, Option<String>)> = sqlx::query_as("DELETE FROM room_states s WHERE s.room_id=$1 AND s.id=$2 AND s.state='banned' RETURNING s.user_id,host(s.ip_address),(SELECT u.display_name FROM users u WHERE u.id=s.user_id)")
                        .bind(room_id).bind(id).fetch_optional(&mut *transaction).await?;
                    let Some((uid, ip, name)) = row else {
                        return Ok(None);
                    };
                    let target_id = uid.map_or_else(
                        || ModerationTargetId::Ban(id.to_string()),
                        |uid| ModerationTargetId::from(ParticipantId::from(uid)),
                    );
                    let event = ModerationEvent::new(
                        ModerationAction::Unban,
                        Actor { id: &actor_id, name: &actor_name },
                        Target {
                            id: target_id.clone(),
                            name: name.as_deref().unwrap_or(if uid.is_some() { "Removed account" } else { "Guest" }),
                            authenticated: uid.is_some(),
                            ip: ip.as_deref().and_then(|ip| ip.parse().ok()),
                        },
                        None,
                        None,
                        None,
                    );
                    record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                    transaction.commit().await?;
                    Ok(Some((uid, ip)))
                })
                .await?;
            request.reacquire().await?;
            let (uid, ip) = row.ok_or_else(|| not_found("Ban not found"))?;
            (
                uid.map(ParticipantId::from),
                if uid.is_none() {
                    ip.and_then(|ip| ip.parse::<IpAddr>().ok())
                } else {
                    None
                },
                Vec::new(),
            )
        } else {
            let ban = room
                .social
                .bans
                .remove(ban_id)
                .ok_or_else(|| not_found("Ban not found"))?;
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
        let room = request.room();
        if let Some(uid) = &user_id {
            room.banned_participants.remove(uid);
        }
        if let Some(ip) = guest_ip {
            room.banned_guest_ips.remove(&ip);
        }
        let mut affected = ids;
        room.social.bans.retain(|_, ban| {
            let matches = user_id
                .as_ref()
                .is_some_and(|uid| ban.participant_ids.contains(uid))
                || guest_ip.is_some_and(|ip| ban.guest_ip == Some(ip));
            if matches {
                affected.extend(ban.participant_ids.iter().cloned());
            }
            !matches
        });
        for id in affected {
            room.banned_participants.remove(&id);
            room.banned_guest_participants.remove(&id);
        }
        room.policy_revision = room.policy_revision.wrapping_add(1);
        Ok(SocialResponse::RemoveRoomBan { removed: true })
    }

    pub(super) async fn list_room_members(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let room = request.room();
        let mut members: Vec<MemberEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Member service unavailable"))?;
            request.release();
            let rows: Vec<(Uuid, String, roles::Role)> = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                sqlx::query_as("WITH roster AS (SELECT owner_id AS user_id,5::smallint AS role FROM rooms WHERE id=$1 UNION ALL SELECT rr.user_id,rr.role FROM room_roles rr JOIN rooms r ON r.id=rr.room_id WHERE rr.room_id=$1 AND rr.user_id<>r.owner_id) SELECT u.id,u.display_name,roster.role FROM roster JOIN users u ON u.id=roster.user_id ORDER BY u.display_name,u.id LIMIT 101 OFFSET $2")
                    .bind(room_id).bind(offset as i64).fetch_all(pool),
            ).await.map_err(|_| timed_out("Member service timed out"))??;
            request.reacquire().await?;
            let room = request.room();
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
    pub(super) async fn set_member_role(
        &self,
        request: &mut SocialRequest<'_>,
        target_user_id: &UserId,
        role: roles::Role,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_role = request.actor_role;
        let actor_authenticated = request.actor_authenticated;
        if !request.room().persisted || !actor_authenticated {
            return Err(forbidden("Offline roles require a persistent room"));
        }
        let target: Uuid = target_user_id.as_uuid();
        let setter: Uuid = participant_id.as_uuid();
        let new_role = role;
        if new_role < roles::Role::User {
            return Err(forbidden(
                "Registered accounts require at least the user role",
            ));
        }
        let pool = self
            .db_pool
            .as_ref()
            .ok_or_else(|| unavailable("Member service unavailable"))?;
        request.release();
        // Absent targets must already belong to this room's roster; no account enumeration.
        let current: Option<roles::Role> = tokio::time::timeout(
            control::PERSISTENCE_TIMEOUT,
            sqlx::query_scalar("SELECT CASE WHEN $2=owner_id THEN 5 ELSE (SELECT role FROM room_roles WHERE room_id=$1 AND user_id=$2) END::smallint FROM rooms WHERE id=$1 AND ($2=owner_id OR EXISTS(SELECT 1 FROM room_roles WHERE room_id=$1 AND user_id=$2))")
                .bind(room_id).bind(target).fetch_optional(pool),
        ).await.map_err(|_| timed_out("Member service timed out"))??;
        let current = current.ok_or_else(|| not_found("Room member not found"))?;
        if !actor_role.can_set_role(current, new_role) {
            return Err(forbidden("Insufficient role permissions"));
        }
        self.persist_room(
            room_id,
            request.room_lock,
            roles::set_role(pool, room_id, &target, new_role, &setter),
        )
        .await?;
        request.reacquire().await?;
        let room = request.room();
        room.policy_revision = room.policy_revision.wrapping_add(1);
        Ok(SocialResponse::SetMemberRole { updated: true })
    }
}
