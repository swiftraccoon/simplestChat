//! Reports and the moderation log over the socket: submitting a report, the
//! moderators' report and event pages, and resolving a report.

use super::*;

impl RoomManager {
    pub(super) async fn report_participant(
        &self,
        request: &mut SocialRequest<'_>,
        target_participant_id: &ParticipantId,
        reason: &str,
    ) -> Result<SocialResponse> {
        let reason = reason.trim();
        if reason.is_empty() || reason.len() > 1024 || reason.chars().any(char::is_control) {
            return Err(invalid("Report reason must be 1–1024 bytes"));
        }
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        if target_participant_id == participant_id {
            return Err(invalid("Choose another participant"));
        }
        let room = request.room();
        let target = room
            .participants
            .get(target_participant_id)
            .ok_or_else(|| not_found("Participant is no longer in this room"))?;
        let target_name = target.name.clone();
        let reporter = reporter_key(room.participants.get(participant_id).unwrap());
        if room
            .social
            .report_cooldown_active(&reporter, std::time::Instant::now())
        {
            return Err(rate_limited("Please wait before submitting another report"));
        }
        let report_id = Uuid::new_v4();
        let report = ReportEntry {
            report_id: report_id.to_string(),
            reporter_id: *participant_id,
            reporter_name: actor_name,
            target_participant_id: *target_participant_id,
            target_name,
            reason: reason.to_string(),
            status: ReportStatus::Open,
            created_at: chrono::Utc::now().to_rfc3339(),
            resolved_at: None,
            outcome: None,
        };
        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let inserted = self.persist_room(room_id, request.room_lock, async {
                let mut transaction = pool.begin().await?;
                sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1,71339))")
                    .bind(room_id)
                    .execute(&mut *transaction)
                    .await?;
                let count: i64 =
                    sqlx::query_scalar("SELECT COUNT(*) FROM room_reports WHERE room_id=$1")
                        .bind(room_id)
                        .fetch_one(&mut *transaction)
                        .await?;
                if count >= MAX_REPORTS as i64 {
                    let removed=sqlx::query("DELETE FROM room_reports WHERE id IN (SELECT id FROM room_reports WHERE room_id=$1 AND status<>'open' ORDER BY created_at,id LIMIT 1)").bind(room_id).execute(&mut *transaction).await?.rows_affected();
                    if removed == 0 {
                        return Ok(false);
                    }
                }
                sqlx::query("INSERT INTO room_reports(id,room_id,reporter_id,reporter_name,target_participant_id,target_name,reason) VALUES($1,$2,$3,$4,$5,$6,$7)")
                    .bind(report_id).bind(room_id).bind(participant_id).bind(&report.reporter_name).bind(target_participant_id).bind(&report.target_name).bind(reason).execute(&mut *transaction).await?;
                transaction.commit().await?;
                Ok(true)
            })
            .await?;
            if !inserted {
                return Err(forbidden(
                    "Room report capacity reached; contact a moderator",
                ));
            }
            request.reacquire().await?;
        } else {
            if room.social.reports.len() >= MAX_REPORTS {
                if let Some(index) = room
                    .social
                    .reports
                    .iter()
                    .position(|r| r.status != ReportStatus::Open)
                {
                    room.social.reports.remove(index);
                } else {
                    return Err(forbidden(
                        "Room report capacity reached; contact a moderator",
                    ));
                }
            }
            room.social.reports.push_back(report.clone());
        }
        request
            .room()
            .social
            .note_report(reporter, std::time::Instant::now());
        // Only the submitting participant receives the report ID here.
        Ok(SocialResponse::ReportParticipant {
            report_id: report.report_id.clone(),
            status: ReportStatus::Open,
        })
    }

    pub(super) async fn list_room_reports(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let room_id = request.room_id;
        let room = request.room();
        let mut reports: Vec<ReportEntry> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let rows: Vec<ReportRow> = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                sqlx::query_as("SELECT r.id,r.reporter_id,r.reporter_name,r.target_participant_id,r.target_name,r.reason,r.status,r.created_at,r.resolved_at,o.action,o.created_at FROM room_reports r LEFT JOIN LATERAL (SELECT e.action,e.created_at FROM moderation_events e WHERE e.report_id=r.id ORDER BY e.created_at DESC,e.id LIMIT 1) o ON true WHERE r.room_id=$1 ORDER BY r.created_at DESC,r.id LIMIT 101 OFFSET $2")
                    .bind(room_id).bind(offset as i64).fetch_all(pool),
            ).await.map_err(|_| timed_out("Report service timed out"))??;
            request.reacquire().await?;
            rows.into_iter()
                .map(
                    |(
                        id,
                        rid,
                        rname,
                        tid,
                        tname,
                        reason,
                        status,
                        created,
                        resolved,
                        outcome_action,
                        outcome_at,
                    )| {
                        ReportEntry {
                            report_id: id.to_string(),
                            reporter_id: rid,
                            reporter_name: rname,
                            target_participant_id: tid,
                            target_name: tname,
                            reason,
                            status,
                            created_at: created.to_rfc3339(),
                            resolved_at: resolved.map(|r| r.to_rfc3339()),
                            outcome: outcome_action.zip(outcome_at).map(|(action, at)| {
                                ReportOutcome {
                                    action,
                                    created_at: at.to_rfc3339(),
                                }
                            }),
                        }
                    },
                )
                .collect()
        } else {
            room.social
                .reports
                .iter()
                .rev()
                .skip(offset)
                .take(PAGE_SIZE + 1)
                .map(|report| ReportEntry {
                    outcome: room.social.report_outcome(&report.report_id),
                    ..report.clone()
                })
                .collect()
        };
        let has_more = reports.len() > PAGE_SIZE;
        reports.truncate(PAGE_SIZE);
        Ok(SocialResponse::ListRoomReports { reports, has_more })
    }

    pub(super) async fn resolve_room_report(
        &self,
        request: &mut SocialRequest<'_>,
        report_id: &str,
        status: moderation::ReportResolution,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let id: Uuid = report_id.parse().map_err(|_| invalid("Invalid report"))?;
        let action = status.action();
        let room_id = request.room_id;
        let participant_id = request.participant_id;
        let actor_name = request.actor_name.clone();
        let room = request.room();
        if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("Report service unavailable"))?;
            request.release();
            let actor_id = *participant_id;
            let resolved = self
                .persist_room(room_id, request.room_lock, async {
                    let mut transaction = pool.begin().await?;
                    let target: Option<(String, String, bool)> = sqlx::query_as("UPDATE room_reports r SET status=$3,resolved_at=now(),resolved_by=$4 WHERE r.room_id=$1 AND r.id=$2 AND r.status='open' RETURNING r.target_participant_id,r.target_name,EXISTS(SELECT 1 FROM users u WHERE u.id::text=r.target_participant_id)")
                        .bind(room_id).bind(id).bind(status).bind(actor_id).fetch_optional(&mut *transaction).await?;
                    let Some((target_id, target_name, target_authenticated)) = target else {
                        return Ok(false);
                    };
                    let event = ModerationEvent::new(
                        action,
                        Actor { id: &actor_id, name: &actor_name },
                        Target { id: ModerationTargetId::from_text(&target_id), name: &target_name, authenticated: target_authenticated, ip: None },
                        None,
                        None,
                        Some(id),
                    );
                    record_event(&mut transaction, room_id, &event, MAX_MODERATION_EVENTS).await?;
                    transaction.commit().await?;
                    Ok(true)
                })
                .await?;
            request.reacquire().await?;
            if !resolved {
                return Err(not_found("Open report not found"));
            }
        } else {
            let report = room
                .social
                .reports
                .iter_mut()
                .find(|r| r.report_id == *report_id && r.status == ReportStatus::Open)
                .ok_or_else(|| not_found("Open report not found"))?;
            report.status = status.status();
            report.resolved_at = Some(chrono::Utc::now().to_rfc3339());
            let target_id = report.target_participant_id;
            let target_name = report.target_name.clone();
            let target_authenticated = room
                .participants
                .get(&target_id)
                .is_some_and(|participant| participant.authenticated);
            room.social.record_moderation_event(ModerationEvent::new(
                action,
                Actor {
                    id: participant_id,
                    name: &actor_name,
                },
                Target {
                    id: target_id.into(),
                    name: &target_name,
                    authenticated: target_authenticated,
                    ip: None,
                },
                None,
                None,
                Some(id),
            ));
        }
        Ok(SocialResponse::ResolveRoomReport {
            report_id: report_id.to_string(),
            status,
        })
    }

    pub(super) async fn list_moderation_events(
        &self,
        request: &mut SocialRequest<'_>,
        offset: Option<u32>,
    ) -> Result<SocialResponse> {
        require_role(request.actor_role, roles::Role::Moderator)?;
        let offset = page_offset(offset)?;
        let owner = request.actor_role >= roles::Role::Owner;
        let room_id = request.room_id;
        let room = request.room();
        let mut events: Vec<ModerationEvent> = if room.persisted {
            let pool = self
                .db_pool
                .as_ref()
                .ok_or_else(|| unavailable("History service unavailable"))?;
            request.release();
            let rows = tokio::time::timeout(
                control::PERSISTENCE_TIMEOUT,
                moderation::list_events(pool, room_id, offset as i64, (PAGE_SIZE + 1) as i64),
            )
            .await
            .map_err(|_| timed_out("History service timed out"))??;
            request.reacquire().await?;
            rows
        } else {
            room.social
                .moderation_events
                .iter()
                .rev()
                .skip(offset)
                .take(PAGE_SIZE + 1)
                .cloned()
                .collect()
        };
        let has_more = events.len() > PAGE_SIZE;
        events.truncate(PAGE_SIZE);
        // Only the owner reads a target's address; moderators never do.
        let events: Vec<ModerationEventView> = events
            .iter()
            .map(|event| ModerationEventView::new(event.clone(), owner))
            .collect();
        Ok(SocialResponse::ListModerationEvents { events, has_more })
    }
}
